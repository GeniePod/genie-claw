use genie_common::jsonl::{self, DEFAULT_MAX_JSONL_LINE_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ha::HomeState;

const ACTION_HISTORY_LIMIT: usize = 32;
const MAX_PENDING_CONFIRMATIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RequestOrigin {
    #[default]
    Unknown,
    Voice,
    Dashboard,
    Api,
    Telegram,
    Repl,
    Confirmation,
}

impl RequestOrigin {
    pub fn from_header(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "voice" => Self::Voice,
            "dashboard" => Self::Dashboard,
            "api" => Self::Api,
            "telegram" => Self::Telegram,
            "repl" => Self::Repl,
            "confirmation" => Self::Confirmation,
            _ => Self::Unknown,
        }
    }

    pub fn as_policy_key(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Voice => "voice",
            Self::Dashboard => "dashboard",
            Self::Api => "api",
            Self::Telegram => "telegram",
            Self::Repl => "repl",
            Self::Confirmation => "confirmation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingConfirmation {
    pub token: String,
    pub entity: String,
    pub action: String,
    pub value: Option<f64>,
    pub reason: String,
    pub requested_by: RequestOrigin,
    pub created_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoRestore {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedAction {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_of: Option<u64>,
    pub entity: String,
    pub action: String,
    pub value: Option<f64>,
    pub inverse_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_restore: Option<UndoRestore>,
    pub origin: RequestOrigin,
    pub summary: String,
    pub confidence: Option<f32>,
    pub executed_ms: u64,
}

impl RecordedAction {
    /// Undo suffix for `action_history` lines — mirrors [`ActionLedger::last_undoable`].
    pub fn action_history_undo_hint(&self) -> String {
        if let Some(inverse) = self.inverse_action.as_deref() {
            format!(" undo: {inverse}")
        } else if let Some(restore) = &self.undo_restore {
            match restore.value {
                Some(value) => format!(" undo: {} {value}", restore.action),
                None => format!(" undo: {}", restore.action),
            }
        } else {
            " not undoable".into()
        }
    }
}

#[derive(Debug, Default)]
pub struct ConfirmationManager {
    inner: Mutex<ConfirmationState>,
}

#[derive(Debug, Default)]
pub struct ActionLedger {
    inner: Mutex<ActionLedgerState>,
}

#[derive(Debug, Default)]
struct ConfirmationState {
    next_id: u64,
    pending: HashMap<String, PendingConfirmation>,
}

#[derive(Debug, Default)]
struct ActionLedgerState {
    next_id: u64,
    actions: VecDeque<RecordedAction>,
    undone_action_ids: HashSet<u64>,
}

impl ConfirmationManager {
    pub fn issue(
        &self,
        entity: &str,
        action: &str,
        value: Option<f64>,
        reason: &str,
        requested_by: RequestOrigin,
    ) -> Option<PendingConfirmation> {
        let mut state = self.inner.lock().expect("confirmation manager lock");
        prune_expired(&mut state.pending);
        if state.pending.len() >= MAX_PENDING_CONFIRMATIONS {
            return None;
        }
        state.next_id += 1;
        let created_ms = now_ms();
        // The token is a bearer secret: its only entropy is 128 CSPRNG bits.
        // `created_ms`/`next_id` are display/bookkeeping fields and MUST NOT
        // feed the token, or it collapses back to the guessable clock+counter
        // scheme this manager replaced.
        let token = random_token();
        let pending = PendingConfirmation {
            token: token.clone(),
            entity: entity.to_string(),
            action: action.to_string(),
            value,
            reason: reason.to_string(),
            requested_by,
            created_ms,
            expires_ms: created_ms + 10 * 60 * 1000,
        };
        state.pending.insert(token, pending.clone());
        Some(pending)
    }

    pub fn confirm(&self, token: &str) -> Option<PendingConfirmation> {
        let mut state = self.inner.lock().expect("confirmation manager lock");
        prune_expired(&mut state.pending);
        // Constant-time match: a plain `HashMap::remove(token)` would compare
        // the supplied token against stored keys with early-exit equality,
        // leaking how many leading bytes matched as a timing signal. Scan
        // every pending token, fold the comparisons with `ct_eq`, and never
        // short-circuit on the first mismatch.
        let mut matched: Option<String> = None;
        for key in state.pending.keys() {
            if ct_eq(key.as_bytes(), token.as_bytes()) {
                matched = Some(key.clone());
            }
        }
        matched.and_then(|key| state.pending.remove(&key))
    }

    pub fn list(&self) -> Vec<PendingConfirmation> {
        let mut state = self.inner.lock().expect("confirmation manager lock");
        prune_expired(&mut state.pending);
        let mut items = state.pending.values().cloned().collect::<Vec<_>>();
        items.sort_by_key(|item| item.created_ms);
        items
    }
}

impl ActionLedger {
    pub fn record(
        &self,
        entity: &str,
        action: &str,
        value: Option<f64>,
        origin: RequestOrigin,
        summary: &str,
        confidence: Option<f32>,
        undo_restore: Option<UndoRestore>,
    ) -> RecordedAction {
        self.record_internal(
            entity,
            action,
            value,
            origin,
            summary,
            confidence,
            None,
            undo_restore,
        )
    }

    pub fn record_undo(
        &self,
        original_id: u64,
        entity: &str,
        action: &str,
        value: Option<f64>,
        origin: RequestOrigin,
        summary: &str,
        confidence: Option<f32>,
        undo_restore: Option<UndoRestore>,
    ) -> RecordedAction {
        self.record_internal(
            entity,
            action,
            value,
            origin,
            summary,
            confidence,
            Some(original_id),
            undo_restore,
        )
    }

    fn record_internal(
        &self,
        entity: &str,
        action: &str,
        value: Option<f64>,
        origin: RequestOrigin,
        summary: &str,
        confidence: Option<f32>,
        undo_of: Option<u64>,
        undo_restore: Option<UndoRestore>,
    ) -> RecordedAction {
        let mut state = self.inner.lock().expect("action ledger lock");
        state.next_id += 1;
        let item = RecordedAction {
            id: state.next_id,
            undo_of,
            entity: entity.to_string(),
            action: action.to_string(),
            value,
            inverse_action: inverse_action(action).map(str::to_string),
            undo_restore,
            origin,
            summary: summary.to_string(),
            confidence,
            executed_ms: now_ms(),
        };
        if let Some(original_id) = undo_of {
            state.undone_action_ids.insert(original_id);
        }
        state.actions.push_back(item.clone());
        while state.actions.len() > ACTION_HISTORY_LIMIT {
            if let Some(removed) = state.actions.pop_front() {
                state.undone_action_ids.remove(&removed.id);
            }
        }
        item
    }

    pub fn list(&self) -> Vec<RecordedAction> {
        let state = self.inner.lock().expect("action ledger lock");
        state.actions.iter().rev().cloned().collect()
    }

    pub fn last_undoable(&self) -> Option<RecordedAction> {
        let state = self.inner.lock().expect("action ledger lock");
        state
            .actions
            .iter()
            .rev()
            .find(|item| {
                item.undo_of.is_none()
                    && !state.undone_action_ids.contains(&item.id)
                    && (item.undo_restore.is_some() || item.inverse_action.is_some())
            })
            .cloned()
    }

    /// Build `home_control` arguments that restore the ledger entry's pre-action state.
    pub fn undo_home_control_args(action: &RecordedAction) -> Option<serde_json::Value> {
        if let Some(restore) = &action.undo_restore {
            let mut args = serde_json::json!({
                "entity": action.entity,
                "action": restore.action,
            });
            if let Some(value) = restore.value {
                args["value"] = serde_json::json!(value);
            }
            return Some(args);
        }
        let inverse = action.inverse_action.as_deref()?;
        Some(serde_json::json!({
            "entity": action.entity,
            "action": inverse,
        }))
    }

    pub fn hydrate(&self, actions: Vec<RecordedAction>) {
        let mut state = self.inner.lock().expect("action ledger lock");
        state.actions.clear();
        state.undone_action_ids.clear();
        state.next_id = 0;

        for action in actions.into_iter().rev().take(ACTION_HISTORY_LIMIT).rev() {
            state.next_id = state.next_id.max(action.id);
            if let Some(original_id) = action.undo_of {
                state.undone_action_ids.insert(original_id);
            }
            state.actions.push_back(action);
        }
    }
}

fn inverse_action(action: &str) -> Option<&'static str> {
    match action {
        "turn_on" => Some("turn_off"),
        "turn_off" => Some("turn_on"),
        "open" => Some("close"),
        "close" => Some("open"),
        "lock" => Some("unlock"),
        "unlock" => Some("lock"),
        _ => None,
    }
}

/// Capture the `home_control` call that restores `prior` before `action` ran.
pub fn undo_restore_from_prior(action: &str, prior: &HomeState) -> Option<UndoRestore> {
    let mut entities = prior.entities.iter();
    let restore = restore_for_entity(action, entities.next()?);
    for entity in entities {
        if restore_for_entity(action, entity) != restore {
            return None;
        }
    }
    restore
}

fn restore_for_entity(action: &str, entity: &crate::ha::Entity) -> Option<UndoRestore> {
    match action {
        "set_brightness" => {
            if entity.state == "off" {
                return Some(UndoRestore {
                    action: "turn_off".into(),
                    value: None,
                });
            }
            let brightness = entity.attributes.get("brightness")?.as_u64()?;
            let percent = (brightness as f64) * 100.0 / 255.0;
            Some(UndoRestore {
                action: "set_brightness".into(),
                value: Some(percent),
            })
        }
        "set_temperature" => {
            if entity.state == "off" {
                return Some(UndoRestore {
                    action: "turn_off".into(),
                    value: None,
                });
            }
            let temp = entity
                .attributes
                .get("temperature")
                .and_then(|v| v.as_f64())
                .or_else(|| {
                    entity
                        .attributes
                        .get("target_temp")
                        .and_then(|v| v.as_f64())
                })?;
            Some(UndoRestore {
                action: "set_temperature".into(),
                value: Some(temp),
            })
        }
        "toggle" => undo_restore_for_power_state(&entity.state),
        _ => None,
    }
}

fn undo_restore_for_power_state(state: &str) -> Option<UndoRestore> {
    match state {
        "on" => Some(UndoRestore {
            action: "turn_on".into(),
            value: None,
        }),
        "off" => Some(UndoRestore {
            action: "turn_off".into(),
            value: None,
        }),
        "open" => Some(UndoRestore {
            action: "open".into(),
            value: None,
        }),
        "closed" => Some(UndoRestore {
            action: "close".into(),
            value: None,
        }),
        "locked" => Some(UndoRestore {
            action: "lock".into(),
            value: None,
        }),
        "unlocked" => Some(UndoRestore {
            action: "unlock".into(),
            value: None,
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    ConfirmationIssued,
    BlockedPolicy,
    BlockedRuntime,
    Executed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub ts_ms: u64,
    pub status: AuditStatus,
    pub origin: RequestOrigin,
    pub entity: String,
    pub action: String,
    pub value: Option<f64>,
    pub reason: String,
    pub token: Option<String>,
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_of: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_restore: Option<UndoRestore>,
}

/// A failure while appending one JSON line to an audit log, tagged by the step
/// that failed. Kept narrow (one variant per IO step) so structured-log filters
/// and future metric labels can tell a disk-full `Write` apart from a
/// permission-denied `Open`.
#[derive(Debug)]
pub enum AuditError {
    CreateDir(std::io::Error),
    Open(std::io::Error),
    Serialize(serde_json::Error),
    Write(std::io::Error),
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateDir(e) => write!(f, "audit log: create parent directory: {e}"),
            Self::Open(e) => write!(f, "audit log: open file for append: {e}"),
            Self::Serialize(e) => write!(f, "audit log: serialize event: {e}"),
            Self::Write(e) => write!(f, "audit log: write line: {e}"),
        }
    }
}

impl std::error::Error for AuditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir(e) | Self::Open(e) | Self::Write(e) => Some(e),
            Self::Serialize(e) => Some(e),
        }
    }
}

