use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const ACTION_HISTORY_LIMIT: usize = 32;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedAction {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_of: Option<u64>,
    pub entity: String,
    pub action: String,
    pub value: Option<f64>,
    pub inverse_action: Option<String>,
    pub origin: RequestOrigin,
    pub summary: String,
    pub confidence: Option<f32>,
    pub executed_ms: u64,
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
    ) -> PendingConfirmation {
        let mut state = self.inner.lock().expect("confirmation manager lock");
        prune_expired(&mut state.pending);
        state.next_id += 1;
        let created_ms = now_ms();
        let token = format!("act-{:x}-{:x}", created_ms, state.next_id);
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
        pending
    }

    pub fn confirm(&self, token: &str) -> Option<PendingConfirmation> {
        let mut state = self.inner.lock().expect("confirmation manager lock");
        prune_expired(&mut state.pending);
        state.pending.remove(token)
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
    ) -> RecordedAction {
        self.record_internal(entity, action, value, origin, summary, confidence, None)
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
    ) -> RecordedAction {
        self.record_internal(
            entity,
            action,
            value,
            origin,
            summary,
            confidence,
            Some(original_id),
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
                item.inverse_action.is_some()
                    && item.undo_of.is_none()
                    && !state.undone_action_ids.contains(&item.id)
            })
            .cloned()
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
}

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
            Self::Write(e) => write!(f, "audit log: write event: {e}"),
        }
    }
}

impl std::error::Error for AuditError {}

pub(crate) fn append_json_line<T: serde::Serialize>(
    path: &Path,
    lock: &Arc<Mutex<()>>,
    payload: &T,
) -> Result<(), AuditError> {
    let _guard = lock.lock().expect("audit logger lock");
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(AuditError::CreateDir)?;
        }
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

    pub fn append(&self, event: AuditEvent) -> Result<(), AuditError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        append_json_line(path, &self.lock, &event)
    }

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
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "audit read failed; returning no recent executed actions"
                );
                return Vec::new();
            }
        };
        let mut actions = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_manager_issues_and_confirms() {
        let manager = ConfirmationManager::default();
        let pending = manager.issue(
            "front door",
            "unlock",
            None,
            "needs confirmation",
            RequestOrigin::Voice,
        );
        assert!(pending.token.starts_with("act-"));
        assert_eq!(manager.list().len(), 1);

        let confirmed = manager.confirm(&pending.token).unwrap();
        assert_eq!(confirmed.entity, "front door");
        assert!(manager.list().is_empty());
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
        );
        ledger.record(
            "movie night",
            "activate",
            None,
            RequestOrigin::Dashboard,
            "Scene activated",
            Some(0.99),
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
        );
        assert_eq!(next.id, 12);
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
            })
            .unwrap();
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
            })
            .unwrap();

        let actions = logger.read_recent_executed_actions(10);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].entity, "kitchen light");
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
    fn append_returns_ok_when_logger_is_disabled() {
        let logger = AuditLogger::disabled();
        let result = logger.append(AuditEvent {
            ts_ms: 1,
            status: AuditStatus::Executed,
            origin: RequestOrigin::Api,
            entity: "x".into(),
            action: "y".into(),
            value: None,
            reason: "ok".into(),
            token: None,
            confidence: None,
            action_id: None,
            undo_of: None,
        });
        assert!(result.is_ok());
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
            .append(AuditEvent {
                ts_ms: 42,
                status: AuditStatus::Executed,
                origin: RequestOrigin::Voice,
                entity: "hall light".into(),
                action: "turn_on".into(),
                value: None,
                reason: "done".into(),
                token: None,
                confidence: Some(0.8),
                action_id: Some(7),
                undo_of: None,
            })
            .unwrap();

        let line = std::fs::read_to_string(&path).unwrap();
        let parsed: AuditEvent = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed.entity, "hall light");
        assert_eq!(parsed.action_id, Some(7));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_returns_error_when_parent_path_is_a_file() {
        let blocker = std::env::temp_dir().join(format!(
            "geniepod-actuation-audit-blocker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not-a-directory").unwrap();
        let log_path = blocker.join("nested.jsonl");
        let logger = AuditLogger::new(&log_path);
        let err = logger
            .append(AuditEvent {
                ts_ms: 1,
                status: AuditStatus::Failed,
                origin: RequestOrigin::Api,
                entity: "x".into(),
                action: "y".into(),
                value: None,
                reason: "fail".into(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
            })
            .unwrap_err();
        assert!(matches!(err, AuditError::CreateDir(_)));
        let _ = std::fs::remove_file(&blocker);
    }
}
