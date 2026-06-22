use anyhow::Result;
use genie_common::config::{
    ActuationSafetyConfig, ToolPolicyConfig, WebSearchConfig, WebSearchProvider,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::actuation::{
    ActionLedger, AuditError, AuditEvent, AuditLogger, AuditStatus, ConfirmationManager,
    PendingConfirmation, RecordedAction, RequestOrigin, append_json_line, now_ms,
    undo_restore_from_prior,
};
use super::home;
use super::timer;
use crate::ha::{HomeActionKind, HomeAutomationProvider};
use crate::skills::SkillLoader;

const ACTUATION_RATE_WINDOW_MS: u64 = 60_000;

const HOME_CONTROL_ACTIONS: &[&str] = &[
    "turn_on",
    "turn_off",
    "toggle",
    "set_brightness",
    "set_temperature",
    "open",
    "close",
    "lock",
    "unlock",
    "activate",
];

fn parse_home_control_args(args: &serde_json::Value) -> Result<(&str, &str, Option<f64>)> {
    let entity = args
        .get("entity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("home_control requires non-empty string argument 'entity'")
        })?;
    let raw_action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("home_control requires string argument 'action'"))?;
    let action = canon_home_control_action(raw_action).ok_or_else(|| {
        anyhow::anyhow!(
            "home_control action '{}' is invalid; expected one of: {}",
            raw_action,
            HOME_CONTROL_ACTIONS.join(", ")
        )
    })?;
    // `value` stays optional, but a *provided* value must be a number. The old
    // `args.get("value").and_then(|v| v.as_f64())` silently dropped a non-numeric
    // value (e.g. a model emitting `"value": "72"` or `"value": true`) to `None`,
    // so `set_temperature` / `set_brightness` actuated with no value instead of
    // the user's intent. Reject the malformed value at the boundary the same way
    // set_timer rejects a non-integer `seconds`; an absent or null value is still
    // a no-op None.
    let value = match args.get("value") {
        None | Some(serde_json::Value::Null) => None,
        Some(provided) => Some(provided.as_f64().ok_or_else(|| {
            anyhow::anyhow!("home_control 'value' must be a number when provided")
        })?),
    };
    // set_brightness / set_temperature actuate a numeric setpoint, so a call with
    // no `value` is under-specified. The provider used to substitute a hardcoded
    // default (brightness 50 / temperature 20) and report success, actuating a
    // setpoint the user never asked for. Reject the missing value at the boundary
    // so the agent asks for the number instead of guessing — the same boundary
    // #414 uses to reject a *non-numeric* value. (issue #421)
    if value.is_none() && action_requires_value(action) {
        anyhow::bail!("home_control '{action}' requires a numeric argument 'value'");
    }
    Ok((entity, action, value))
}

fn home_action_kind(action: &str) -> Result<HomeActionKind> {
    match action {
        "turn_on" => Ok(HomeActionKind::TurnOn),
        "turn_off" => Ok(HomeActionKind::TurnOff),
        "toggle" => Ok(HomeActionKind::Toggle),
        "set_brightness" => Ok(HomeActionKind::SetBrightness),
        "set_temperature" => Ok(HomeActionKind::SetTemperature),
        "open" => Ok(HomeActionKind::Open),
        "close" => Ok(HomeActionKind::Close),
        "lock" => Ok(HomeActionKind::Lock),
        "unlock" => Ok(HomeActionKind::Unlock),
        "activate" | "activate_scene" => Ok(HomeActionKind::Activate),
        other => anyhow::bail!("unknown home action: {other}"),
    }
}

/// Actions that actuate a numeric setpoint and therefore require a `value`.
/// Every other action (turn_on, turn_off, toggle, open, close, lock, unlock,
/// activate) is a no-op for `value` and leaves it `None`.
fn action_requires_value(action: &str) -> bool {
    matches!(action, "set_brightness" | "set_temperature")
}

/// Canonicalize a model-emitted action verb to one of [`HOME_CONTROL_ACTIONS`].
///
/// Small models routinely emit the natural-language form ("turn off"),
/// hyphenated/cased variants ("Turn-Off"), or a synonym ("deactivate") rather
/// than the exact enum value `turn_off`. Rejecting those means a correct intent
/// silently fails to actuate. Normalize separators + case, map a few
/// unambiguous synonyms, and accept the result only if it lands on a real
/// action. `activate` is left as-is (it is its own action for scenes/scripts).
fn canon_home_control_action(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().to_lowercase().replace([' ', '-'], "_");
    let mapped: &str = match normalized.as_str() {
        "deactivate" | "disable" | "switch_off" | "power_off" | "shut_off" => "turn_off",
        "enable" | "switch_on" | "power_on" => "turn_on",
        other => other,
    };
    HOME_CONTROL_ACTIONS.iter().copied().find(|&a| a == mapped)
}

fn parse_home_status_args(args: &serde_json::Value) -> Result<&str> {
    args.get("entity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("home_status requires non-empty string argument 'entity'"))
}

fn parse_set_timer_args(args: &serde_json::Value) -> Result<(u64, &str)> {
    let seconds = match args.get("seconds") {
        Some(value) => value
            .as_u64()
            .filter(|seconds| *seconds >= 1)
            .ok_or_else(|| {
                if value.as_u64() == Some(0) {
                    anyhow::anyhow!("set_timer seconds must be at least 1")
                } else {
                    anyhow::anyhow!("set_timer requires integer argument 'seconds'")
                }
            })?,
        None => anyhow::bail!("set_timer requires integer argument 'seconds'"),
    };
    let label = args
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("timer");
    Ok((seconds, label))
}

fn parse_memory_recall_query(args: &serde_json::Value) -> Result<String> {
    let raw = args
        .get("query")
        .or_else(|| args.get("topic"))
        .or_else(|| args.get("what"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("memory_recall requires non-empty string argument 'query'")
        })?;
    Ok(normalize_memory_recall_query(raw))
}

fn normalize_memory_recall_query(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("my name") || lower == "name" || lower.contains("who am i") {
        "name".into()
    } else if lower.contains("about me") || lower == "me" || lower == "user" {
        "user".into()
    } else {
        raw.to_string()
    }
}

fn parse_memory_forget_query(args: &serde_json::Value) -> Result<&str> {
    args.get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("memory_forget requires non-empty string argument 'query'"))
}

/// Reject a `memory_store` call with no usable `content` (#416). Delegates to
/// `normalize_memories_to_store` so all valid shapes (aliases, catch-all, `name`)
/// still pass; only missing/empty/wrong-type content is rejected.
fn parse_memory_store_content(args: &serde_json::Value) -> Result<Vec<(String, String)>> {
    let memories = normalize_memories_to_store(args);
    if memories.is_empty() {
        anyhow::bail!("memory_store requires non-empty string argument 'content'");
    }
    Ok(memories)
}

fn parse_calculate_expression(args: &serde_json::Value) -> Result<&str> {
    args.get("expression")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("calculate requires non-empty string argument 'expression'"))
}

fn parse_play_media_query(args: &serde_json::Value) -> Result<&str> {
    args.get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("play_media requires non-empty string argument 'query'"))
}

fn parse_web_search_query(args: &serde_json::Value) -> Result<&str> {
    args.get("query")
        .or_else(|| args.get("q"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("web_search requires non-empty string argument 'query'"))
}

/// `limit` stays optional — an absent or null value defaults to 3 results. But a
/// *provided* value must be a non-negative integer. The old
/// `args.get("limit").and_then(|v| v.as_u64()).unwrap_or(3)` silently coerced a
/// malformed limit (a stringified `"5"`, a float `2.5`, or a negative) to the
/// default 3, quietly returning a different result count than the caller asked
/// for. Reject the malformed value at the boundary the same way home_control
/// rejects a non-numeric `value` (PR #414); a valid integer outside 1..=5 still
/// clamps into range rather than erroring.
fn parse_web_search_limit(args: &serde_json::Value) -> Result<usize> {
    match args.get("limit") {
        None | Some(serde_json::Value::Null) => Ok(3),
        Some(provided) => {
            let limit = provided.as_u64().ok_or_else(|| {
                anyhow::anyhow!("web_search 'limit' must be an integer when provided")
            })?;
            Ok(limit.clamp(1, 5) as usize)
        }
    }
}

fn parse_get_weather_location(args: &serde_json::Value) -> Result<&str> {
    args.get("location")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("get_weather requires non-empty string argument 'location'"))
}

/// `forecast` stays optional — an absent or null value means current weather
/// (the no-op default). But a *provided* value must be a real boolean. The old
/// `args.get("forecast").and_then(|v| v.as_bool()).unwrap_or(false)` silently
/// coerced a stringified `"true"` (a form small models routinely emit) to
/// `false`, returning current weather when the user asked for the 7-day
/// forecast. Reject the malformed value at the boundary the same way
/// home_control rejects a non-numeric `value` (PR #414).
fn parse_get_weather_forecast(args: &serde_json::Value) -> Result<bool> {
    match args.get("forecast") {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(anyhow::anyhow!(
            "get_weather 'forecast' must be a boolean when provided"
        )),
    }
}

/// Tool definition for LLM function calling.
///
/// These are sent to the configured LLM backend as part of the system prompt or
/// via the `tools` parameter when a backend supports OpenAI function-calling.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Result from executing a tool.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub tool: String,
    pub action_class: ToolActionClass,
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActionClass {
    ReadOnly,
    Diagnostic,
    MemoryRead,
    MemoryWrite,
    HomeActuation,
    Media,
    Network,
    Timer,
    Skill,
}

impl ToolActionClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Diagnostic => "diagnostic",
            Self::MemoryRead => "memory_read",
            Self::MemoryWrite => "memory_write",
            Self::HomeActuation => "home_actuation",
            Self::Media => "media",
            Self::Network => "network",
            Self::Timer => "timer",
            Self::Skill => "skill",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ToolExecutionContext {
    pub memory_read_context: Option<crate::memory::policy::MemoryReadContext>,
    pub request_origin: RequestOrigin,
    pub confirmed: bool,
}

/// LLM-generated tool call (parsed from model output).
/// Accepts both `{"tool": "..."}` and `{"name": "..."}` formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(alias = "tool")]
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Central tool dispatcher. Compiled-in tools, no plugin execution.
pub struct ToolDispatcher {
    ha: Option<Arc<dyn HomeAutomationProvider>>,
    memory: Option<Arc<std::sync::Mutex<crate::memory::Memory>>>,
    skills: Option<Arc<std::sync::Mutex<SkillLoader>>>,
    web_search: WebSearchConfig,
    tool_policy: ToolPolicyConfig,
    actuation_safety: ActuationSafetyConfig,
    confirmations: Arc<ConfirmationManager>,
    action_ledger: Arc<ActionLedger>,
    actuation_rate_limiter: Arc<ActuationRateLimiter>,
    tool_rate_limiter: Arc<ToolRateLimiter>,
    tool_confirmations: Arc<ToolConfirmationGate>,
    audit_logger: AuditLogger,
    tool_audit_logger: ToolAuditLogger,
    pub(crate) timers: timer::TimerManager,
}

#[derive(Debug, Default)]
struct ActuationRateLimiter {
    attempts: Mutex<HashMap<RequestOrigin, VecDeque<u64>>>,
}

/// Per-tool sliding-window rate limiter for the dispatcher gate. Unlike
/// [`ActuationRateLimiter`] (which buckets physical home actions by origin),
/// this bounds *any* tool by name via `tool_policy.max_actions_per_minute_by_tool`
/// so a fast loop (voice, skill, or LLM) bounces off the limit after N calls.
#[derive(Debug, Default)]
struct ToolRateLimiter {
    attempts: Mutex<HashMap<String, VecDeque<u64>>>,
}

/// Two-step confirmation gate for sensitive tools (issue #22).
///
/// A tool listed in `tool_policy.requires_confirmation_tools` must be requested
/// twice with the same `(origin, tool, arguments)` within a TTL window: the
/// first leg (`confirmed = false`) records the request and returns a stable
/// token asking the caller to repeat it; the confirming leg (`confirmed = true`)
/// only executes when a matching first leg is still inside the window, otherwise
/// it reports the confirmation as expired.
#[derive(Debug, Default)]
struct ToolConfirmationGate {
    /// Map of `(origin, tool, args)` key -> first-seen epoch millis.
    pending: Mutex<HashMap<String, u64>>,
}

/// Pending first legs are retained for an hour so a late confirming leg reports
/// "expired" rather than silently restarting confirmation, while still bounding
/// memory if a first leg is never followed up.
const TOOL_CONFIRMATION_RETENTION_MS: u64 = 60 * 60 * 1000;

/// Hard cap on tracked first legs; the oldest is evicted past this so a flood of
/// distinct sensitive requests cannot grow the map without bound.
const MAX_TOOL_CONFIRMATIONS: usize = 256;

enum ToolConfirmDecision {
    /// First leg recorded; caller must repeat the same request to proceed.
    Pending { token: String },
    /// A matching first leg is still inside the TTL window — proceed.
    Confirmed,
    /// The confirming leg arrived with no live first leg (never requested, or
    /// the TTL window elapsed).
    Expired,
}

/// How the gate resolved a tool call, recorded on every tool-audit line so the
/// evidence trail distinguishes an execution from each refusal class.
#[derive(Debug, Clone, Copy)]
enum GateDecision {
    Executed,
    Error,
    DeniedPolicy,
    RateLimited,
    PendingConfirmation,
    ConfirmationExpired,
}

impl GateDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Error => "error",
            Self::DeniedPolicy => "denied_policy",
            Self::RateLimited => "rate_limited",
            Self::PendingConfirmation => "pending_confirmation",
            Self::ConfirmationExpired => "confirmation_expired",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ToolAuditEvent {
    ts_ms: u64,
    tool: String,
    action_class: ToolActionClass,
    origin: RequestOrigin,
    success: bool,
    /// Which gate branch produced this line: `executed`, `error`,
    /// `denied_policy`, `rate_limited`, `pending_confirmation`, or
    /// `confirmation_expired`.
    decision: &'static str,
    duration_ms: u64,
    argument_keys: Vec<String>,
    output_chars: usize,
}

#[derive(Debug, Clone, Default)]
struct ToolAuditLogger {
    path: Option<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl ToolAuditLogger {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn append(&self, event: ToolAuditEvent) -> Result<(), AuditError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let _guard = self.lock.lock().expect("tool audit logger lock");
        append_json_line(path, &event)
    }

    fn append_or_log(&self, event: ToolAuditEvent) {
        if let Err(err) = self.append(event) {
            tracing::error!(
                path = ?self.path,
                error = %err,
                "tool audit event dropped due to IO failure"
            );
        }
    }

    fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
}