/// Append one `\n`-terminated JSON record to a JSONL file, creating the parent
/// directory and the file if needed. Shared by both audit loggers so the IO
/// path and its failure handling live in one tested place. Callers hold their
/// own serialization lock across this call.
pub(crate) fn append_json_line<T: Serialize>(path: &Path, payload: &T) -> Result<(), AuditError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(AuditError::CreateDir)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(AuditError::Open)?;
    let line = serde_json::to_string(payload).map_err(AuditError::Serialize)?;
    writeln!(file, "{line}").map_err(AuditError::Write)?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct AuditLogger {
    path: Option<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl AuditLogger {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// Append an audit event. Returns `Ok(())` for a disabled logger (no path)
    /// and surfaces the failing IO step otherwise, so a security-critical caller
    /// can adopt a "no audit, no action" posture if it chooses.
    pub fn append(&self, event: AuditEvent) -> Result<(), AuditError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let _guard = self.lock.lock().expect("audit logger lock");
        append_json_line(path, &event)
    }

    /// Log-and-continue wrapper: append and, on failure, emit a `tracing::error!`
    /// with the path and underlying error rather than silently dropping the
    /// event. This is the default posture for the existing call sites.
    pub fn append_or_log(&self, event: AuditEvent) {
        if let Err(err) = self.append(event) {
            tracing::error!(
                path = ?self.path,
                error = %err,
                "audit event dropped due to IO failure"
            );
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn read_recent_executed_actions(&self, limit: usize) -> Vec<RecordedAction> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let scan_limit = limit.saturating_mul(16).max(limit);
        let lines = match jsonl::tail_lines(path, scan_limit, DEFAULT_MAX_JSONL_LINE_BYTES) {
            Ok(lines) => lines,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "audit tail read failed; returning no recent executed actions"
                );
                return Vec::new();
            }
        };
        let mut actions = lines
            .into_iter()
            .filter_map(|line| match serde_json::from_str::<AuditEvent>(&line) {
                Ok(event) => audit_event_to_recorded_action(event),
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "audit line parse failed");
                    None
                }
            })
            .collect::<Vec<_>>();
        if actions.len() > limit {
            actions.drain(0..actions.len() - limit);
        }
        actions
    }
}