impl ToolDispatcher {
    pub fn new(ha: Option<Arc<dyn HomeAutomationProvider>>) -> Self {
        Self {
            ha,
            memory: None,
            skills: None,
            web_search: WebSearchConfig::default(),
            tool_policy: ToolPolicyConfig::default(),
            actuation_safety: ActuationSafetyConfig::default(),
            confirmations: Arc::new(ConfirmationManager::default()),
            action_ledger: Arc::new(ActionLedger::default()),
            actuation_rate_limiter: Arc::new(ActuationRateLimiter::default()),
            tool_rate_limiter: Arc::new(ToolRateLimiter::default()),
            tool_confirmations: Arc::new(ToolConfirmationGate::default()),
            audit_logger: AuditLogger::disabled(),
            tool_audit_logger: ToolAuditLogger::default(),
            timers: timer::TimerManager::new(),
        }
    }

    pub fn has_home_automation(&self) -> bool {
        self.ha.is_some()
    }

    pub fn has_web_search(&self) -> bool {
        self.web_search.enabled
    }

    pub fn web_search_status(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.web_search.enabled,
            "provider": match self.web_search.provider {
                WebSearchProvider::Duckduckgo => "duckduckgo",
                WebSearchProvider::Searxng => "searxng",
            },
            "base_url_configured": !self.web_search.base_url.trim().is_empty()
                || std::env::var("GENIEPOD_WEB_SEARCH_BASE_URL")
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false),
            "allow_remote_base_url": self.web_search.allow_remote_base_url,
            "timeout_secs": self.web_search.timeout_secs,
            "max_results": self.web_search.max_results,
            "cache_enabled": self.web_search.cache_enabled,
            "cache_ttl_secs": self.web_search.cache_ttl_secs,
            "cache_max_entries": self.web_search.cache_max_entries,
            "cache_entries": super::web_search::cache_size(),
        })
    }

    pub fn runtime_policy_status(&self) -> serde_json::Value {
        let (loaded_skills, skill_manifests, skill_policy) = self
            .skills
            .as_ref()
            .and_then(|skills| {
                skills.lock().ok().map(|loader| {
                    let loaded = loader
                        .loaded()
                        .iter()
                        .map(|skill| {
                            serde_json::json!({
                                "name": &skill.name,
                                "version": &skill.version,
                                "path": skill.path.display().to_string(),
                                "manifest": &skill.manifest,
                            })
                        })
                        .collect::<Vec<_>>();
                    (
                        loader.loaded().len(),
                        loaded,
                        serde_json::json!(loader.policy()),
                    )
                })
            })
            .unwrap_or_else(|| (0, Vec::new(), serde_json::Value::Null));

        serde_json::json!({
            "home_automation": {
                "available": self.has_home_automation(),
            },
            "tool_policy": {
                "enabled": self.tool_policy.enabled,
                "allowed_tools_by_origin": &self.tool_policy.allowed_tools_by_origin,
                "denied_tools_by_origin": &self.tool_policy.denied_tools_by_origin,
                "max_actions_per_minute_by_tool": &self.tool_policy.max_actions_per_minute_by_tool,
                "requires_confirmation_tools": &self.tool_policy.requires_confirmation_tools,
                "confirmation_ttl_secs": self.tool_policy.confirmation_ttl_secs,
            },
            "actuation_safety": {
                "enabled": self.actuation_safety.enabled,
                "min_target_confidence": self.actuation_safety.min_target_confidence,
                "min_sensitive_confidence": self.actuation_safety.min_sensitive_confidence,
                "deny_multi_target_sensitive": self.actuation_safety.deny_multi_target_sensitive,
                "require_available_state": self.actuation_safety.require_available_state,
                "allowed_origins": &self.actuation_safety.allowed_origins,
                "max_actions_per_minute": self.actuation_safety.max_actions_per_minute,
                "max_actions_per_minute_by_origin": &self.actuation_safety.max_actions_per_minute_by_origin,
                "audit_enabled": self.actuation_audit_path().is_some(),
            },
            "web_search": {
                "enabled": self.web_search.enabled,
                "provider": match self.web_search.provider {
                    WebSearchProvider::Duckduckgo => "duckduckgo",
                    WebSearchProvider::Searxng => "searxng",
                },
                "base_url_configured": !self.web_search.base_url.trim().is_empty()
                    || std::env::var("GENIEPOD_WEB_SEARCH_BASE_URL")
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false),
                "allow_remote_base_url": self.web_search.allow_remote_base_url,
                "timeout_secs": self.web_search.timeout_secs,
                "max_results": self.web_search.max_results,
                "cache_enabled": self.web_search.cache_enabled,
                "cache_ttl_secs": self.web_search.cache_ttl_secs,
                "cache_max_entries": self.web_search.cache_max_entries,
            },
            "memory_read_default": "shared_room_voice",
            "tool_audit": {
                "enabled": self.tool_audit_logger.path().is_some(),
                "path": self.tool_audit_logger.path().map(|path| path.display().to_string()),
            },
            "skills": {
                "loader_attached": self.skills.is_some(),
                "loaded_count": loaded_skills,
                "policy": skill_policy,
                "loaded": skill_manifests,
            },
        })
    }

    pub(crate) async fn web_search_response(
        &self,
        query: &str,
        limit: usize,
        fresh: bool,
    ) -> Result<super::web_search::SearchResponse> {
        super::web_search::search_response_with_options(query, limit, &self.web_search, fresh).await
    }

    /// Set public web search provider configuration.
    pub fn with_web_search_config(mut self, config: WebSearchConfig) -> Self {
        self.web_search = config;
        self
    }

    pub fn with_tool_policy_config(mut self, config: ToolPolicyConfig) -> Self {
        self.tool_policy = config;
        self
    }

    pub fn with_actuation_safety_config(mut self, config: ActuationSafetyConfig) -> Self {
        self.actuation_safety = config;
        self
    }

    pub fn with_actuation_audit_path(mut self, path: PathBuf) -> Self {
        self.audit_logger = AuditLogger::new(path);
        let recent = self.audit_logger.read_recent_executed_actions(32);
        self.action_ledger.hydrate(recent);
        self
    }

    pub fn with_tool_audit_path(mut self, path: PathBuf) -> Self {
        self.tool_audit_logger = ToolAuditLogger::new(path);
        self
    }

    /// Set the memory store for memory tools (recall, forget, store).
    pub fn with_memory(mut self, memory: Arc<std::sync::Mutex<crate::memory::Memory>>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Set the dynamic skill loader for loadable skill modules.
    pub fn with_skill_loader(mut self, skill_loader: SkillLoader) -> Self {
        self.skills = Some(Arc::new(std::sync::Mutex::new(skill_loader)));
        self
    }

    pub fn pending_confirmations(&self) -> Vec<PendingConfirmation> {
        self.confirmations.list()
    }

    pub fn recent_home_actions(&self) -> Vec<RecordedAction> {
        self.action_ledger.list()
    }

    pub fn actuation_audit_path(&self) -> Option<&std::path::Path> {
        self.audit_logger.path()
    }

    /// All available tool definitions (for the LLM system prompt).
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        let mut defs = Vec::new();

        if self.has_home_automation() {
            defs.push(ToolDef {
                name: "home_control".into(),
                description: "Control Home Assistant devices, scenes, and voice-safe routines. Use for lights, switches, climate, safe covers, and scene activation. Risky actions like locks, garage doors, cameras, and alarms require local confirmation and may be blocked.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": {"type": "string", "description": "Household-facing target such as 'living room lights', 'thermostat', 'front door lock', or 'movie night'"},
                        "action": {"type": "string", "enum": ["turn_on", "turn_off", "toggle", "set_brightness", "set_temperature", "open", "close", "lock", "unlock", "activate"]},
                        "value": {"type": "number", "description": "Optional value. Brightness may be 0-100 percent or 0-255. Temperature is in degrees."}
                    },
                    "required": ["entity", "action"]
                }),
            });
            defs.push(ToolDef {
                name: "home_status".into(),
                description: "Get the current status of a smart home device, room lights, thermostat, lock, cover, scene, or other Home Assistant target.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": {"type": "string", "description": "Household-facing target to query, such as 'living room lights' or 'front door lock'"}
                    },
                    "required": ["entity"]
                }),
            });
            defs.push(ToolDef {
                name: "home_undo".into(),
                description: "Undo the most recent reversible home action. Use when the user says undo, put it back, revert that, or asks you to reverse the last device action. Still goes through runtime safety and may require confirmation.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            });
            defs.push(ToolDef {
                name: "action_history".into(),
                description: "Report recent physical home actions and pending confirmations. Use when the user asks what you did, what changed, recent actions, or pending confirmations.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            });
        }

        defs.extend([
            ToolDef {
                name: "set_timer".into(),
                description: "Set a countdown timer. Use for 'set a timer for 10 minutes', 'remind me in 5 minutes'.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "seconds": {"type": "integer", "description": "Duration in seconds"},
                        "label": {"type": "string", "description": "What the timer is for"}
                    },
                    "required": ["seconds"]
                }),
            },
            ToolDef {
                name: "get_time".into(),
                description: "Get the current date and time.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
        ]);

        defs.push(ToolDef {
            name: "get_weather".into(),
            description: "Get current weather or forecast for a location. Use for any weather question.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string", "description": "City name (e.g., 'Denver', 'Tokyo', 'London')"},
                    "forecast": {"type": "boolean", "description": "true for 7-day forecast, false for current weather"}
                },
                "required": ["location"]
            }),
        });

        if self.has_web_search() {
            defs.push(ToolDef {
                name: "web_search".into(),
                description: "Search the public web using a free no-key provider. Use for current or recent public facts, online lookup requests, and explicit web search requests. Do not use for private memory, local system status, or Home Assistant state.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 5, "description": "Maximum number of results to return"},
                        "fresh": {"type": "boolean", "description": "Bypass cached results and fetch fresh results"}
                    },
                    "required": ["query"]
                }),
            });
        }

        defs.push(ToolDef {
            name: "system_info".into(),
            description:
                "Get GeniePod system status: Home Assistant connection state, memory, uptime, governor mode, and load average."
                    .into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        });

        defs.push(ToolDef {
            name: "calculate".into(),
            description: "Evaluate a math expression. Supports +, -, *, /, parentheses, decimals.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": {"type": "string", "description": "Math expression (e.g., '(100 - 32) * 5 / 9')"}
                },
                "required": ["expression"]
            }),
        });

        defs.push(ToolDef {
            name: "play_media".into(),
            description: "Play media on the TV/HDMI output. Triggers media mode (unloads LLM, launches mpv).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to play (movie title, music, etc.)"}
                },
                "required": ["query"]
            }),
        });

        defs.push(ToolDef {
            name: "memory_recall".into(),
            description: "Recall what you know about a topic. Use when the user asks 'what do you know about me', 'do you remember my name', etc.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Topic to search memories for (e.g., 'name', 'age', 'preferences')"}
                },
                "required": ["query"]
            }),
        });

        defs.push(ToolDef {
            name: "memory_status".into(),
            description: "Check memory database health, row counts, FTS consistency, and promoted memory count. Use for memory system diagnostics, not for recalling personal facts.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        });

        defs.push(ToolDef {
            name: "memory_forget".into(),
            description: "Forget a specific piece of information. Use ONLY when the user explicitly asks to forget something, like 'forget my age' or 'delete what you know about X'.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to forget (e.g., 'age', 'name', 'favorite color')"}
                },
                "required": ["query"]
            }),
        });

        defs.push(ToolDef {
            name: "memory_store".into(),
            description: "Explicitly store a safe household fact or preference. Use when the user says 'remember that...' or asks you to save something. Do not store passwords, one-time codes, payment details, keys, tokens, household access codes, lock combinations, sensitive document/key locations, or private secrets.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "The fact to remember"},
                    "category": {"type": "string", "enum": ["identity", "preference", "relationship", "fact", "context"], "description": "Category of the memory"}
                },
                "required": ["content"]
            }),
        });

        if let Some(skill_defs) = self.skill_tool_defs() {
            defs.extend(skill_defs);
        }

        defs
    }

    /// Execute a tool call from the LLM.
    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        self.execute_with_context(call, ToolExecutionContext::default())
            .await
    }

    pub async fn execute_with_context(
        &self,
        call: &ToolCall,
        exec_ctx: ToolExecutionContext,
    ) -> ToolResult {
        let started = Instant::now();
        let action_class = tool_action_class(&call.name);

        // Single chokepoint: every tool call passes the gate (per-origin ACLs,
        // two-step confirmation for sensitive tools, per-tool rate limits)
        // before any tool body runs. Refusals are already audited.
        if let Some(rejected) = self.run_gate(call, exec_ctx, started) {
            return rejected;
        }

        let result = match call.name.as_str() {
            "home_control" => self.exec_home_control(&call.arguments, exec_ctx).await,
            "home_status" => self.exec_home_status(&call.arguments).await,
            "home_undo" => self.exec_home_undo(exec_ctx).await,
            "action_history" => Ok(self.exec_action_history()),
            "set_timer" => self.exec_set_timer(&call.arguments),
            "get_time" => Ok(get_current_time()),
            "get_weather" => exec_weather(&call.arguments).await,
            "web_search" => exec_web_search(&call.arguments, &self.web_search).await,
            "system_info" => super::system::system_info(self.ha.as_deref()).await,
            "calculate" => exec_calculate(&call.arguments),
            "play_media" => self.exec_play_media(&call.arguments).await,
            "memory_recall" => self.exec_memory_recall(&call.arguments, exec_ctx),
            "memory_status" => self.exec_memory_status(),
            "memory_forget" => self.exec_memory_forget(&call.arguments, exec_ctx),
            "memory_store" => self.exec_memory_store(&call.arguments),
            other => self.exec_skill(other, &call.arguments).await,
        };

        let tool_result = match result {
            Ok(output) => ToolResult {
                tool: call.name.clone(),
                action_class,
                success: true,
                output,
            },
            Err(e) => ToolResult {
                tool: call.name.clone(),
                action_class,
                success: false,
                output: e.to_string(),
            },
        };

        let decision = if tool_result.success {
            GateDecision::Executed
        } else {
            GateDecision::Error
        };
        self.audit_gate_decision(call, exec_ctx, started, &tool_result, decision);

        tool_result
    }

    /// Run the tool-call gate without dispatching: per-origin ACLs, two-step
    /// confirmation for sensitive tools, then per-tool rate limits. Returns
    /// `Some(rejection)` (already written to the tool-audit trail) when the gate
    /// refuses, or `None` when the call may proceed. The caller audits the
    /// eventual outcome of an allowed call.
    fn run_gate(
        &self,
        call: &ToolCall,
        exec_ctx: ToolExecutionContext,
        started: Instant,
    ) -> Option<ToolResult> {
        let action_class = tool_action_class(&call.name);

        // 1. Per-origin allow/deny ACLs (wildcards supported; deny wins).
        if let Err(err) =
            tool_origin_allowed(&self.tool_policy, exec_ctx.request_origin, &call.name)
        {
            let result = ToolResult {
                tool: call.name.clone(),
                action_class,
                success: false,
                output: format!("Tool blocked by origin policy: {err}"),
            };
            self.audit_gate_decision(call, exec_ctx, started, &result, GateDecision::DeniedPolicy);
            return Some(result);
        }

        // 2. Two-step confirmation for configured sensitive tools. Skipped for
        //    pre-confirmed re-entries of tools NOT in the list (e.g. the home
        //    actuation confirm flow, which carries its own confirmation deeper).
        if tool_requires_confirmation(&self.tool_policy, &call.name) {
            let ttl_ms = self.tool_policy.confirmation_ttl_secs.saturating_mul(1000);
            match self.tool_confirmations.evaluate(
                exec_ctx.request_origin,
                &call.name,
                &call.arguments,
                ttl_ms,
                exec_ctx.confirmed,
            ) {
                ToolConfirmDecision::Pending { token } => {
                    let result = ToolResult {
                        tool: call.name.clone(),
                        action_class,
                        success: true,
                        output: format!(
                            "Confirmation required before I run '{}'. Re-issue the same request within {}s to proceed (confirmation token {}).",
                            call.name, self.tool_policy.confirmation_ttl_secs, token
                        ),
                    };
                    self.audit_gate_decision(
                        call,
                        exec_ctx,
                        started,
                        &result,
                        GateDecision::PendingConfirmation,
                    );
                    return Some(result);
                }
                ToolConfirmDecision::Expired => {
                    let result = ToolResult {
                        tool: call.name.clone(),
                        action_class,
                        success: false,
                        output: format!(
                            "Confirmation for '{}' expired or was never requested; the {}s window elapsed. Request it again to restart confirmation.",
                            call.name, self.tool_policy.confirmation_ttl_secs
                        ),
                    };
                    self.audit_gate_decision(
                        call,
                        exec_ctx,
                        started,
                        &result,
                        GateDecision::ConfirmationExpired,
                    );
                    return Some(result);
                }
                ToolConfirmDecision::Confirmed => {}
            }
        }

        // 3. Per-tool sliding-window rate limit. Pre-confirmed re-entries skip
        //    the recharge: the slot was already paid by the first leg.
        if !exec_ctx.confirmed
            && let Err(err) = self
                .tool_rate_limiter
                .check_and_record(&self.tool_policy, &call.name)
        {
            let result = ToolResult {
                tool: call.name.clone(),
                action_class,
                success: false,
                output: format!("Tool blocked by rate limit: {err}"),
            };
            self.audit_gate_decision(call, exec_ctx, started, &result, GateDecision::RateLimited);
            return Some(result);
        }

        None
    }

    /// Public chokepoint entry for specialized fast-paths (e.g. the voice
    /// `web_search` renderer) that need the gate's ACL / confirmation /
    /// rate-limit decision and audit trail but render their own output.
    ///
    /// Returns `Some(rejection)` (already audited) when the gate refuses, or
    /// `None` when the call may proceed — in which case the caller MUST record
    /// the eventual outcome with [`ToolDispatcher::audit_gated_tool`] so the
    /// single chokepoint still produces exactly one audit line per call.
    pub fn gate_tool_call(
        &self,
        call: &ToolCall,
        exec_ctx: ToolExecutionContext,
    ) -> Option<ToolResult> {
        self.run_gate(call, exec_ctx, Instant::now())
    }

    /// Record one tool-audit line for a call that passed [`gate_tool_call`] and
    /// was executed by a specialized fast-path.
    pub fn audit_gated_tool(
        &self,
        call: &ToolCall,
        exec_ctx: ToolExecutionContext,
        started: Instant,
        success: bool,
        output: &str,
    ) {
        let result = ToolResult {
            tool: call.name.clone(),
            action_class: tool_action_class(&call.name),
            success,
            output: output.to_string(),
        };
        let decision = if success {
            GateDecision::Executed
        } else {
            GateDecision::Error
        };
        self.audit_gate_decision(call, exec_ctx, started, &result, decision);
    }

    fn audit_gate_decision(
        &self,
        call: &ToolCall,
        exec_ctx: ToolExecutionContext,
        started: Instant,
        result: &ToolResult,
        decision: GateDecision,
    ) {
        self.tool_audit_logger.append_or_log(ToolAuditEvent {
            ts_ms: now_ms(),
            tool: call.name.clone(),
            action_class: result.action_class,
            origin: exec_ctx.request_origin,
            success: result.success,
            decision: decision.as_str(),
            duration_ms: started.elapsed().as_millis() as u64,
            argument_keys: tool_argument_keys(&call.arguments),
            output_chars: result.output.chars().count(),
        });
    }

    async fn exec_home_control(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        self.exec_home_control_inner(args, exec_ctx, None).await
    }

    async fn exec_home_control_inner(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
        undo_of: Option<u64>,
    ) -> Result<String> {
        let ha = self
            .ha
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Home Assistant not connected"))?;
        let (entity_name, action, value) = parse_home_control_args(args)?;
        let resolved_entity = self.resolve_device_alias(entity_name);
        if !actuation_origin_allowed(&self.actuation_safety, exec_ctx.request_origin) {
            let reason = format!(
                "actuation from '{}' is not allowed by channel policy",
                exec_ctx.request_origin.as_policy_key()
            );
            self.audit_logger.append_or_log(AuditEvent {
                ts_ms: now_ms(),
                status: AuditStatus::BlockedPolicy,
                origin: exec_ctx.request_origin,
                entity: resolved_entity.clone(),
                action: action.to_string(),
                value,
                reason: reason.clone(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
            });
            anyhow::bail!("Home action blocked by channel policy: {}", reason);
        }
        // Skip the rate-limit recharge for pre-confirmed actions. The
        // origin's bucket already paid one slot on the original request that
        // returned `ConfirmationRequired`; counting the confirmation re-entry
        // as a second hit would double-charge the same logical action.
        if !exec_ctx.confirmed
            && let Err(err) = self
                .actuation_rate_limiter
                .check_and_record(&self.actuation_safety, exec_ctx.request_origin)
        {
            let reason = err.to_string();
            self.audit_logger.append_or_log(AuditEvent {
                ts_ms: now_ms(),
                status: AuditStatus::BlockedRuntime,
                origin: exec_ctx.request_origin,
                entity: resolved_entity.clone(),
                action: action.to_string(),
                value,
                reason: reason.clone(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
            });
            anyhow::bail!("Home action blocked by rate limit: {}", reason);
        }
        let undo_restore = if undo_of.is_some() {
            None
        } else if matches!(action, "set_brightness" | "set_temperature" | "toggle") {
            match home_action_kind(action) {
                Ok(kind) => match ha.resolve_target(&resolved_entity, Some(kind)).await {
                    Ok(target) => ha
                        .get_state(&target)
                        .await
                        .ok()
                        .and_then(|prior| undo_restore_from_prior(action, &prior)),
                    Err(_) => None,
                },
                Err(_) => None,
            }
        } else {
            None
        };
        match home::control(
            ha.as_ref(),
            &resolved_entity,
            action,
            value,
            &self.actuation_safety,
            exec_ctx.request_origin,
            exec_ctx.confirmed,
        )
        .await
        {
            Ok(home::ControlOutcome::Executed(output, confidence)) => {
                let recorded = if let Some(original_id) = undo_of {
                    self.action_ledger.record_undo(
                        original_id,
                        &resolved_entity,
                        action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                        None,
                    )
                } else {
                    self.action_ledger.record(
                        &resolved_entity,
                        action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                        undo_restore,
                    )
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::Executed,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity.clone(),
                    action: action.to_string(),
                    value,
                    reason: "home action executed".into(),
                    token: None,
                    confidence,
                    action_id: Some(recorded.id),
                    undo_of: recorded.undo_of,
                });
                Ok(output)
            }
            Ok(home::ControlOutcome::ConfirmationRequired { reason, .. }) => {
                let Some(pending) = self.confirmations.issue(
                    &resolved_entity,
                    action,
                    value,
                    &reason,
                    exec_ctx.request_origin,
                ) else {
                    return Ok(
                        "Too many pending home confirmations; confirm or wait for existing ones to expire before requesting another.".into(),
                    );
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::ConfirmationIssued,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity.clone(),
                    action: action.to_string(),
                    value,
                    reason: reason.clone(),
                    token: Some(pending.token.clone()),
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                });
                // The token is a bearer secret: a leaked one is a reusable
                // door-unlock credential for its full validity window. Keep it
                // out of LLM tool output (transcripts, voice, logs). The
                // dashboard fetches it over /api/actuation/pending to drive the
                // Confirm button; humans confirm there rather than reading the
                // token back from this string.
                Ok(format!(
                    "Confirmation required before I can do that: {}. Confirm this pending action from the local dashboard (or POST /api/actuation/confirm with its token from /api/actuation/pending).",
                    reason
                ))
            }
            Err(err) => {
                let error = err.to_string();
                let status = if error.contains("local policy") {
                    AuditStatus::BlockedPolicy
                } else if error.contains("runtime safety") {
                    AuditStatus::BlockedRuntime
                } else {
                    AuditStatus::Failed
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity,
                    action: action.to_string(),
                    value,
                    reason: error.clone(),
                    token: None,
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                });
                Err(anyhow::anyhow!(error))
            }
        }
    }

    async fn exec_home_undo(&self, exec_ctx: ToolExecutionContext) -> Result<String> {
        let action = self
            .action_ledger
            .last_undoable()
            .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?;
        let args = ActionLedger::undo_home_control_args(&action)
            .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?;
        let output = self
            .exec_home_control_inner(&args, exec_ctx, Some(action.id))
            .await?;
        if output.starts_with("Confirmation required") {
            Ok(output)
        } else {
            Ok(format!("Undid the last home action. {}", output))
        }
    }

    fn exec_action_history(&self) -> String {
        let pending = self.pending_confirmations();
        let actions = self.recent_home_actions();
        if actions.is_empty() && pending.is_empty() {
            return "No recent home actions or pending confirmations.".into();
        }

        let mut lines = Vec::new();
        if !actions.is_empty() {
            lines.push("Recent home actions:".to_string());
            for action in actions.iter().take(5) {
                let undo = action
                    .inverse_action
                    .as_deref()
                    .map(|inverse| format!(" undo: {inverse}"))
                    .unwrap_or_else(|| " not undoable".into());
                lines.push(format!(
                    "- {} {} via {:?};{}",
                    action.action, action.entity, action.origin, undo
                ));
            }
        }
        if !pending.is_empty() {
            lines.push("Pending confirmations:".to_string());
            for item in pending.iter().take(5) {
                lines.push(format!(
                    "- {} {} requested by {:?}: {}",
                    item.action, item.entity, item.requested_by, item.reason
                ));
            }
        }
        lines.join("\n")
    }

    pub async fn confirm_pending_home_action(&self, token: &str) -> Result<String> {
        let pending = self
            .confirmations
            .confirm(token)
            .ok_or_else(|| anyhow::anyhow!("unknown or expired confirmation token"))?;
        let call = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": pending.entity,
                "action": pending.action,
                "value": pending.value,
            }),
        };
        // Re-enter with the channel that ORIGINALLY requested the action, not a
        // synthetic `Confirmation` origin. The `confirmed: true` flag is what
        // tells the policy gate the action is pre-approved; overriding origin
        // would (a) hide the originating channel in `AuditEvent.origin`,
        // (b) bypass `max_actions_per_minute_by_origin` for the requesting
        // channel by charging the `Confirmation` bucket instead, and
        // (c) break ACL setups whose `allowed_origins` exclude
        // `"confirmation"`. The original-bucket already paid one slot when
        // the request returned `ConfirmationRequired`, so the limiter skips
        // re-charging here (see `confirmed`-guard in `exec_home_control_inner`).
        let result = self
            .execute_with_context(
                &call,
                ToolExecutionContext {
                    request_origin: pending.requested_by,
                    confirmed: true,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        if result.success {
            Ok(result.output)
        } else {
            Err(anyhow::anyhow!(result.output))
        }
    }

    async fn exec_home_status(&self, args: &serde_json::Value) -> Result<String> {
        let ha = self
            .ha
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Home Assistant not connected"))?;
        let entity_name = parse_home_status_args(args)?;
        let entity_name = self.resolve_device_alias(entity_name);

        home::status(ha.as_ref(), &entity_name).await
    }

    fn resolve_device_alias(&self, query: &str) -> String {
        let Some(memory) = &self.memory else {
            return query.to_string();
        };
        let Ok(memory) = memory.lock() else {
            return query.to_string();
        };
        memory
            .device_alias(query)
            .ok()
            .flatten()
            .map(|alias| alias.target_id)
            .unwrap_or_else(|| query.to_string())
    }

    fn exec_set_timer(&self, args: &serde_json::Value) -> Result<String> {
        let (seconds, label) = parse_set_timer_args(args)?;
        self.timers
            .set(seconds, label)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(format!("Timer set for {} seconds: {}", seconds, label))
    }

    fn exec_memory_recall(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        let query = parse_memory_recall_query(args)?;
        // Identity context must come only from trusted runtime surfaces (voice
        // pipeline via exec_ctx.memory_read_context). Do not read
        // identity_confidence or related fields from LLM tool arguments — an
        // attacker could otherwise bypass shared-room privacy controls (#430).
        let read_context = exec_ctx
            .memory_read_context
            .unwrap_or_else(crate::memory::policy::MemoryReadContext::shared_room_voice);
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let query_ref = query.as_str();

        if let Some(answer) = mem.structured_household_answer(query_ref)? {
            return Ok(answer);
        }

        if let Some(role) = household_role_query(query_ref) {
            let profiles = mem.household_profiles_by_role(role)?;
            if !profiles.is_empty() {
                return Ok(format_household_role_answer(role, &profiles));
            }
        }

        let results =
            crate::memory::recall::recall_with_context(&mem, query_ref, 10, read_context)?;
        if results.is_empty() {
            return Ok(match query_ref {
                "name" => "I don't remember your name yet.".to_string(),
                "user" => "I don't remember anything about you yet.".to_string(),
                other => format!("I don't remember anything about {} yet.", other),
            });
        }

        if query_ref == "name"
            && let Some(entry) = results
                .iter()
                .find(|entry| entry.entry.content.to_lowercase().contains("name is "))
        {
            return Ok(entry
                .entry
                .content
                .replace("User's name is ", "Your name is "));
        }

        if query_ref == "user" || query_ref == "me" {
            let items = results
                .iter()
                .take(3)
                .map(|entry| entry.entry.content.clone())
                .collect::<Vec<_>>();
            return Ok(format!("I remember:\n- {}", items.join("\n- ")));
        }

        if results.len() == 1 {
            return Ok(format!("I remember: {}", results[0].entry.content));
        }

        let items = results
            .iter()
            .map(|entry| format!("- [{}] {}", entry.entry.kind, entry.entry.content))
            .collect::<Vec<_>>();
        Ok(format!("I found these memories:\n{}", items.join("\n")))
    }

    fn exec_memory_status(&self) -> Result<String> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let health = mem.health()?;
        let promoted = mem.promoted_count()?;
        let state = if health.quick_check_ok && health.fts_consistent && !health.migration_degraded
        {
            "ok"
        } else {
            "degraded"
        };

        Ok(format!(
            "Memory status: {}. Rows: {}. FTS rows: {}. FTS consistent: {}. Migration degraded: {}. Promoted memories: {}. Canonical root: {}. Namespace notes: {}. Daily notes: {}. Event logs: {}. Person-scoped memories: {}. Private memories: {}. Restricted memories: {}.",
            state,
            health.memory_rows,
            health.fts_rows,
            if health.fts_consistent { "yes" } else { "no" },
            if health.migration_degraded {
                "yes"
            } else {
                "no"
            },
            promoted,
            if health.canonical_root_exists {
                "present"
            } else {
                "missing"
            },
            health.canonical_namespace_files,
            health.canonical_daily_files,
            health.canonical_event_logs,
            health.person_rows,
            health.private_rows,
            health.restricted_rows,
        ))
    }

    fn exec_memory_forget(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        // Validate the argument at the execution boundary, before touching the
        // memory backend, the same way exec_memory_recall does (PR #362). The
        // previous `unwrap_or("")` silently coerced a missing or non-string
        // `query` into "", and a whitespace-only query slipped past the
        // `is_empty()` check straight into `mem.search("   ", ...)` — running a
        // destructive forget on garbage input instead of being rejected and
        // audited like its read-side sibling.
        let query = parse_memory_forget_query(args)?;
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;

        // Gate deletes through the same MemoryReadContext that exec_memory_recall
        // uses. Without it, an LLM that cannot READ a person-scoped row could
        // still DELETE it by calling memory_forget — destroying data it has no
        // privilege to see. This mirrors the read-side fix landed in
        // PR #201 (commit be4a2da).
        let read_context = exec_ctx
            .memory_read_context
            .unwrap_or_else(crate::memory::policy::MemoryReadContext::shared_room_voice);
        let candidates = mem.search(query, 10)?;
        let allowed = crate::memory::recall::filter_recall_results(candidates, read_context);
        let mut deleted = 0usize;
        for recallable in &allowed {
            if mem.delete_by_id(recallable.entry.id)? {
                deleted += 1;
            }
        }

        if deleted == 0 {
            Ok(format!("No memories found matching '{}'.", query))
        } else {
            Ok(format!("Forgot {} memory(ies) about '{}'.", deleted, query))
        }
    }

    fn exec_memory_store(&self, args: &serde_json::Value) -> Result<String> {
        // Validate content before the lock; previously a soft Ok() audited as success (#416).
        let memories = parse_memory_store_content(args)?;
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;

        let mut stored = Vec::new();
        let mut stored_categories = Vec::new();
        let mut rejected = Vec::new();
        let mut replaced = 0;
        for (category, content) in memories {
            let policy = crate::memory::policy::assess_memory_write(&category, &content);
            if !policy.allowed {
                rejected.push(policy.reason);
                continue;
            }
            let outcome = mem.store_resolved(&category, &content)?;
            replaced += outcome.replaced;
            stored_categories.push(category);
            stored.push(content);
        }

        if stored.is_empty() {
            return Ok(rejected
                .first()
                .copied()
                .unwrap_or("I could not store that memory.")
                .to_string());
        }

        if stored_categories
            .iter()
            .any(|category| category == "shopping")
        {
            let count = mem.shopping_list_pending_count().unwrap_or(0);
            let removed = stored.iter().any(|content| {
                content
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("shopping list removed:")
            });
            let added = stored
                .iter()
                .map(|content| {
                    content
                        .trim_start_matches("shopping list pending:")
                        .trim_start_matches("shopping list removed:")
                        .trim()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            if removed {
                return Ok(format!(
                    "Removed {added} from the shopping list. You have {count} item(s) total."
                ));
            }
            return Ok(format!(
                "Added {added} to the shopping list. You have {count} item(s) total."
            ));
        }

        if stored.len() == 1 {
            if replaced > 0 {
                Ok(format!(
                    "I've updated that memory: {}.",
                    stored[0].to_lowercase()
                ))
            } else {
                Ok(format!("I'll remember that {}.", stored[0].to_lowercase()))
            }
        } else {
            let prefix = if replaced > 0 {
                "I've updated these details"
            } else {
                "I'll remember these details"
            };
            let mut response = format!("{prefix}:\n- {}", stored.join("\n- "));
            if let Some(reason) = rejected.first() {
                response.push_str(&format!("\nSkipped one memory: {reason}"));
            }
            Ok(response)
        }
    }

    fn skill_tool_defs(&self) -> Option<Vec<ToolDef>> {
        let skills = self.skills.as_ref()?;
        let loader = skills.lock().ok()?;
        Some(
            loader
                .loaded()
                .iter()
                .map(|skill| ToolDef {
                    name: skill.name.clone(),
                    description: runtime_skill_description(skill),
                    parameters: serde_json::from_str(&skill.parameters_json).unwrap_or_else(
                        |_| serde_json::json!({"type": "object", "properties": {}}),
                    ),
                })
                .collect(),
        )
    }

    async fn exec_skill(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        let skills = self
            .skills
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;

        let args_json = serde_json::to_string(args)?;

        // Build a Send invocation handle under a short lock, then drop the lock
        // BEFORE awaiting the (possibly blocking) C call. The invocation owns an
        // Arc to the skill's library, so the native code stays mapped for the
        // whole call even though the loader lock is released. Holding a
        // std::sync::Mutex guard across the await would both serialize every
        // other skill access and trip clippy's `await_holding_lock`.
        let invocation = {
            let loader = skills
                .lock()
                .map_err(|e| anyhow::anyhow!("skill loader lock: {}", e))?;
            let skill = loader
                .loaded()
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
            skill.prepare(&args_json)
        };

        let outcome = invocation.run().await;

        // Re-acquire the lock to record the fault and reap a skill that has
        // exceeded its fault budget. The skill may have been unloaded meanwhile;
        // that is fine — the Arc kept its library alive for the call above.
        {
            let mut loader = skills
                .lock()
                .map_err(|e| anyhow::anyhow!("skill loader lock: {}", e))?;
            if outcome.faulted
                && let Some(skill) = loader.get_mut(name)
            {
                skill.fault_count += 1;
            }
            let pruned = loader.prune_faulted();
            if pruned.iter().any(|skill_name| skill_name == name) {
                tracing::warn!(skill = name, "skill auto-unloaded after repeated faults");
            }
        }

        if outcome.success {
            Ok(outcome.output)
        } else {
            Err(anyhow::anyhow!("{}", outcome.output))
        }
    }

    async fn exec_play_media(&self, args: &serde_json::Value) -> Result<String> {
        let query = parse_play_media_query(args)?;
        let resolved = self.resolve_media_query(query);
        tracing::info!(
            query,
            resolved_query = resolved.query.as_str(),
            provider = resolved.provider.as_deref().unwrap_or("unknown"),
            "triggering media mode via governor control socket"
        );
        write_media_request(&resolved).await;

        // Send media_start command to the governor via its Unix control socket.
        let response = governor_command(r#"{"cmd":"media_start"}"#).await;

        match response {
            Some(resp) => {
                let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    Ok(format!(
                        "Playing: {}. Switched to media mode — LLM unloaded, HDMI ready.",
                        resolved.display()
                    ))
                } else {
                    let err = resp
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    Err(anyhow::anyhow!("governor rejected media mode: {}", err))
                }
            }
            None => {
                // Fallback: write file trigger if governor socket is unavailable.
                let _ = tokio::fs::create_dir_all("/run/geniepod").await;
                tokio::fs::write("/run/geniepod/media_mode", b"1").await?;
                Ok(format!(
                    "Playing: {}. Media mode triggered (file fallback).",
                    resolved.display()
                ))
            }
        }
    }

    fn resolve_media_query(&self, query: &str) -> ResolvedMediaQuery {
        let Some(memory) = &self.memory else {
            return ResolvedMediaQuery::unresolved(query);
        };
        let Ok(memory) = memory.lock() else {
            return ResolvedMediaQuery::unresolved(query);
        };
        match memory.media_playlist_for_query(query).ok().flatten() {
            Some(item) => ResolvedMediaQuery {
                query: item.name,
                provider: item.provider,
                target: Some(item.target),
                source: "memory".into(),
            },
            None => ResolvedMediaQuery::unresolved(query),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct ResolvedMediaQuery {
    query: String,
    provider: Option<String>,
    target: Option<String>,
    source: String,
}

impl ResolvedMediaQuery {
    fn unresolved(query: &str) -> Self {
        Self {
            query: query.trim().to_string(),
            provider: None,
            target: None,
            source: "query".into(),
        }
    }

    fn display(&self) -> String {
        match (&self.provider, &self.target) {
            (Some(provider), Some(target))
                if target
                    .to_ascii_lowercase()
                    .starts_with(&format!("{provider}:")) =>
            {
                format!("{} ({target})", self.query)
            }
            (Some(provider), Some(target)) => format!("{} ({provider}: {target})", self.query),
            (_, Some(target)) => format!("{} ({target})", self.query),
            _ => self.query.clone(),
        }
    }
}

pub fn tool_action_class(name: &str) -> ToolActionClass {
    match name {
        "home_control" | "home_undo" => ToolActionClass::HomeActuation,
        "play_media" => ToolActionClass::Media,
        "memory_recall" => ToolActionClass::MemoryRead,
        "memory_forget" | "memory_store" => ToolActionClass::MemoryWrite,
        "memory_status" | "system_info" | "action_history" => ToolActionClass::Diagnostic,
        "web_search" | "get_weather" => ToolActionClass::Network,
        "set_timer" => ToolActionClass::Timer,
        "home_status" | "get_time" | "calculate" => ToolActionClass::ReadOnly,
        _ => ToolActionClass::Skill,
    }
}

fn actuation_origin_allowed(config: &ActuationSafetyConfig, origin: RequestOrigin) -> bool {
    config
        .allowed_origins
        .iter()
        .any(|allowed| allowed.trim().eq_ignore_ascii_case(origin.as_policy_key()))
}

impl ActuationRateLimiter {
    fn check_and_record(
        &self,
        config: &ActuationSafetyConfig,
        origin: RequestOrigin,
    ) -> Result<()> {
        let limit = actuation_rate_limit(config, origin);
        if limit == 0 {
            anyhow::bail!(
                "actuation from '{}' is rate-limited to zero actions per minute",
                origin.as_policy_key()
            );
        }

        let now = now_ms();
        let cutoff = now.saturating_sub(ACTUATION_RATE_WINDOW_MS);
        let mut attempts = self.attempts.lock().expect("actuation rate limiter lock");
        let bucket = attempts.entry(origin).or_default();
        while bucket.front().copied().is_some_and(|ts| ts < cutoff) {
            bucket.pop_front();
        }
        if bucket.len() >= limit {
            anyhow::bail!(
                "actuation from '{}' exceeded {} action(s) per minute",
                origin.as_policy_key(),
                limit
            );
        }
        bucket.push_back(now);
        Ok(())
    }
}

fn actuation_rate_limit(config: &ActuationSafetyConfig, origin: RequestOrigin) -> usize {
    config
        .max_actions_per_minute_by_origin
        .iter()
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(origin.as_policy_key()))
        .map(|(_, limit)| *limit)
        .unwrap_or(config.max_actions_per_minute)
}

impl ToolRateLimiter {
    fn check_and_record(&self, policy: &ToolPolicyConfig, tool: &str) -> Result<()> {
        let Some(limit) = tool_rate_limit(policy, tool) else {
            return Ok(());
        };
        if limit == 0 {
            anyhow::bail!("tool '{}' is rate-limited to zero calls per minute", tool);
        }

        let now = now_ms();
        let cutoff = now.saturating_sub(ACTUATION_RATE_WINDOW_MS);
        let mut attempts = self.attempts.lock().expect("tool rate limiter lock");
        let bucket = attempts.entry(tool.to_string()).or_default();
        while bucket.front().copied().is_some_and(|ts| ts < cutoff) {
            bucket.pop_front();
        }
        if bucket.len() >= limit {
            anyhow::bail!("tool '{}' exceeded {} call(s) per minute", tool, limit);
        }
        bucket.push_back(now);
        Ok(())
    }
}

/// Per-tool limit for `tool`, honoring an exact match first and a `"*"`
/// catch-all fallback. `None` means the tool is unlimited.
fn tool_rate_limit(policy: &ToolPolicyConfig, tool: &str) -> Option<usize> {
    if !policy.enabled {
        return None;
    }
    policy
        .max_actions_per_minute_by_tool
        .get(tool)
        .or_else(|| policy.max_actions_per_minute_by_tool.get("*"))
        .copied()
}

impl ToolConfirmationGate {
    fn evaluate(
        &self,
        origin: RequestOrigin,
        tool: &str,
        args: &serde_json::Value,
        ttl_ms: u64,
        confirmed: bool,
    ) -> ToolConfirmDecision {
        let key = tool_confirmation_key(origin, tool, args);
        let now = now_ms();
        let mut pending = self.pending.lock().expect("tool confirmation gate lock");
        pending.retain(|_, first_seen| {
            now.saturating_sub(*first_seen) < TOOL_CONFIRMATION_RETENTION_MS
        });

        if !confirmed {
            // First leg: record (or refresh) the request and ask for a repeat.
            if pending.len() >= MAX_TOOL_CONFIRMATIONS
                && !pending.contains_key(&key)
                && let Some(oldest) = pending
                    .iter()
                    .min_by_key(|(_, first_seen)| **first_seen)
                    .map(|(oldest_key, _)| oldest_key.clone())
            {
                pending.remove(&oldest);
            }
            pending.insert(key.clone(), now);
            return ToolConfirmDecision::Pending {
                token: tool_confirmation_token(&key),
            };
        }

        // Confirming leg: succeed only when a matching first leg is still inside
        // the TTL window. A missing or stale first leg reports as expired.
        match pending.remove(&key) {
            Some(first_seen) if now.saturating_sub(first_seen) <= ttl_ms => {
                ToolConfirmDecision::Confirmed
            }
            _ => ToolConfirmDecision::Expired,
        }
    }
}

/// Whether `tool` is in `requires_confirmation_tools` (wildcards supported).
/// Only consulted when the tool policy is enabled.
fn tool_requires_confirmation(policy: &ToolPolicyConfig, tool: &str) -> bool {
    policy.enabled
        && policy
            .requires_confirmation_tools
            .iter()
            .any(|entry| entry == "*" || entry.trim().eq_ignore_ascii_case(tool))
}

/// Stable key for a confirmable request: identical `(origin, tool, arguments)`
/// triples map to the same key so the confirming leg matches its first leg.
fn tool_confirmation_key(origin: RequestOrigin, tool: &str, args: &serde_json::Value) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        origin.as_policy_key(),
        tool,
        serde_json::to_string(args).unwrap_or_default()
    )
}

/// Stable, non-secret token derived from the confirmation key. It only
/// identifies the pending request (the args themselves are the authorization),
/// so unlike a home-actuation token it is safe to surface to the caller.
fn tool_confirmation_token(key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("conf-{:016x}", hasher.finish())
}

fn household_role_query(query: &str) -> Option<&'static str> {
    let normalized = query
        .trim()
        .to_ascii_lowercase()
        .replace(
            |ch: char| !ch.is_ascii_alphanumeric() && !ch.is_whitespace(),
            " ",
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let role = tokens
        .iter()
        .find_map(|token| normalize_household_role_query_token(token))?;

    let is_role_question = normalized.starts_with("who is ")
        || normalized.starts_with("who are ")
        || normalized.starts_with("whos ")
        || normalized.starts_with("who s ")
        || normalized.contains(" in this house")
        || normalized.contains(" in our house")
        || normalized.contains(" household");
    let is_direct_role_topic = tokens.len() == 1
        || (tokens.len() == 2
            && normalize_household_role_query_token(tokens[0]).is_some()
            && matches!(tokens[1], "name" | "names"));

    if is_role_question || is_direct_role_topic {
        Some(role)
    } else {
        None
    }
}

fn normalize_household_role_query_token(token: &str) -> Option<&'static str> {
    match token {
        "dad" | "father" => Some("dad"),
        "mom" | "mother" | "mum" => Some("mom"),
        "son" | "sons" => Some("son"),
        "daughter" | "daughters" => Some("daughter"),
        "child" | "children" | "kid" | "kids" => Some("child"),
        "wife" => Some("wife"),
        "husband" => Some("husband"),
        "partner" => Some("partner"),
        "dog" | "dogs" => Some("dog"),
        "cat" | "cats" => Some("cat"),
        "pet" | "pets" => Some("pet"),
        _ => None,
    }
}

fn format_household_role_answer(
    role: &str,
    profiles: &[crate::memory::HouseholdProfile],
) -> String {
    if profiles.len() == 1 {
        return format!("{} is the {}.", profiles[0].name, role);
    }

    let names = profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{names} are the {role}s.")
}

fn normalize_memories_to_store(args: &serde_json::Value) -> Vec<(String, String)> {
    let category_hint = args
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("fact");

    let primary = ["content", "fact", "text", "memory", "note"]
        .iter()
        .find_map(|key| args.get(*key).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            args.as_object().and_then(|obj| {
                obj.iter()
                    .filter(|(key, _)| key.as_str() != "category")
                    .find_map(|(_, value)| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        });

    let mut normalized = Vec::new();

    if let Some(content) = primary {
        let extracted = crate::memory::extract::extract_facts(&content);
        if extracted.is_empty() {
            normalized.push((category_hint.to_string(), content));
        } else {
            normalized.extend(
                extracted
                    .into_iter()
                    .map(|fact| (fact.category, fact.content))
                    .collect::<Vec<_>>(),
            );
        }
    } else if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
        let name = name.trim();
        if !name.is_empty() {
            normalized.push(("identity".into(), format!("User's name is {}", name)));
        }
    }

    normalized
}

fn runtime_skill_description(skill: &crate::skills::LoadedSkill) -> String {
    if skill.name == "hello_world" {
        "Demo greeting skill. Only use when the user explicitly asks you to say hello to someone or test the hello_world demo skill.".into()
    } else {
        skill.description.clone()
    }
}

fn tool_origin_allowed(
    policy: &ToolPolicyConfig,
    origin: RequestOrigin,
    tool_name: &str,
) -> Result<()> {
    if !policy.enabled {
        return Ok(());
    }

    let origin_key = origin.as_policy_key();
    if tool_list_contains(&policy.denied_tools_by_origin, origin_key, tool_name) {
        anyhow::bail!("tool '{}' is denied for origin '{}'", tool_name, origin_key);
    }

    if let Some(allowed) = origin_tool_list(&policy.allowed_tools_by_origin, origin_key)
        && !tool_matches(allowed, tool_name)
    {
        anyhow::bail!(
            "tool '{}' is not in the allowlist for origin '{}'",
            tool_name,
            origin_key
        );
    }

    Ok(())
}

fn tool_list_contains(
    rules: &HashMap<String, Vec<String>>,
    origin_key: &str,
    tool_name: &str,
) -> bool {
    origin_tool_list(rules, origin_key)
        .map(|tools| tool_matches(tools, tool_name))
        .unwrap_or(false)
}

fn origin_tool_list<'a>(
    rules: &'a HashMap<String, Vec<String>>,
    origin_key: &str,
) -> Option<&'a Vec<String>> {
    rules.get(origin_key).or_else(|| rules.get("*"))
}