fn audit_event_to_recorded_action(event: AuditEvent) -> Option<RecordedAction> {
    if event.status != AuditStatus::Executed {
        return None;
    }
    let id = event.action_id?;
    Some(RecordedAction {
        id,
        undo_of: event.undo_of,
        entity: event.entity,
        action: event.action.clone(),
        value: event.value,
        inverse_action: inverse_action(&event.action).map(str::to_string),
        undo_restore: event.undo_restore,
        origin: event.origin,
        summary: event.reason,
        confidence: event.confidence,
        executed_ms: event.ts_ms,
    })
}

fn prune_expired(pending: &mut HashMap<String, PendingConfirmation>) {
    let now = now_ms();
    pending.retain(|_, item| item.expires_ms > now);
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Mint an unguessable confirmation token from 128 bits of CSPRNG entropy,
/// hex-encoded behind the `act-` prefix the rest of the system expects.
///
/// `getrandom` reads the OS CSPRNG (`/dev/urandom`, `getrandom(2)`,
/// `BCryptGenRandom`, …). It cannot return short reads; the only error is the
/// source being unavailable, which on a running host means the entropy pool is
/// broken. Rather than fall back to a weaker, guessable source, we panic — a
/// confirmation token we cannot generate securely must not be issued at all.
fn random_token() -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable while minting confirmation token");
    let mut token = String::with_capacity(4 + bytes.len() * 2);
    token.push_str("act-");
    for byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    token
}