fn tool_matches(tools: &[String], tool_name: &str) -> bool {
    tools.iter().any(|tool| tool == "*" || tool == tool_name)
}

fn tool_argument_keys(args: &serde_json::Value) -> Vec<String> {
    let Some(object) = args.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn exec_calculate(args: &serde_json::Value) -> Result<String> {
    let expr = parse_calculate_expression(args)?;
    match super::calc::evaluate(expr) {
        Ok(result) => {
            // Format nicely: drop trailing zeros for integers.
            if result == result.floor() && result.abs() < 1e15 {
                Ok(format!("{} = {}", expr, result as i64))
            } else {
                Ok(format!("{} = {:.6}", expr, result))
            }
        }
        Err(e) => Err(anyhow::anyhow!("calculation error: {}", e)),
    }
}

async fn exec_weather(args: &serde_json::Value) -> Result<String> {
    let location = parse_get_weather_location(args)?;
    let forecast = parse_get_weather_forecast(args)?;

    if forecast {
        super::weather::get_forecast(location).await
    } else {
        super::weather::get_weather(location).await
    }
}

async fn exec_web_search(args: &serde_json::Value, config: &WebSearchConfig) -> Result<String> {
    let query = parse_web_search_query(args)?;
    let limit = parse_web_search_limit(args)?;
    let fresh = args
        .get("fresh")
        .or_else(|| args.get("cache_bypass"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    super::web_search::search_with_options(query, limit, config, fresh).await
}

fn get_current_time() -> String {
    // Use libc for proper timezone.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    #[cfg(unix)]
    {
        let time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&time_t, &mut tm) };
        if !result.is_null() {
            return format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            );
        }
    }

    format!("Unix timestamp: {}", secs)
}

/// Send a JSON command to the governor's Unix control socket.
/// Returns parsed JSON response, or None if the governor is unreachable.
async fn governor_command(json_cmd: &str) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect("/run/geniepod/governor.sock")
        .await
        .ok()?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(json_cmd.as_bytes()).await.ok()?;
    writer.write_all(b"\n").await.ok()?;

    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .ok()?
        .ok()?;

    line.and_then(|l| serde_json::from_str(&l).ok())
}

async fn write_media_request(request: &ResolvedMediaQuery) {
    let result: Result<()> = async {
        tokio::fs::create_dir_all("/run/geniepod").await?;
        let json = serde_json::to_vec(request)?;
        tokio::fs::write("/run/geniepod/media_request.json", json).await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::debug!(error = %error, "media request sidecar write skipped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::{
        ActionResult, DeviceRef, Entity, HomeAction, HomeActionKind, HomeAutomationProvider,
        HomeGraph, HomeState, HomeTarget, HomeTargetKind, IntegrationHealth, SceneRef,
    };
    use crate::skills::SkillLoader;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubHomeProvider;

    struct RecordingHomeProvider {
        executed: Arc<std::sync::Mutex<Vec<HomeActionKind>>>,
        light: Arc<std::sync::Mutex<StubLightState>>,
    }

    struct StubLightState {
        power: String,
        brightness: Option<u64>,
    }

    impl StubLightState {
        fn new() -> Self {
            Self {
                power: "off".into(),
                brightness: None,
            }
        }
    }

    impl RecordingHomeProvider {
        fn new(executed: Arc<std::sync::Mutex<Vec<HomeActionKind>>>) -> Self {
            Self {
                executed,
                light: Arc::new(std::sync::Mutex::new(StubLightState::new())),
            }
        }

        fn brightness(&self) -> Option<u64> {
            self.light.lock().unwrap().brightness
        }

        fn power(&self) -> String {
            self.light.lock().unwrap().power.clone()
        }
    }

    fn workspace_root() -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().parent().unwrap().to_path_buf()
    }

    fn sample_skill_path() -> &'static Path {
        static SAMPLE_SKILL_PATH: OnceLock<PathBuf> = OnceLock::new();
        SAMPLE_SKILL_PATH.get_or_init(|| {
            let root = workspace_root();
            let build_dir = std::env::temp_dir().join(format!(
                "geniepod-sample-skill-build-dispatch-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&build_dir);
            std::fs::create_dir_all(&build_dir).unwrap();
            let output = Command::new("cargo")
                .args(["build", "-p", "genie-skill-hello", "--target-dir"])
                .arg(&build_dir)
                .current_dir(&root)
                .output()
                .expect("failed to build sample skill");

            assert!(
                output.status.success(),
                "sample skill build failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            let candidates = [
                build_dir.join("debug/libgenie_skill_hello.so"),
                build_dir.join("debug/libgenie_skill_hello.dylib"),
                build_dir.join("debug/genie_skill_hello.dll"),
            ];

            candidates
                .into_iter()
                .find(|path| path.exists())
                .expect("sample skill artifact not found")
        })
    }

    fn sample_skill_loader() -> SkillLoader {
        static TEMP_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let skill_path = sample_skill_path();
        let dir = std::env::temp_dir().join(format!(
            "geniepod-dispatch-skill-test-{}-{}",
            std::process::id(),
            TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let installed_path = dir.join(skill_path.file_name().unwrap());
        std::fs::copy(skill_path, &installed_path).unwrap();

        let mut loader = SkillLoader::new(&dir);
        let loaded = loader.load_skill(&installed_path).unwrap();
        assert_eq!(loaded, "hello_world");
        loader
    }

    #[async_trait::async_trait]
    impl HomeAutomationProvider for StubHomeProvider {
        async fn health(&self) -> IntegrationHealth {
            IntegrationHealth {
                connected: true,
                cached_graph: true,
                message: "ok".into(),
            }
        }

        async fn sync_structure(&self) -> Result<HomeGraph> {
            Ok(HomeGraph {
                areas: Vec::new(),
                devices: Vec::new(),
                entities: Vec::new(),
                scenes: Vec::new(),
                scripts: Vec::new(),
                aliases: Vec::new(),
                domains: Vec::new(),
                capabilities: Vec::new(),
            })
        }

        async fn resolve_target(
            &self,
            _query: &str,
            _action_hint: Option<crate::ha::HomeActionKind>,
        ) -> Result<HomeTarget> {
            anyhow::bail!("not used in test")
        }

        async fn get_state(&self, _target: &HomeTarget) -> Result<HomeState> {
            anyhow::bail!("not used in test")
        }

        async fn execute(&self, _action: HomeAction) -> Result<ActionResult> {
            anyhow::bail!("not used in test")
        }

        async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
            Ok(Vec::new())
        }

        async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl HomeAutomationProvider for RecordingHomeProvider {
        async fn health(&self) -> IntegrationHealth {
            IntegrationHealth {
                connected: true,
                cached_graph: true,
                message: "ok".into(),
            }
        }

        async fn sync_structure(&self) -> Result<HomeGraph> {
            anyhow::bail!("not used in test")
        }

        async fn resolve_target(
            &self,
            query: &str,
            _action_hint: Option<HomeActionKind>,
        ) -> Result<HomeTarget> {
            Ok(HomeTarget {
                kind: HomeTargetKind::Entity,
                query: query.into(),
                display_name: query.into(),
                entity_ids: vec!["light.test".into()],
                domain: Some("light".into()),
                area: Some("Kitchen".into()),
                confidence: 0.96,
                voice_safe: true,
            })
        }

        async fn get_state(&self, target: &HomeTarget) -> Result<HomeState> {
            let light = self.light.lock().unwrap();
            let mut attributes = serde_json::Map::new();
            if let Some(brightness) = light.brightness {
                attributes.insert("brightness".into(), serde_json::json!(brightness));
            }
            Ok(HomeState {
                target_name: target.display_name.clone(),
                domain: target.domain.clone(),
                area: target.area.clone(),
                entities: vec![Entity {
                    entity_id: target.entity_ids[0].clone(),
                    state: light.power.clone(),
                    attributes: serde_json::Value::Object(attributes),
                }],
                available: true,
                spoken_summary: format!("{} is {}", target.display_name, light.power),
            })
        }

        async fn execute(&self, action: HomeAction) -> Result<ActionResult> {
            {
                let mut light = self.light.lock().unwrap();
                match action.kind {
                    HomeActionKind::TurnOn => {
                        light.power = "on".into();
                        if light.brightness.is_none() {
                            light.brightness = Some(255);
                        }
                    }
                    HomeActionKind::TurnOff => {
                        light.power = "off".into();
                        light.brightness = None;
                    }
                    HomeActionKind::Toggle => {
                        if light.power == "on" {
                            light.power = "off".into();
                            light.brightness = None;
                        } else {
                            light.power = "on".into();
                            if light.brightness.is_none() {
                                light.brightness = Some(255);
                            }
                        }
                    }
                    HomeActionKind::SetBrightness => {
                        light.power = "on".into();
                        if let Some(value) = action.value {
                            light.brightness =
                                Some(((value * 255.0 / 100.0).round() as u64).min(255));
                        }
                    }
                    other => anyhow::bail!("unsupported stub action: {other:?}"),
                }
            }
            self.executed.lock().unwrap().push(action.kind);
            Ok(ActionResult {
                success: true,
                spoken_summary: format!("Executed {:?}", action.kind),
                affected_targets: vec![action.target.display_name],
                state_snapshot: None,
                confidence: Some(action.target.confidence),
            })
        }

        async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
            Ok(Vec::new())
        }

        async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn tool_defs_hide_home_tools_when_unavailable() {
        let dispatcher = ToolDispatcher::new(None);
        let defs = dispatcher.tool_defs();
        assert!(defs.len() >= 4);
        assert!(!defs.iter().any(|d| d.name == "home_control"));
        assert!(defs.iter().any(|d| d.name == "set_timer"));
        assert!(defs.iter().any(|d| d.name == "web_search"));
    }

    #[test]
    fn tool_defs_include_home_tools_when_available() {
        let dispatcher = ToolDispatcher::new(Some(Arc::new(StubHomeProvider)));
        let defs = dispatcher.tool_defs();
        assert!(defs.iter().any(|d| d.name == "home_control"));
        assert!(defs.iter().any(|d| d.name == "home_status"));
        assert!(defs.iter().any(|d| d.name == "home_undo"));
        assert!(defs.iter().any(|d| d.name == "action_history"));
    }

    #[test]
    fn tool_defs_hide_web_search_when_disabled() {
        let web_search = WebSearchConfig {
            enabled: false,
            ..WebSearchConfig::default()
        };
        let dispatcher = ToolDispatcher::new(None).with_web_search_config(web_search);
        let defs = dispatcher.tool_defs();

        assert!(!defs.iter().any(|d| d.name == "web_search"));
        assert!(!dispatcher.has_web_search());
    }

    #[test]
    fn get_time_returns_something() {
        let time = get_current_time();
        assert!(!time.is_empty());
    }

    #[tokio::test]
    async fn execute_unknown_tool() {
        let dispatcher = ToolDispatcher::new(None);
        let call = ToolCall {
            name: "nonexistent".into(),
            arguments: serde_json::json!({}),
        };
        let result = dispatcher.execute(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("unknown tool"));
    }

    #[tokio::test]
    async fn tool_policy_blocks_denied_tool_by_origin() {
        let mut policy = ToolPolicyConfig::default();
        policy
            .denied_tools_by_origin
            .insert("telegram".into(), vec!["web_search".into()]);
        let dispatcher = ToolDispatcher::new(None).with_tool_policy_config(policy);

        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "web_search".into(),
                    arguments: serde_json::json!({"query": "test"}),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(!result.success);
        assert!(result.output.contains("origin policy"));
    }

    #[tokio::test]
    async fn tool_policy_allowlist_blocks_unspecified_tool() {
        let mut policy = ToolPolicyConfig::default();
        policy
            .allowed_tools_by_origin
            .insert("voice".into(), vec!["get_time".into()]);
        let dispatcher = ToolDispatcher::new(None).with_tool_policy_config(policy);

        let allowed = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "get_time".into(),
                    arguments: serde_json::json!({}),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Voice,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        let blocked = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "calculate".into(),
                    arguments: serde_json::json!({"expression": "1 + 1"}),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Voice,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(allowed.success);
        assert!(!blocked.success);
        assert!(blocked.output.contains("allowlist"));
    }

    #[tokio::test]
    async fn execute_get_time() {
        let dispatcher = ToolDispatcher::new(None);
        let call = ToolCall {
            name: "get_time".into(),
            arguments: serde_json::json!({}),
        };
        let result = dispatcher.execute(&call).await;
        assert!(result.success);
        assert_eq!(result.action_class, ToolActionClass::ReadOnly);
        assert!(!result.output.is_empty());
    }

    #[test]
    fn home_control_canonicalizes_action_synonyms() {
        // The bug: a small model emits "turn off" (space), the runtime rejected
        // it, and a correct intent silently failed to actuate.
        for (raw, want) in [
            ("turn off", Some("turn_off")),
            ("Turn-Off", Some("turn_off")),
            ("deactivate", Some("turn_off")),
            ("disable", Some("turn_off")),
            ("turn on", Some("turn_on")),
            ("switch_on", Some("turn_on")),
            ("toggle", Some("toggle")),
            ("activate", Some("activate")), // distinct action, must not remap
            ("frobnicate", None),
        ] {
            assert_eq!(canon_home_control_action(raw), want, "action {raw:?}");
        }

        // End-to-end through the arg parser: "turn off" now resolves to "turn_off".
        let args = serde_json::json!({"entity": "kitchen lights", "action": "turn off"});
        let (entity, action, _) =
            parse_home_control_args(&args).expect("'turn off' should canonicalize and parse");
        assert_eq!(entity, "kitchen lights");
        assert_eq!(action, "turn_off");
    }

    #[test]
    fn home_control_value_must_be_numeric_when_provided() {
        // A numeric value parses through to Some(..).
        let args =
            serde_json::json!({"entity": "thermostat", "action": "set_temperature", "value": 72});
        let (_, _, value) = parse_home_control_args(&args).expect("numeric value parses");
        assert_eq!(value, Some(72.0));

        // An absent value is a legitimate no-op None — value stays optional.
        let args = serde_json::json!({"entity": "kitchen lights", "action": "turn_on"});
        let (_, _, value) = parse_home_control_args(&args).expect("absent value parses");
        assert_eq!(value, None);

        // An explicit null is also None, not a rejection.
        let args =
            serde_json::json!({"entity": "kitchen lights", "action": "turn_on", "value": null});
        let (_, _, value) = parse_home_control_args(&args).expect("null value parses");
        assert_eq!(value, None);

        // The bug: a provided but non-numeric value used to be silently dropped
        // to None, so the action actuated without it. It must now be rejected.
        for bad in [
            serde_json::json!({"entity": "thermostat", "action": "set_temperature", "value": "72"}),
            serde_json::json!({"entity": "thermostat", "action": "set_temperature", "value": true}),
            serde_json::json!({"entity": "thermostat", "action": "set_temperature", "value": [72]}),
        ] {
            let err = parse_home_control_args(&bad)
                .expect_err("non-numeric value must be rejected")
                .to_string();
            assert!(
                err.contains("home_control 'value' must be a number when provided"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn get_weather_forecast_must_be_boolean_when_provided() {
        // A real boolean parses through.
        let args = serde_json::json!({"location": "Denver", "forecast": true});
        assert!(parse_get_weather_forecast(&args).expect("bool forecast parses"));

        // Absent and explicit null both default to current weather (false),
        // not a rejection — forecast stays optional.
        assert!(
            !parse_get_weather_forecast(&serde_json::json!({"location": "Denver"}))
                .expect("absent forecast parses")
        );
        assert!(
            !parse_get_weather_forecast(
                &serde_json::json!({"location": "Denver", "forecast": null})
            )
            .expect("null forecast parses")
        );

        // The bug: a provided but non-boolean forecast (a stringified "true", a
        // number) used to be silently dropped to false, returning current
        // weather when the user asked for the forecast. It must now be rejected.
        for bad in [
            serde_json::json!({"location": "Denver", "forecast": "true"}),
            serde_json::json!({"location": "Denver", "forecast": 1}),
            serde_json::json!({"location": "Denver", "forecast": "yes"}),
        ] {
            let err = parse_get_weather_forecast(&bad)
                .expect_err("non-boolean forecast must be rejected")
                .to_string();
            assert!(
                err.contains("get_weather 'forecast' must be a boolean when provided"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn web_search_limit_must_be_integer_when_provided() {
        // A valid integer parses through.
        assert_eq!(
            parse_web_search_limit(&serde_json::json!({"query": "rust", "limit": 5}))
                .expect("integer limit parses"),
            5
        );

        // Absent and explicit null both default to 3 — limit stays optional.
        assert_eq!(
            parse_web_search_limit(&serde_json::json!({"query": "rust"}))
                .expect("absent limit parses"),
            3
        );
        assert_eq!(
            parse_web_search_limit(&serde_json::json!({"query": "rust", "limit": null}))
                .expect("null limit parses"),
            3
        );

        // A valid integer outside 1..=5 still clamps into range rather than
        // erroring, preserving the prior lenient behavior for in-type values.
        assert_eq!(
            parse_web_search_limit(&serde_json::json!({"query": "rust", "limit": 0}))
                .expect("zero clamps up"),
            1
        );
        assert_eq!(
            parse_web_search_limit(&serde_json::json!({"query": "rust", "limit": 99}))
                .expect("large clamps down"),
            5
        );

        // The bug: a provided but non-integer limit (a stringified "5", a float,
        // a negative) used to be silently dropped to the default 3. It must now
        // be rejected.
        for bad in [
            serde_json::json!({"query": "rust", "limit": "5"}),
            serde_json::json!({"query": "rust", "limit": 2.5}),
            serde_json::json!({"query": "rust", "limit": -1}),
        ] {
            let err = parse_web_search_limit(&bad)
                .expect_err("non-integer limit must be rejected")
                .to_string();
            assert!(
                err.contains("web_search 'limit' must be an integer when provided"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn home_control_set_actions_require_value() {
        // set_brightness / set_temperature with a value parse through.
        for ok in [
            serde_json::json!({"entity": "kitchen light", "action": "set_brightness", "value": 60}),
            serde_json::json!({"entity": "thermostat", "action": "set_temperature", "value": 21}),
        ] {
            let (_, _, value) = parse_home_control_args(&ok).expect("value provided parses");
            assert!(value.is_some());
        }

        // The bug (issue #421): a value-requiring action with NO value used to be
        // silently defaulted by the provider (brightness 50 / temperature 20) and
        // reported success. It must now be rejected at the boundary. Absent and
        // explicit null both count as missing.
        for bad in [
            serde_json::json!({"entity": "kitchen light", "action": "set_brightness"}),
            serde_json::json!({"entity": "kitchen light", "action": "set_brightness", "value": null}),
            serde_json::json!({"entity": "thermostat", "action": "set_temperature"}),
            serde_json::json!({"entity": "thermostat", "action": "set_temperature", "value": null}),
        ] {
            let err = parse_home_control_args(&bad)
                .expect_err("missing value must be rejected")
                .to_string();
            assert!(
                err.contains("requires a numeric argument 'value'"),
                "unexpected error: {err}"
            );
        }

        // Non-value actions are unaffected: no value is the correct no-op.
        for ok in [
            serde_json::json!({"entity": "kitchen light", "action": "turn_on"}),
            serde_json::json!({"entity": "front door", "action": "lock"}),
            serde_json::json!({"entity": "movie night", "action": "activate"}),
        ] {
            let (_, _, value) = parse_home_control_args(&ok).expect("no-value action parses");
            assert_eq!(value, None);
        }
    }

    #[test]
    fn action_requires_value_only_for_setpoint_actions() {
        // Only the two numeric-setpoint actions require a value (#421).
        for a in ["set_brightness", "set_temperature"] {
            assert!(action_requires_value(a), "{a} should require a value");
        }
        for a in [
            "turn_on", "turn_off", "toggle", "open", "close", "lock", "unlock", "activate",
        ] {
            assert!(!action_requires_value(a), "{a} must not require a value");
        }
    }

    #[test]
    fn tool_action_class_maps_side_effecting_tools() {
        assert_eq!(
            tool_action_class("home_control"),
            ToolActionClass::HomeActuation
        );
        assert_eq!(
            tool_action_class("memory_store"),
            ToolActionClass::MemoryWrite
        );
        assert_eq!(
            tool_action_class("memory_recall"),
            ToolActionClass::MemoryRead
        );
        assert_eq!(tool_action_class("web_search"), ToolActionClass::Network);
        assert_eq!(tool_action_class("custom_skill"), ToolActionClass::Skill);
        assert_eq!(ToolActionClass::HomeActuation.as_str(), "home_actuation");
    }

    #[test]
    fn household_role_query_ignores_non_role_topics() {
        assert_eq!(household_role_query("who is the dad"), Some("dad"));
        assert_eq!(household_role_query("dog name"), Some("dog"));
        assert_eq!(household_role_query("hot dog recipe"), None);
    }

    #[tokio::test]
    async fn tool_audit_records_origin_and_argument_keys_without_values() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-tool-audit-test-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let dispatcher = ToolDispatcher::new(None).with_tool_audit_path(path.clone());

        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "calculate".into(),
                    arguments: serde_json::json!({"expression": "secret-token-value"}),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Api,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(!result.success);
        let line = std::fs::read_to_string(&path).unwrap();
        let event: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(event["tool"], "calculate");
        assert_eq!(event["action_class"], "read_only");
        assert_eq!(event["origin"], "api");
        assert_eq!(event["success"], false);
        assert_eq!(event["argument_keys"], serde_json::json!(["expression"]));
        assert!(event["duration_ms"].as_u64().is_some());
        assert!(!line.contains("secret-token-value"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tool_audit_logger_disabled_appends_ok() {
        let logger = ToolAuditLogger::default();
        let event = ToolAuditEvent {
            ts_ms: now_ms(),
            tool: "calculate".into(),
            action_class: ToolActionClass::ReadOnly,
            origin: RequestOrigin::Api,
            success: true,
            decision: "executed",
            duration_ms: 1,
            argument_keys: vec!["expression".into()],
            output_chars: 3,
        };
        assert!(logger.append(event).is_ok());
    }

    #[test]
    fn tool_audit_logger_surfaces_blocked_parent_error() {
        let blocker = std::env::temp_dir().join(format!(
            "geniepod-tool-audit-blocker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&blocker);
        std::fs::write(&blocker, b"not a directory").unwrap();
        let logger = ToolAuditLogger::new(blocker.join("tool-audit.jsonl"));

        let event = ToolAuditEvent {
            ts_ms: now_ms(),
            tool: "calculate".into(),
            action_class: ToolActionClass::ReadOnly,
            origin: RequestOrigin::Api,
            success: true,
            decision: "executed",
            duration_ms: 1,
            argument_keys: vec!["expression".into()],
            output_chars: 3,
        };
        let err = logger.append(event).expect_err("append must fail");
        assert!(matches!(
            err,
            AuditError::CreateDir(_) | AuditError::Open(_)
        ));
        let _ = std::fs::remove_file(&blocker);
    }

    #[tokio::test]
    async fn execute_system_info_reports_home_assistant_health() {
        let dispatcher = ToolDispatcher::new(Some(Arc::new(StubHomeProvider)));
        let call = ToolCall {
            name: "system_info".into(),
            arguments: serde_json::json!({}),
        };

        let result = dispatcher.execute(&call).await;
        assert!(result.success);
        assert!(result.output.contains("Home Assistant: connected"));
    }

    #[tokio::test]
    async fn home_control_records_action_history() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher =
            ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(executed.clone()))));

        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_control".into(),
                    arguments: serde_json::json!({
                        "entity": "kitchen light",
                        "action": "turn_on"
                    }),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Dashboard,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(result.success);
        assert_eq!(*executed.lock().unwrap(), vec![HomeActionKind::TurnOn]);

        let history = dispatcher
            .execute(&ToolCall {
                name: "action_history".into(),
                arguments: serde_json::json!({}),
            })
            .await;
        assert!(history.success);
        assert!(history.output.contains("turn_on kitchen light"));
        assert!(history.output.contains("undo: turn_off"));
    }

    #[tokio::test]
    async fn home_control_resolves_structured_device_alias() {
        let db = std::env::temp_dir().join(format!(
            "home-control-device-alias-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("fact", "Playroom lights maps to light.playroom")
            .unwrap();

        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher =
            ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(executed.clone()))))
                .with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_control".into(),
                    arguments: serde_json::json!({
                        "entity": "playroom lights",
                        "action": "turn_on"
                    }),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Dashboard,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(result.success, "{}", result.output);
        assert_eq!(*executed.lock().unwrap(), vec![HomeActionKind::TurnOn]);

        let history = dispatcher
            .execute(&ToolCall {
                name: "action_history".into(),
                arguments: serde_json::json!({}),
            })
            .await;
        assert!(history.output.contains("turn_on light.playroom"));
    }

    #[tokio::test]
    async fn home_control_blocks_unknown_origin_by_default() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher =
            ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(executed.clone()))));

        let result = dispatcher
            .execute(&ToolCall {
                name: "home_control".into(),
                arguments: serde_json::json!({
                    "entity": "kitchen light",
                    "action": "turn_on"
                }),
            })
            .await;

        assert!(!result.success);
        assert!(result.output.contains("channel policy"));
        assert!(executed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn home_control_respects_configured_allowed_origins() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let safety = ActuationSafetyConfig {
            allowed_origins: vec!["dashboard".into(), "confirmation".into()],
            ..ActuationSafetyConfig::default()
        };
        let dispatcher =
            ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(executed.clone()))))
                .with_actuation_safety_config(safety);

        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_control".into(),
                    arguments: serde_json::json!({
                        "entity": "kitchen light",
                        "action": "turn_on"
                    }),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(!result.success);
        assert!(result.output.contains("telegram"));
        assert!(executed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn home_control_rate_limits_by_origin() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut safety = ActuationSafetyConfig::default();
        safety
            .max_actions_per_minute_by_origin
            .insert("dashboard".into(), 1);
        let dispatcher =
            ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(executed.clone()))))
                .with_actuation_safety_config(safety);
        let call = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": "kitchen light",
                "action": "turn_on"
            }),
        };
        let ctx = ToolExecutionContext {
            request_origin: RequestOrigin::Dashboard,
            ..ToolExecutionContext::default()
        };

        let first = dispatcher.execute_with_context(&call, ctx).await;
        let second = dispatcher.execute_with_context(&call, ctx).await;

        assert!(first.success);
        assert!(!second.success);
        assert!(second.output.contains("rate limit"));
        assert_eq!(*executed.lock().unwrap(), vec![HomeActionKind::TurnOn]);
    }

    #[tokio::test]
    async fn home_undo_reverses_last_reversible_action() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher =
            ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(executed.clone()))));

        let control = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": "kitchen light",
                "action": "turn_on"
            }),
        };
        assert!(
            dispatcher
                .execute_with_context(
                    &control,
                    ToolExecutionContext {
                        request_origin: RequestOrigin::Dashboard,
                        ..ToolExecutionContext::default()
                    },
                )
                .await
                .success
        );

        let undo = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_undo".into(),
                    arguments: serde_json::json!({}),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Dashboard,
                    ..ToolExecutionContext::default()
                },
            )
            .await;

        assert!(undo.success);
        assert!(undo.output.contains("Undid the last home action"));
        assert_eq!(
            *executed.lock().unwrap(),
            vec![HomeActionKind::TurnOn, HomeActionKind::TurnOff]
        );

        let second_undo = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_undo".into(),
                    arguments: serde_json::json!({}),
                },
                ToolExecutionContext {
                    request_origin: RequestOrigin::Dashboard,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        assert!(!second_undo.success);
        assert!(second_undo.output.contains("No recent reversible"));
    }

    #[tokio::test]
    async fn home_undo_restores_prior_brightness_after_dim() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingHomeProvider::new(executed.clone()));
        let dispatcher = ToolDispatcher::new(Some(provider.clone()));
        let ctx = || ToolExecutionContext {
            request_origin: RequestOrigin::Dashboard,
            ..ToolExecutionContext::default()
        };

        assert!(
            dispatcher
                .execute_with_context(
                    &ToolCall {
                        name: "home_control".into(),
                        arguments: serde_json::json!({
                            "entity": "kitchen light",
                            "action": "turn_on"
                        }),
                    },
                    ctx(),
                )
                .await
                .success
        );
        assert!(
            dispatcher
                .execute_with_context(
                    &ToolCall {
                        name: "home_control".into(),
                        arguments: serde_json::json!({
                            "entity": "kitchen light",
                            "action": "set_brightness",
                            "value": 30
                        }),
                    },
                    ctx(),
                )
                .await
                .success
        );
        assert_eq!(provider.brightness(), Some(77));

        let undo = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_undo".into(),
                    arguments: serde_json::json!({}),
                },
                ctx(),
            )
            .await;

        assert!(undo.success);
        assert!(undo.output.contains("Undid the last home action"));
        assert_eq!(
            *executed.lock().unwrap(),
            vec![
                HomeActionKind::TurnOn,
                HomeActionKind::SetBrightness,
                HomeActionKind::SetBrightness,
            ]
        );
        assert_eq!(provider.power(), "on");
        assert_eq!(provider.brightness(), Some(255));
    }

    #[tokio::test]
    async fn home_undo_restores_prior_state_after_toggle() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingHomeProvider::new(executed.clone()));
        let dispatcher = ToolDispatcher::new(Some(provider.clone()));
        let ctx = || ToolExecutionContext {
            request_origin: RequestOrigin::Dashboard,
            ..ToolExecutionContext::default()
        };

        assert!(
            dispatcher
                .execute_with_context(
                    &ToolCall {
                        name: "home_control".into(),
                        arguments: serde_json::json!({
                            "entity": "kitchen light",
                            "action": "turn_on"
                        }),
                    },
                    ctx(),
                )
                .await
                .success
        );
        assert_eq!(provider.power(), "on");

        assert!(
            dispatcher
                .execute_with_context(
                    &ToolCall {
                        name: "home_control".into(),
                        arguments: serde_json::json!({
                            "entity": "kitchen light",
                            "action": "toggle"
                        }),
                    },
                    ctx(),
                )
                .await
                .success
        );
        assert_eq!(provider.power(), "off");

        let undo = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_undo".into(),
                    arguments: serde_json::json!({}),
                },
                ctx(),
            )
            .await;

        assert!(undo.success);
        assert!(undo.output.contains("Undid the last home action"));
        assert_eq!(
            *executed.lock().unwrap(),
            vec![
                HomeActionKind::TurnOn,
                HomeActionKind::Toggle,
                HomeActionKind::TurnOn,
            ]
        );
        assert_eq!(provider.power(), "on");
    }

    #[tokio::test]
    async fn action_history_hydrates_from_audit_log() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-dispatch-audit-test-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(Arc::new(
            std::sync::Mutex::new(Vec::new()),
        )))))
        .with_actuation_audit_path(path.clone());
        assert!(
            dispatcher
                .execute_with_context(
                    &ToolCall {
                        name: "home_control".into(),
                        arguments: serde_json::json!({
                            "entity": "kitchen light",
                            "action": "turn_on"
                        }),
                    },
                    ToolExecutionContext {
                        request_origin: RequestOrigin::Dashboard,
                        ..ToolExecutionContext::default()
                    },
                )
                .await
                .success
        );

        let restarted = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider::new(Arc::new(
            std::sync::Mutex::new(Vec::new()),
        )))))
        .with_actuation_audit_path(path.clone());
        let history = restarted
            .execute(&ToolCall {
                name: "action_history".into(),
                arguments: serde_json::json!({}),
            })
            .await;

        assert!(history.success);
        assert!(history.output.contains("turn_on kitchen light"));
        assert!(history.output.contains("undo: turn_off"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tool_defs_include_loaded_skills() {
        let dispatcher = ToolDispatcher::new(None).with_skill_loader(sample_skill_loader());
        let defs = dispatcher.tool_defs();

        assert!(defs.iter().any(|d| d.name == "hello_world"));
        let hello = defs.iter().find(|d| d.name == "hello_world").unwrap();
        assert!(
            hello
                .description
                .contains("Only use when the user explicitly asks")
        );
    }

    #[tokio::test]
    async fn execute_loaded_skill() {
        let dispatcher = ToolDispatcher::new(None).with_skill_loader(sample_skill_loader());
        let call = ToolCall {
            name: "hello_world".into(),
            arguments: serde_json::json!({"name": "Jared"}),
        };

        let result = dispatcher.execute(&call).await;
        assert!(result.success);
        assert!(result.output.contains("Jared"));
        assert!(result.output.contains("loadable skill module"));
    }

    #[test]
    fn memory_store_normalizes_name_facts() {
        let db = std::env::temp_dir().join(format!("memory-store-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let result = dispatcher
            .exec_memory_store(&serde_json::json!({
                "content": "my name is Jared",
                "category": "identity"
            }))
            .unwrap();

        assert!(result.to_lowercase().contains("remember"));

        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        let results = mem.search("name", 5).unwrap();
        assert!(results.iter().any(|entry| entry.content.contains("Jared")));
    }

    #[test]
    fn memory_store_updates_changed_name() {
        let db = std::env::temp_dir().join(format!(
            "memory-store-update-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory.store("identity", "User's name is Jared").unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let result = dispatcher
            .exec_memory_store(&serde_json::json!({
                "content": "my name is Alice",
                "category": "identity"
            }))
            .unwrap();

        assert!(result.to_lowercase().contains("updated"));

        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        let results = mem.get_by_kind("identity", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Alice"));
    }

    #[test]
    fn memory_store_adds_shopping_list_items_with_count() {
        let db = std::env::temp_dir().join(format!(
            "memory-store-shopping-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let result = dispatcher
            .exec_memory_store(&serde_json::json!({
                "content": "shopping list pending: milk, eggs",
                "category": "shopping"
            }))
            .unwrap();

        assert!(result.contains("Added milk, eggs"));
        assert!(result.contains("2 item"));

        {
            let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
            assert_eq!(mem.shopping_list_pending_count().unwrap(), 2);
        }

        let result = dispatcher
            .exec_memory_store(&serde_json::json!({
                "content": "shopping list removed: milk",
                "category": "shopping"
            }))
            .unwrap();
        assert!(result.contains("Removed milk"));
        assert!(result.contains("1 item"));

        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        assert_eq!(mem.shopping_list_pending_count().unwrap(), 1);
    }

    #[test]
    fn memory_store_rejects_high_risk_secret() {
        let db = std::env::temp_dir().join(format!(
            "memory-store-secret-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let result = dispatcher
            .exec_memory_store(&serde_json::json!({
                "content": "remember that my password is swordfish",
                "category": "fact"
            }))
            .unwrap();

        assert!(result.contains("should not store passwords"));

        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        assert!(mem.search("password", 5).unwrap().is_empty());
    }

    #[test]
    fn memory_store_rejects_household_access_code() {
        let db = std::env::temp_dir().join(format!(
            "memory-store-access-code-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let result = dispatcher
            .exec_memory_store(&serde_json::json!({
                "content": "remember that the gate code is 5829",
                "category": "fact"
            }))
            .unwrap();

        assert!(result.contains("should not store household access codes"));

        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        assert!(mem.search("gate", 5).unwrap().is_empty());
    }

    #[test]
    fn memory_recall_formats_name_answers_naturally() {
        let db = std::env::temp_dir().join(format!("memory-recall-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory.store("identity", "User's name is Jared").unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "did you remember my name"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert_eq!(output, "Your name is Jared");
    }

    #[test]
    fn memory_recall_accepts_topic_alias_after_schema_validation() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-topic-alias-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory.store("preference", "User likes jazz music").unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"topic": "jazz"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.contains("jazz"));
    }

    #[test]
    fn memory_recall_answers_household_role_from_structured_profile() {
        let db =
            std::env::temp_dir().join(format!("memory-recall-role-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory.store("relationship", "Jared is the dad").unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "who is the father in this house"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert_eq!(output, "Jared is the dad.");
    }

    #[test]
    fn memory_recall_answers_structured_household_rule() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-structured-rule-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("fact", "Leo is not allowed to play video games after 8 PM")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "is Leo allowed to play video games after 8 PM"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.starts_with("No."));
        assert!(output.contains("Leo is not allowed"));
    }

    #[test]
    fn memory_recall_answers_calendar_and_access_permission() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-calendar-access-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store(
                "calendar",
                "Mia has piano lessons today at 4:00 PM with Mrs. Higgins",
            )
            .unwrap();
        memory
            .store(
                "access_permission",
                "Leo is not authorized to unlock the front door. He can only unlock the side door",
            )
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let calendar = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "does Mia have piano lessons today"}),
                ToolExecutionContext::default(),
            )
            .unwrap();
        assert!(calendar.contains("Mia"));
        assert!(calendar.contains("piano"));

        let access = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "can Leo unlock the front door"}),
                ToolExecutionContext::default(),
            )
            .unwrap();
        assert!(access.starts_with("No."));
        assert!(access.contains("front door"));
    }

    #[test]
    fn memory_recall_answers_typed_household_note() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-household-note-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("note", "Bike lock hangs on the garage hook")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "find my note about bicycle lock"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.contains("garage hook"));
    }

    #[test]
    fn memory_recall_answers_app_only_secret_reference_without_value() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-secret-ref-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store(
                "credential_reference",
                "Guest Wi-Fi password is stored in credential:guest_wifi",
            )
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "what is our wifi password for guests"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.contains("app-only reference"));
        assert!(!output.contains("credential:guest_wifi"));
    }

    #[test]
    fn memory_recall_answers_semantic_home_comfort_query() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-semantic-comfort-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store(
                "preference",
                "Jared prefers the living room thermostat at 72F.",
            )
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "I'm feeling cold"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.contains("thermostat"));
        assert!(output.contains("72F"));
    }

    #[test]
    fn memory_recall_answers_semantic_lunchbox_query() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-semantic-lunchbox-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store(
                "shopping",
                "Leo's lunchbox snacks include granola bars and fruit snacks.",
            )
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "We need more snacks for Leo's lunchbox"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.contains("granola bars"));
        assert!(output.contains("fruit snacks"));
    }

    #[test]
    fn memory_recall_answers_semantic_movie_query() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-semantic-movie-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store(
                "note",
                "Watched The Iron Giant with the kids - they loved it.",
            )
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "what was the movie about a robot that wanted to be a real boy"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(output.contains("Iron Giant"));
    }

    #[test]
    fn play_media_resolves_playlist_from_memory() {
        let db = std::env::temp_dir().join(format!(
            "media-profile-dispatch-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store(
                "media_profile",
                "Jared's Morning Boost playlist is spotify:playlist:morning_boost",
            )
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let resolved = dispatcher.resolve_media_query("play my Morning Boost playlist");

        assert_eq!(resolved.query, "Morning Boost");
        assert_eq!(resolved.provider.as_deref(), Some("spotify"));
        assert_eq!(
            resolved.target.as_deref(),
            Some("spotify:playlist:morning_boost")
        );
        assert_eq!(
            resolved.display(),
            "Morning Boost (spotify:playlist:morning_boost)"
        );
    }

    #[test]
    fn memory_recall_hides_person_memory_in_shared_room_context() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-shared-room-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({"query": "oat milk"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert_eq!(output, "I don't remember anything about oat milk yet.");
    }

    #[test]
    fn memory_recall_ignores_llm_supplied_identity_fields() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-identity-bypass-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({
                    "query": "oat milk",
                    "identity_confidence": "high",
                    "explicit_named_person": true
                }),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert_eq!(
            output, "I don't remember anything about oat milk yet.",
            "LLM-injected identity fields must not unlock person-scoped recall"
        );
    }

    #[test]
    fn memory_recall_allows_person_scope_with_verified_context() {
        // Positive counterpart to memory_recall_ignores_llm_supplied_identity_fields
        // (and mirror of memory_forget_allows_person_scope_with_verified_context):
        // person-scoped recall still works when the voice pipeline sets a trusted
        // MemoryReadContext on exec_ctx — injected tool-argument identity fields
        // must not matter either way.
        let db = std::env::temp_dir().join(format!(
            "memory-recall-verified-context-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_recall(
                &serde_json::json!({
                    "query": "oat milk",
                    "identity_confidence": "high",
                    "explicit_named_person": true
                }),
                ToolExecutionContext {
                    memory_read_context: Some(crate::memory::policy::MemoryReadContext {
                        identity_confidence: crate::memory::policy::IdentityConfidence::High,
                        explicit_named_person: true,
                        explicit_private_intent: false,
                        shared_space_voice: true,
                    }),
                    ..ToolExecutionContext::default()
                },
            )
            .unwrap();

        assert_eq!(
            output, "I remember: Maya likes oat milk",
            "verified exec_ctx.memory_read_context must unlock person-scoped recall"
        );
    }

    #[test]
    fn memory_forget_blocks_person_scope_without_verified_context() {
        // Regression for the delete-side analogue of be4a2da (PR #201): without a
        // verified MemoryReadContext, the LLM must not be able to destroy
        // person-scoped rows it cannot read. memory_forget previously called
        // Memory::delete_matching directly (scope-blind), so an LLM that could
        // not READ Maya's person_preference could still DELETE it.
        let db = std::env::temp_dir().join(format!(
            "memory-forget-shared-room-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_forget(
                &serde_json::json!({"query": "Maya"}),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert!(
            output.contains("No memories"),
            "shared-room delete must report no-match, got: {output}"
        );
        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        let still_there = mem.search("Maya", 5).unwrap();
        assert_eq!(
            still_there.len(),
            1,
            "person-scoped row must remain after a shared-room forget"
        );
    }

    #[test]
    fn memory_forget_allows_person_scope_with_verified_context() {
        // Mirror of the read-side identity-context unlock: when the server /
        // voice pipeline has set a verified MemoryReadContext on exec_ctx,
        // memory_forget should be able to delete person-scoped rows.
        let db = std::env::temp_dir().join(format!(
            "memory-forget-identity-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher
            .exec_memory_forget(
                &serde_json::json!({"query": "Maya"}),
                ToolExecutionContext {
                    memory_read_context: Some(crate::memory::policy::MemoryReadContext {
                        identity_confidence: crate::memory::policy::IdentityConfidence::High,
                        explicit_named_person: false,
                        explicit_private_intent: false,
                        shared_space_voice: true,
                    }),
                    ..ToolExecutionContext::default()
                },
            )
            .unwrap();

        assert!(
            output.contains("Forgot 1"),
            "verified-context delete must report success, got: {output}"
        );
        let mem = dispatcher.memory.as_ref().unwrap().lock().unwrap();
        assert!(
            mem.search("Maya", 5).unwrap().is_empty(),
            "person-scoped row must be deleted under a verified context"
        );
    }

    #[tokio::test]
    async fn execute_with_context_allows_person_memory_recall() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-exec-ctx-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let call = ToolCall {
            name: "memory_recall".into(),
            arguments: serde_json::json!({"query": "oat milk"}),
        };
        let output = dispatcher
            .execute_with_context(
                &call,
                ToolExecutionContext {
                    memory_read_context: Some(crate::memory::policy::MemoryReadContext {
                        identity_confidence: crate::memory::policy::IdentityConfidence::High,
                        explicit_named_person: false,
                        explicit_private_intent: false,
                        shared_space_voice: true,
                    }),
                    request_origin: RequestOrigin::Dashboard,
                    confirmed: false,
                },
            )
            .await;

        assert!(output.success);
        assert_eq!(output.output, "I remember: Maya likes oat milk");
    }

    #[test]
    fn memory_status_reports_health() {
        static MEMORY_STATUS_COUNTER: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "memory-status-test-{}-{}",
            std::process::id(),
            MEMORY_STATUS_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("memory.db");
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory.store("fact", "GenieClaw has local memory").unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher.exec_memory_status().unwrap();

        assert!(output.contains("Memory status: ok"));
        assert!(output.contains("Rows: 1"));
        assert!(output.contains("FTS consistent: yes"));
        assert!(output.contains("Migration degraded: no"));
        assert!(output.contains("Canonical root:"));
        assert!(output.contains("Daily notes: 1"));
        assert!(output.contains("Event logs: 1"));
        assert!(output.contains("Person-scoped memories: 0"));
        assert!(output.contains("Private memories: 0"));
        assert!(output.contains("Restricted memories: 0"));
    }

    /// `HomeAutomationProvider` that resolves every target as a sensitive
    /// lock (`voice_safe = false`, domain = "lock"). Used by the confirmation
    /// regression tests below — any action against this provider trips the
    /// confirmation policy gate, which is what we need to exercise the
    /// `confirm_pending_home_action` re-entry path.
    struct SensitiveHomeProvider {
        executed: Arc<std::sync::Mutex<Vec<HomeActionKind>>>,
    }

    #[async_trait::async_trait]
    impl HomeAutomationProvider for SensitiveHomeProvider {
        async fn health(&self) -> IntegrationHealth {
            IntegrationHealth {
                connected: true,
                cached_graph: true,
                message: "ok".into(),
            }
        }

        async fn sync_structure(&self) -> Result<HomeGraph> {
            anyhow::bail!("not used in test")
        }

        async fn resolve_target(
            &self,
            query: &str,
            _action_hint: Option<HomeActionKind>,
        ) -> Result<HomeTarget> {
            Ok(HomeTarget {
                kind: HomeTargetKind::Entity,
                query: query.into(),
                display_name: query.into(),
                entity_ids: vec!["lock.front_door".into()],
                domain: Some("lock".into()),
                area: Some("Entry".into()),
                confidence: 0.96,
                voice_safe: false,
            })
        }

        async fn get_state(&self, target: &HomeTarget) -> Result<HomeState> {
            Ok(HomeState {
                target_name: target.display_name.clone(),
                domain: target.domain.clone(),
                area: target.area.clone(),
                entities: Vec::new(),
                available: true,
                spoken_summary: format!("{} is available", target.display_name),
            })
        }

        async fn execute(&self, action: HomeAction) -> Result<ActionResult> {
            self.executed.lock().unwrap().push(action.kind);
            Ok(ActionResult {
                success: true,
                spoken_summary: format!("Executed {:?}", action.kind),
                affected_targets: vec![action.target.display_name],
                state_snapshot: None,
                confidence: Some(action.target.confidence),
            })
        }

        async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
            Ok(Vec::new())
        }

        async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
            Ok(Vec::new())
        }
    }

    /// Fetch the freshest pending confirmation token from the dispatcher.
    ///
    /// The token is deliberately NOT echoed into `home_control`'s tool output
    /// (it is a bearer secret), so tests read it from the same channel the
    /// dashboard uses — the pending-confirmations list — rather than scraping
    /// the LLM-visible string.
    fn latest_confirmation_token(dispatcher: &ToolDispatcher) -> String {
        dispatcher
            .pending_confirmations()
            .into_iter()
            .max_by_key(|item| item.created_ms)
            .map(|item| item.token)
            .expect("a pending confirmation must exist")
    }

    /// Read the actuation audit JSONL written via `with_actuation_audit_path`
    /// and return every event for which the predicate returns true.
    fn audit_events_matching<P>(path: &Path, mut predicate: P) -> Vec<serde_json::Value>
    where
        P: FnMut(&serde_json::Value) -> bool,
    {
        let content = std::fs::read_to_string(path).expect("read audit log");
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| predicate(event))
            .collect()
    }

    /// Regression for the bug fixed in `confirm_pending_home_action`:
    /// after confirmation, the executed `AuditEvent` must carry the channel
    /// that ORIGINALLY requested the action, not a synthetic `Confirmation`
    /// origin. Otherwise "who unlocked the door?" can never be answered from
    /// the audit log.
    #[tokio::test]
    async fn confirm_preserves_original_origin_in_audit_log() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let audit_path = std::env::temp_dir().join(format!(
            "geniepod-dispatch-confirm-origin-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&audit_path);
        let dispatcher = ToolDispatcher::new(Some(Arc::new(SensitiveHomeProvider {
            executed: executed.clone(),
        })))
        .with_actuation_audit_path(audit_path.clone());

        let lock_call = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": "front door",
                "action": "lock"
            }),
        };
        let issued = dispatcher
            .execute_with_context(
                &lock_call,
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        assert!(issued.success, "issuing confirmation must succeed");
        assert!(
            !issued.output.contains("act-"),
            "raw bearer token must not be echoed into tool output: {:?}",
            issued.output
        );
        let token = latest_confirmation_token(&dispatcher);
        let executed_output = dispatcher
            .confirm_pending_home_action(&token)
            .await
            .expect("confirm should succeed");
        assert!(executed_output.contains("Executed"));
        assert_eq!(executed.lock().unwrap().len(), 1, "exactly one HA execute");

        let executed_rows = audit_events_matching(&audit_path, |event| {
            event["status"].as_str() == Some("executed")
        });
        assert_eq!(executed_rows.len(), 1, "exactly one executed audit row");
        assert_eq!(
            executed_rows[0]["origin"].as_str(),
            Some("telegram"),
            "executed audit row must keep the original origin, not 'confirmation'"
        );
    }

    /// Regression: per-origin rate limit must not be bypassed by funnelling
    /// sensitive actions through the confirmation flow. If an operator sets
    /// `max_actions_per_minute_by_origin = { telegram = 1 }`, the second
    /// Telegram-initiated sensitive request — even routed through
    /// `confirm_pending_home_action` — must be rejected.
    #[tokio::test]
    async fn confirm_does_not_bypass_per_origin_rate_limit() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut safety = ActuationSafetyConfig::default();
        safety
            .max_actions_per_minute_by_origin
            .insert("telegram".into(), 1);
        let dispatcher = ToolDispatcher::new(Some(Arc::new(SensitiveHomeProvider {
            executed: executed.clone(),
        })))
        .with_actuation_safety_config(safety);

        let lock_call = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": "front door",
                "action": "lock"
            }),
        };

        // First Telegram request → returns ConfirmationRequired and charges
        // the telegram bucket once.
        let first_issue = dispatcher
            .execute_with_context(
                &lock_call,
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        assert!(first_issue.success);
        let first_token = latest_confirmation_token(&dispatcher);

        // Confirming that first request must succeed: the bucket already paid
        // its single slot on the request, the confirmation must not double-
        // charge, so the configured `telegram = 1` lets exactly this through.
        let first_confirm = dispatcher.confirm_pending_home_action(&first_token).await;
        assert!(
            first_confirm.is_ok(),
            "first confirmed action under telegram=1 must succeed (got {:?})",
            first_confirm.err()
        );

        // A second Telegram-initiated sensitive request inside the same window
        // must now be rate-limited at the issue step, instead of getting
        // through by routing through the confirmation bucket.
        let second_issue = dispatcher
            .execute_with_context(
                &lock_call,
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        assert!(
            !second_issue.success,
            "second telegram-initiated sensitive request must be rate-limited"
        );
        assert!(
            second_issue.output.to_lowercase().contains("rate limit"),
            "expected a rate-limit message, got: {:?}",
            second_issue.output
        );
        assert_eq!(
            executed.lock().unwrap().len(),
            1,
            "only the confirmed first action may reach the HA provider"
        );
    }

    /// Regression: when the original request already pushed one slot into
    /// the origin's rate-limit bucket (on the `ConfirmationRequired` path),
    /// the confirmation re-entry must not push a second slot for the same
    /// logical action.
    #[tokio::test]
    async fn confirm_does_not_double_charge_when_already_paid() {
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut safety = ActuationSafetyConfig::default();
        // Capacity for exactly two telegram actions in the window. If the
        // confirmation re-entry double-charges, the third issue below would
        // hit the limit. If it does NOT, the third issue still has budget.
        safety
            .max_actions_per_minute_by_origin
            .insert("telegram".into(), 2);
        let dispatcher = ToolDispatcher::new(Some(Arc::new(SensitiveHomeProvider {
            executed: executed.clone(),
        })))
        .with_actuation_safety_config(safety);

        let lock_call = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": "front door",
                "action": "lock"
            }),
        };

        let first_issue = dispatcher
            .execute_with_context(
                &lock_call,
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        assert!(first_issue.success);
        let first_token = latest_confirmation_token(&dispatcher);
        dispatcher
            .confirm_pending_home_action(&first_token)
            .await
            .expect("confirm should succeed");

        // Second telegram-initiated sensitive request. Telegram bucket usage
        // so far: 1 (request) + 0 (confirm doesn't recharge) = 1. With limit
        // = 2, this issue must still be accepted (returns
        // ConfirmationRequired again, charging slot #2).
        let second_issue = dispatcher
            .execute_with_context(
                &lock_call,
                ToolExecutionContext {
                    request_origin: RequestOrigin::Telegram,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        assert!(
            second_issue.success,
            "second issue must succeed: confirm of #1 must not double-charge the telegram bucket"
        );
    }
}