/// Constant-time byte-slice equality. Returns `true` only when both slices have
/// the same length and identical contents, without leaking the position of the
/// first differing byte through timing. Used to compare confirmation tokens so
/// the confirm endpoint is not a partial-match timing oracle.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_manager_issues_and_confirms() {
        let manager = ConfirmationManager::default();
        let pending = manager
            .issue(
                "front door",
                "unlock",
                None,
                "needs confirmation",
                RequestOrigin::Voice,
            )
            .expect("issue should succeed");
        assert!(pending.token.starts_with("act-"));
        assert_eq!(manager.list().len(), 1);

        let confirmed = manager.confirm(&pending.token).unwrap();
        assert_eq!(confirmed.entity, "front door");
        assert!(manager.list().is_empty());
    }

    /// The token is the entire authorization to execute a sensitive physical
    /// action, so it must carry real entropy — not be derivable from the clock
    /// plus a per-process counter. Two tokens issued back-to-back differ in
    /// their random body, and neither equals what the old `act-{ms:x}-{id:x}`
    /// scheme would have produced for the same clock/counter inputs.
    #[test]
    fn confirmation_tokens_are_unpredictable() {
        let manager = ConfirmationManager::default();
        let first = manager
            .issue("front door", "unlock", None, "r", RequestOrigin::Api)
            .expect("issue should succeed");
        let second = manager
            .issue("front door", "unlock", None, "r", RequestOrigin::Api)
            .expect("issue should succeed");

        assert_ne!(
            first.token, second.token,
            "two issued tokens must not collide"
        );

        // 128 bits of entropy => "act-" + 32 lowercase hex chars.
        for token in [&first.token, &second.token] {
            let body = token
                .strip_prefix("act-")
                .expect("token keeps the act- prefix");
            assert_eq!(body.len(), 32, "expected 16 random bytes hex-encoded");
            assert!(
                body.bytes().all(|b| b.is_ascii_hexdigit()),
                "token body must be hex: {token}"
            );
        }

        // The old guessable scheme would have produced these from clock +
        // counter. Knowing both must not let an attacker reconstruct the token.
        let counter_guess_1 = format!("act-{:x}-{:x}", first.created_ms, 1);
        let counter_guess_2 = format!("act-{:x}-{:x}", second.created_ms, 2);
        assert_ne!(first.token, counter_guess_1);
        assert_ne!(second.token, counter_guess_2);
    }

    /// A forged or guessed token — including one built from the now-public
    /// `created_ms` and the low integer counter — must be refused, and must not
    /// consume or match the genuine pending confirmation.
    #[test]
    fn forged_token_is_rejected() {
        let manager = ConfirmationManager::default();
        let pending = manager
            .issue("front door", "unlock", None, "r", RequestOrigin::Api)
            .expect("issue should succeed");

        // Reconstruct what the old clock+counter token would have been.
        let forged_clock_counter = format!("act-{:x}-{:x}", pending.created_ms, 1);
        assert!(
            manager.confirm(&forged_clock_counter).is_none(),
            "clock+counter forgery must not confirm"
        );

        // Same length/shape as a real token but wrong bytes.
        let forged_same_shape = format!("act-{}", "0".repeat(32));
        assert!(
            manager.confirm(&forged_same_shape).is_none(),
            "same-shape forgery must not confirm"
        );

        // A prefix of the real token (timing-oracle probe) must not confirm.
        let prefix = &pending.token[..pending.token.len() - 1];
        assert!(
            manager.confirm(prefix).is_none(),
            "token prefix must not confirm"
        );

        // After all the failed attempts, the genuine token still works exactly
        // once — failed forgeries neither consumed nor unlocked it.
        assert_eq!(manager.list().len(), 1, "forgeries must not evict pending");
        assert!(
            manager.confirm(&pending.token).is_some(),
            "the genuine token must still confirm"
        );
        assert!(manager.confirm(&pending.token).is_none(), "single use only");
    }

    #[test]
    fn issue_returns_none_when_pending_cap_reached() {
        let manager = ConfirmationManager::default();
        for i in 0..MAX_PENDING_CONFIRMATIONS {
            assert!(
                manager
                    .issue(
                        &format!("entity-{i}"),
                        "unlock",
                        None,
                        "cap test",
                        RequestOrigin::Api,
                    )
                    .is_some(),
                "issue {i} should succeed"
            );
        }
        assert!(
            manager
                .issue("overflow", "unlock", None, "cap test", RequestOrigin::Api)
                .is_none(),
            "issue beyond cap must be rejected"
        );
        assert_eq!(manager.list().len(), MAX_PENDING_CONFIRMATIONS);
    }

    #[test]
    fn ct_eq_matches_only_identical_slices() {
        assert!(ct_eq(b"act-abc", b"act-abc"));
        assert!(!ct_eq(b"act-abc", b"act-abd"));
        assert!(!ct_eq(b"act-abc", b"act-ab")); // differing length
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn request_origin_parses_known_values() {
        assert_eq!(
            RequestOrigin::from_header("telegram"),
            RequestOrigin::Telegram
        );
        assert_eq!(
            RequestOrigin::from_header("dashboard"),
            RequestOrigin::Dashboard
        );
        assert_eq!(RequestOrigin::from_header("weird"), RequestOrigin::Unknown);
    }

    #[test]
    fn action_ledger_records_and_finds_undoable_action() {
        let ledger = ActionLedger::default();
        let original = ledger.record(
            "kitchen light",
            "turn_on",
            None,
            RequestOrigin::Voice,
            "Kitchen light is on",
            Some(0.92),
            None,
        );
        ledger.record(
            "movie night",
            "activate",
            None,
            RequestOrigin::Dashboard,
            "Scene activated",
            Some(0.99),
            None,
        );

        let history = ledger.list();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].action, "activate");

        let undo = ledger.last_undoable().unwrap();
        assert_eq!(undo.entity, "kitchen light");
        assert_eq!(undo.inverse_action.as_deref(), Some("turn_off"));

        let undo_action = ledger.record_undo(
            original.id,
            "kitchen light",
            "turn_off",
            None,
            RequestOrigin::Voice,
            "Kitchen light is off",
            Some(0.92),
            None,
        );
        assert_eq!(undo_action.undo_of, Some(original.id));
        assert!(ledger.last_undoable().is_none());
    }

    #[test]
    fn action_ledger_bounds_history() {
        let ledger = ActionLedger::default();
        for idx in 0..40 {
            ledger.record(
                &format!("light {idx}"),
                "turn_on",
                None,
                RequestOrigin::Api,
                "ok",
                None,
                None,
            );
        }

        let history = ledger.list();
        assert_eq!(history.len(), ACTION_HISTORY_LIMIT);
        assert_eq!(history[0].entity, "light 39");
        assert_eq!(history.last().unwrap().entity, "light 8");
    }

    #[test]
    fn action_ledger_hydrates_recent_actions_and_undo_state() {
        let ledger = ActionLedger::default();
        ledger.hydrate(vec![
            RecordedAction {
                id: 10,
                undo_of: None,
                entity: "kitchen light".into(),
                action: "turn_on".into(),
                value: None,
                inverse_action: Some("turn_off".into()),
                undo_restore: None,
                origin: RequestOrigin::Voice,
                summary: "home action executed".into(),
                confidence: Some(0.95),
                executed_ms: 100,
            },
            RecordedAction {
                id: 11,
                undo_of: Some(10),
                entity: "kitchen light".into(),
                action: "turn_off".into(),
                value: None,
                inverse_action: Some("turn_on".into()),
                undo_restore: None,
                origin: RequestOrigin::Voice,
                summary: "home action executed".into(),
                confidence: Some(0.95),
                executed_ms: 200,
            },
        ]);

        assert_eq!(ledger.list().len(), 2);
        assert!(ledger.last_undoable().is_none());
        let next = ledger.record(
            "hall light",
            "turn_on",
            None,
            RequestOrigin::Dashboard,
            "ok",
            None,
            None,
        );
        assert_eq!(next.id, 12);
    }

    fn sample_event(ts_ms: u64) -> AuditEvent {
        AuditEvent {
            ts_ms,
            status: AuditStatus::Executed,
            origin: RequestOrigin::Voice,
            entity: "kitchen light".into(),
            action: "turn_on".into(),
            value: None,
            reason: "home action executed".into(),
            token: None,
            confidence: Some(0.9),
            action_id: Some(1),
            undo_of: None,
            undo_restore: None,
        }
    }

    #[test]
    fn append_returns_ok_when_logger_is_disabled() {
        let logger = AuditLogger::disabled();
        assert!(logger.append(sample_event(1)).is_ok());
    }

    #[test]
    fn append_writes_jsonl_line_with_event_fields() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-write-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let logger = AuditLogger::new(&path);

        logger
            .append(sample_event(42))
            .expect("append should succeed");

        let line = std::fs::read_to_string(&path).unwrap();
        let event: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(event["ts_ms"], 42);
        assert_eq!(event["status"], "executed");
        assert_eq!(event["entity"], "kitchen light");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_surfaces_io_error_when_parent_path_is_a_file() {
        // Put a regular file where the audit log's parent directory should be.
        // create_dir_all then fails deterministically on both Unix and Windows,
        // so the append surfaces an AuditError instead of silently no-op'ing.
        let blocker = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-blocker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("actuation.jsonl");
        let logger = AuditLogger::new(&path);

        let err = logger
            .append(sample_event(1))
            .expect_err("append must fail");
        assert!(matches!(
            err,
            AuditError::CreateDir(_) | AuditError::Open(_)
        ));
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn audit_logger_reads_recent_executed_actions_from_log_tail() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-tail-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let logger = AuditLogger::new(&path);

        for idx in 0..500 {
            logger
                .append(AuditEvent {
                    ts_ms: idx,
                    status: if idx % 2 == 0 {
                        AuditStatus::Executed
                    } else {
                        AuditStatus::ConfirmationIssued
                    },
                    origin: RequestOrigin::Voice,
                    entity: format!("entity-{idx}"),
                    action: "turn_on".into(),
                    value: None,
                    reason: "home action executed".into(),
                    token: None,
                    confidence: None,
                    action_id: if idx % 2 == 0 { Some(idx) } else { None },
                    undo_of: None,
                    undo_restore: None,
                })
                .expect("append audit event");
        }

        let actions = logger.read_recent_executed_actions(3);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].entity, "entity-494");
        assert_eq!(actions[1].entity, "entity-496");
        assert_eq!(actions[2].entity, "entity-498");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_logger_reads_recent_executed_actions() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-test-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let logger = AuditLogger::new(&path);

        logger
            .append(AuditEvent {
                ts_ms: 100,
                status: AuditStatus::Executed,
                origin: RequestOrigin::Voice,
                entity: "kitchen light".into(),
                action: "turn_on".into(),
                value: None,
                reason: "home action executed".into(),
                token: None,
                confidence: Some(0.9),
                action_id: Some(1),
                undo_of: None,
                undo_restore: None,
            })
            .expect("append executed event");
        logger
            .append(AuditEvent {
                ts_ms: 200,
                status: AuditStatus::ConfirmationIssued,
                origin: RequestOrigin::Voice,
                entity: "front door".into(),
                action: "unlock".into(),
                value: None,
                reason: "needs confirmation".into(),
                token: Some("act-test".into()),
                confidence: None,
                action_id: None,
                undo_of: None,
                undo_restore: None,
            })
            .expect("append confirmation event");

        let actions = logger.read_recent_executed_actions(10);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].entity, "kitchen light");
        assert_eq!(actions[0].inverse_action.as_deref(), Some("turn_off"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_logger_hydrates_undo_restore_from_executed_event() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-undo-restore-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let logger = AuditLogger::new(&path);

        logger
            .append(AuditEvent {
                ts_ms: 100,
                status: AuditStatus::Executed,
                origin: RequestOrigin::Dashboard,
                entity: "kitchen light".into(),
                action: "set_brightness".into(),
                value: Some(30.0),
                reason: "home action executed".into(),
                token: None,
                confidence: Some(0.95),
                action_id: Some(7),
                undo_of: None,
                undo_restore: Some(UndoRestore {
                    action: "set_brightness".into(),
                    value: Some(100.0),
                }),
            })
            .expect("append executed event");

        let actions = logger.read_recent_executed_actions(10);
        assert_eq!(actions.len(), 1);
        let restore = actions[0].undo_restore.as_ref().unwrap();
        assert_eq!(restore.action, "set_brightness");
        assert_eq!(restore.value, Some(100.0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_logger_hydrates_legacy_lines_without_undo_restore_field() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-legacy-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let line = serde_json::json!({
            "ts_ms": 100,
            "status": "executed",
            "origin": "dashboard",
            "entity": "kitchen light",
            "action": "turn_on",
            "value": null,
            "reason": "home action executed",
            "token": null,
            "confidence": 0.9,
            "action_id": 1,
            "undo_of": null
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let logger = AuditLogger::new(&path);
        let actions = logger.read_recent_executed_actions(10);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].undo_restore.is_none());
        assert_eq!(actions[0].inverse_action.as_deref(), Some("turn_off"));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn audit_logger_read_returns_empty_when_file_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-unreadable-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"{}\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        let logger = AuditLogger::new(&path);
        let actions = logger.read_recent_executed_actions(10);
        assert!(actions.is_empty());

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn undo_restore_from_prior_captures_brightness_percent() {
        use crate::ha::Entity;

        let prior = HomeState {
            target_name: "kitchen light".into(),
            domain: Some("light".into()),
            area: None,
            entities: vec![Entity {
                entity_id: "light.kitchen".into(),
                state: "on".into(),
                attributes: serde_json::json!({ "brightness": 196 }),
            }],
            available: true,
            spoken_summary: "kitchen light is on".into(),
        };

        let restore = undo_restore_from_prior("set_brightness", &prior).unwrap();
        assert_eq!(restore.action, "set_brightness");
        assert!((restore.value.unwrap() - 76.86).abs() < 0.1);
    }

    #[test]
    fn undo_restore_from_prior_group_mixed_brightness_returns_none() {
        use crate::ha::Entity;

        let prior = HomeState {
            target_name: "living room lights".into(),
            domain: Some("light".into()),
            area: None,
            entities: vec![
                Entity {
                    entity_id: "light.a".into(),
                    state: "off".into(),
                    attributes: serde_json::json!({}),
                },
                Entity {
                    entity_id: "light.b".into(),
                    state: "on".into(),
                    attributes: serde_json::json!({ "brightness": 204 }),
                },
            ],
            available: true,
            spoken_summary: "living room lights".into(),
        };

        assert!(undo_restore_from_prior("set_brightness", &prior).is_none());
    }

    #[test]
    fn undo_restore_from_prior_group_mixed_power_toggle_returns_none() {
        use crate::ha::Entity;

        let prior = HomeState {
            target_name: "living room lights".into(),
            domain: Some("light".into()),
            area: None,
            entities: vec![
                Entity {
                    entity_id: "light.a".into(),
                    state: "on".into(),
                    attributes: serde_json::json!({}),
                },
                Entity {
                    entity_id: "light.b".into(),
                    state: "off".into(),
                    attributes: serde_json::json!({}),
                },
            ],
            available: true,
            spoken_summary: "living room lights".into(),
        };

        assert!(undo_restore_from_prior("toggle", &prior).is_none());
    }

    #[test]
    fn undo_restore_from_prior_group_uniform_brightness_restores() {
        use crate::ha::Entity;

        let prior = HomeState {
            target_name: "living room lights".into(),
            domain: Some("light".into()),
            area: None,
            entities: vec![
                Entity {
                    entity_id: "light.a".into(),
                    state: "on".into(),
                    attributes: serde_json::json!({ "brightness": 204 }),
                },
                Entity {
                    entity_id: "light.b".into(),
                    state: "on".into(),
                    attributes: serde_json::json!({ "brightness": 204 }),
                },
            ],
            available: true,
            spoken_summary: "living room lights".into(),
        };

        let restore = undo_restore_from_prior("set_brightness", &prior).unwrap();
        assert_eq!(restore.action, "set_brightness");
        assert!((restore.value.unwrap() - 80.0).abs() < 0.1);
    }

    #[test]
    fn undo_restore_from_prior_toggle_restores_prior_power_state() {
        use crate::ha::Entity;

        let prior_on = HomeState {
            target_name: "kitchen light".into(),
            domain: Some("light".into()),
            area: None,
            entities: vec![Entity {
                entity_id: "light.kitchen".into(),
                state: "on".into(),
                attributes: serde_json::json!({}),
            }],
            available: true,
            spoken_summary: "kitchen light is on".into(),
        };

        let restore = undo_restore_from_prior("toggle", &prior_on).unwrap();
        assert_eq!(restore.action, "turn_on");
        assert!(restore.value.is_none());

        let prior_off = HomeState {
            entities: vec![Entity {
                entity_id: "light.kitchen".into(),
                state: "off".into(),
                attributes: serde_json::json!({}),
            }],
            ..prior_on.clone()
        };
        let restore = undo_restore_from_prior("toggle", &prior_off).unwrap();
        assert_eq!(restore.action, "turn_off");
    }

    #[test]
    fn undo_restore_from_prior_set_temperature_when_off() {
        use crate::ha::Entity;

        let prior_off = HomeState {
            target_name: "thermostat".into(),
            domain: Some("climate".into()),
            area: None,
            entities: vec![Entity {
                entity_id: "climate.heat".into(),
                state: "off".into(),
                attributes: serde_json::json!({}),
            }],
            available: true,
            spoken_summary: "thermostat is off".into(),
        };

        let restore = undo_restore_from_prior("set_temperature", &prior_off).unwrap();
        assert_eq!(restore.action, "turn_off");
        assert!(restore.value.is_none());

        let prior_on = HomeState {
            entities: vec![Entity {
                entity_id: "climate.heat".into(),
                state: "heat".into(),
                attributes: serde_json::json!({ "temperature": 68.0 }),
            }],
            ..prior_off.clone()
        };
        let restore = undo_restore_from_prior("set_temperature", &prior_on).unwrap();
        assert_eq!(restore.action, "set_temperature");
        assert_eq!(restore.value, Some(68.0));
    }

    #[test]
    fn action_ledger_undo_targets_latest_brightness_change() {
        let ledger = ActionLedger::default();
        ledger.record(
            "kitchen light",
            "turn_on",
            None,
            RequestOrigin::Voice,
            "on",
            None,
            None,
        );
        ledger.record(
            "kitchen light",
            "set_brightness",
            Some(30.0),
            RequestOrigin::Voice,
            "dimmed",
            None,
            Some(UndoRestore {
                action: "set_brightness".into(),
                value: Some(100.0),
            }),
        );

        let undo = ledger.last_undoable().unwrap();
        assert_eq!(undo.action, "set_brightness");
        let args = ActionLedger::undo_home_control_args(&undo).unwrap();
        assert_eq!(args["action"], "set_brightness");
        assert_eq!(args["value"], 100.0);
    }

    #[test]
    fn action_history_undo_hint_uses_restore_when_no_inverse() {
        let action = RecordedAction {
            id: 1,
            undo_of: None,
            entity: "kitchen light".into(),
            action: "set_brightness".into(),
            value: Some(30.0),
            inverse_action: None,
            undo_restore: Some(UndoRestore {
                action: "set_brightness".into(),
                value: Some(100.0),
            }),
            origin: RequestOrigin::Dashboard,
            summary: "dimmed".into(),
            confidence: None,
            executed_ms: 0,
        };
        assert_eq!(
            action.action_history_undo_hint(),
            " undo: set_brightness 100"
        );
        assert!(!action.action_history_undo_hint().contains("not undoable"));
    }

    #[test]
    fn action_history_undo_hint_prefers_inverse_action() {
        let action = RecordedAction {
            id: 1,
            undo_of: None,
            entity: "kitchen light".into(),
            action: "turn_on".into(),
            value: None,
            inverse_action: Some("turn_off".into()),
            undo_restore: None,
            origin: RequestOrigin::Dashboard,
            summary: "on".into(),
            confidence: None,
            executed_ms: 0,
        };
        assert_eq!(action.action_history_undo_hint(), " undo: turn_off");
    }
}
