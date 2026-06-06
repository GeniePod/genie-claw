use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use genie_common::config::{ActuationSafetyConfig, ToolPolicyConfig, WebSearchConfig, WebSearchProvider};
use serde::Serialize;

use super::actuation::{
    ActionLedger, AuditError, AuditLogger, ConfirmationManager, PendingConfirmation,
    RecordedAction, RequestOrigin, append_json_line, now_ms,
};
use super::builtin::{
    action_history::ActionHistoryTool, calculate::CalculateTool, get_time::GetTimeTool,
    get_weather::GetWeatherTool, home_control::HomeControlTool, home_status::HomeStatusTool,
    home_undo::HomeUndoTool, memory_forget::MemoryForgetTool, memory_recall::MemoryRecallTool,
    memory_status::MemoryStatusTool, memory_store::MemoryStoreTool, play_media::PlayMediaTool,
    set_timer::SetTimerTool, skill_runner::{SkillRunnerTool, runtime_skill_description},
    system_info::SystemInfoTool, web_search::WebSearchTool,
};
use super::config::ToolDispatcherConfig;
use super::dispatch::{
    ToolActionClass, ToolCall, ToolDef, ToolEntry, ToolExecutionContext, ToolResult,
    tool_action_class, tool_argument_keys, tool_origin_allowed,
};
use super::registry::ToolRegistry;
use super::timer::TimerManager;
use crate::ha::HomeAutomationProvider;
use crate::memory::Memory;
use crate::skills::SkillLoader;

// ── Rate limiters ────────────────────────────────────────────────────────────

const ACTUATION_RATE_WINDOW_MS: u64 = 60_000;

#[derive(Debug, Default)]
pub(crate) struct ActuationRateLimiter {
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

impl ActuationRateLimiter {
    pub(crate) fn check_and_record(
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

/// Stable, non-secret token derived from the confirmation key.
fn tool_confirmation_token(key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("conf-{:016x}", hasher.finish())
}

// ── Tool audit logger ────────────────────────────────────────────────────────

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

// ── ToolDispatcher ───────────────────────────────────────────────────────────

/// Central tool dispatcher. Compiled-in tools, no plugin execution.
pub struct ToolDispatcher {
    registry: ToolRegistry,
    tool_policy: ToolPolicyConfig,
    confirmations: Arc<ConfirmationManager>,
    action_ledger: Arc<ActionLedger>,
    audit_logger: AuditLogger,
    tool_audit_logger: ToolAuditLogger,
    tool_rate_limiter: Arc<ToolRateLimiter>,
    tool_confirmations: Arc<ToolConfirmationGate>,
    pub(crate) timers: Arc<TimerManager>,
    // Kept for status/introspection methods and builder-method reconstruction
    ha: Option<Arc<dyn HomeAutomationProvider>>,
    web_search: WebSearchConfig,
    memory: Option<Arc<Mutex<Memory>>>,
    skills: Option<Arc<Mutex<SkillLoader>>>,
    actuation_safety: ActuationSafetyConfig,
    // Paths stored for builder-method reconstruction
    actuation_audit_path: Option<PathBuf>,
    tool_audit_path: Option<PathBuf>,
}

impl ToolDispatcher {
    pub fn new(ha: Option<Arc<dyn HomeAutomationProvider>>) -> Self {
        Self::from_config(ToolDispatcherConfig::new(ha))
    }

    pub fn from_config(config: ToolDispatcherConfig) -> Self {
        let confirmations = Arc::new(ConfirmationManager::default());
        let action_ledger = Arc::new(ActionLedger::default());
        let actuation_rate_limiter = Arc::new(ActuationRateLimiter::default());
        let tool_rate_limiter = Arc::new(ToolRateLimiter::default());
        let tool_confirmations = Arc::new(ToolConfirmationGate::default());

        let mut audit_logger = AuditLogger::disabled();
        if let Some(path) = &config.actuation_audit_path {
            audit_logger = AuditLogger::new(path);
            let recent = audit_logger.read_recent_executed_actions(32);
            action_ledger.hydrate(recent);
        }

        let tool_audit_logger = config
            .tool_audit_path
            .as_ref()
            .map(ToolAuditLogger::new)
            .unwrap_or_default();

        let shared_audit = Arc::new(audit_logger.clone());
        let timers = Arc::new(TimerManager::new());

        let entries = build_entries(
            &config,
            Arc::clone(&confirmations),
            Arc::clone(&action_ledger),
            Arc::clone(&actuation_rate_limiter),
            Arc::clone(&shared_audit),
            Arc::clone(&timers),
        );

        Self {
            registry: ToolRegistry::new(entries),
            tool_policy: config.tool_policy.clone(),
            confirmations,
            action_ledger,
            audit_logger: (*shared_audit).clone(),
            tool_audit_logger,
            tool_rate_limiter,
            tool_confirmations,
            timers,
            ha: config.ha,
            web_search: config.web_search,
            memory: config.memory,
            skills: config.skill_loader,
            actuation_safety: config.actuation_safety,
            actuation_audit_path: config.actuation_audit_path,
            tool_audit_path: config.tool_audit_path,
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
        super::web_search::search_response_with_options(query, limit, &self.web_search, fresh)
            .await
    }

    // ── Builder methods (kept for backward compat with server.rs tests) ──────

    pub fn with_web_search_config(self, config: WebSearchConfig) -> Self {
        Self::from_config(ToolDispatcherConfig {
            web_search: config,
            ha: self.ha,
            memory: self.memory,
            skill_loader: self.skills,
            tool_policy: self.tool_policy,
            actuation_safety: self.actuation_safety,
            actuation_audit_path: self.actuation_audit_path,
            tool_audit_path: self.tool_audit_path,
        })
    }

    pub fn with_tool_policy_config(self, config: ToolPolicyConfig) -> Self {
        Self::from_config(ToolDispatcherConfig {
            tool_policy: config,
            ha: self.ha,
            memory: self.memory,
            skill_loader: self.skills,
            web_search: self.web_search,
            actuation_safety: self.actuation_safety,
            actuation_audit_path: self.actuation_audit_path,
            tool_audit_path: self.tool_audit_path,
        })
    }

    pub fn with_actuation_safety_config(self, config: ActuationSafetyConfig) -> Self {
        Self::from_config(ToolDispatcherConfig {
            actuation_safety: config,
            ha: self.ha,
            memory: self.memory,
            skill_loader: self.skills,
            web_search: self.web_search,
            tool_policy: self.tool_policy,
            actuation_audit_path: self.actuation_audit_path,
            tool_audit_path: self.tool_audit_path,
        })
    }

    pub fn with_actuation_audit_path(self, path: PathBuf) -> Self {
        Self::from_config(ToolDispatcherConfig {
            actuation_audit_path: Some(path),
            ha: self.ha,
            memory: self.memory,
            skill_loader: self.skills,
            web_search: self.web_search,
            tool_policy: self.tool_policy,
            actuation_safety: self.actuation_safety,
            tool_audit_path: self.tool_audit_path,
        })
    }

    pub fn with_tool_audit_path(self, path: PathBuf) -> Self {
        Self::from_config(ToolDispatcherConfig {
            tool_audit_path: Some(path),
            ha: self.ha,
            memory: self.memory,
            skill_loader: self.skills,
            web_search: self.web_search,
            tool_policy: self.tool_policy,
            actuation_safety: self.actuation_safety,
            actuation_audit_path: self.actuation_audit_path,
        })
    }

    pub fn with_memory(self, memory: Arc<Mutex<Memory>>) -> Self {
        Self::from_config(ToolDispatcherConfig {
            memory: Some(memory),
            ha: self.ha,
            skill_loader: self.skills,
            web_search: self.web_search,
            tool_policy: self.tool_policy,
            actuation_safety: self.actuation_safety,
            actuation_audit_path: self.actuation_audit_path,
            tool_audit_path: self.tool_audit_path,
        })
    }

    pub fn with_skill_loader(self, skill_loader: SkillLoader) -> Self {
        Self::from_config(ToolDispatcherConfig {
            skill_loader: Some(Arc::new(Mutex::new(skill_loader))),
            ha: self.ha,
            memory: self.memory,
            web_search: self.web_search,
            tool_policy: self.tool_policy,
            actuation_safety: self.actuation_safety,
            actuation_audit_path: self.actuation_audit_path,
            tool_audit_path: self.tool_audit_path,
        })
    }

    // ── Status / query methods ───────────────────────────────────────────────

    pub fn pending_confirmations(&self) -> Vec<PendingConfirmation> {
        self.confirmations.list()
    }

    pub fn recent_home_actions(&self) -> Vec<RecordedAction> {
        self.action_ledger.list()
    }

    pub fn actuation_audit_path(&self) -> Option<&std::path::Path> {
        self.audit_logger.path()
    }

    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.registry.tool_defs()
    }

    // ── Execution ────────────────────────────────────────────────────────────

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

        let result = self.registry.dispatch(call, exec_ctx).await;

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
    /// refuses, or `None` when the call may proceed.
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

        // 2. Two-step confirmation for configured sensitive tools.
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

        // 3. Per-tool sliding-window rate limit.
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

    /// Public chokepoint entry for specialized fast-paths that need the gate's
    /// ACL / confirmation / rate-limit decision and audit trail but render their
    /// own output. Returns `Some(rejection)` when the gate refuses, or `None`
    /// when the call may proceed — callers MUST then call [`audit_gated_tool`].
    pub fn gate_tool_call(
        &self,
        call: &ToolCall,
        exec_ctx: ToolExecutionContext,
    ) -> Option<ToolResult> {
        self.run_gate(call, exec_ctx, Instant::now())
    }

    /// Record one tool-audit line for a call that passed [`gate_tool_call`].
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

    #[cfg(test)]
    pub(crate) fn exec_memory_store(&self, args: &serde_json::Value) -> Result<String> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no memory configured"))?;
        let tool = super::builtin::memory_store::MemoryStoreTool {
            memory: Arc::clone(memory),
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(tool.execute(args, ToolExecutionContext::default()))
    }

    #[cfg(test)]
    pub(crate) fn exec_memory_recall(
        &self,
        args: &serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> Result<String> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no memory configured"))?;
        let tool = super::builtin::memory_recall::MemoryRecallTool {
            memory: Arc::clone(memory),
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(tool.execute(args, ctx))
    }

    #[cfg(test)]
    pub(crate) fn exec_memory_status(&self) -> Result<String> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no memory configured"))?;
        let tool = super::builtin::memory_status::MemoryStatusTool {
            memory: Arc::clone(memory),
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(tool.execute(&serde_json::json!({}), ToolExecutionContext::default()))
    }

    #[cfg(test)]
    pub(crate) fn resolve_media_query(
        &self,
        query: &str,
    ) -> super::builtin::play_media::ResolvedMediaQuery {
        super::builtin::play_media::resolve_media_query(&self.memory, query)
    }
}

// ── Registry builder ─────────────────────────────────────────────────────────

fn build_entries(
    config: &ToolDispatcherConfig,
    confirmations: Arc<ConfirmationManager>,
    action_ledger: Arc<ActionLedger>,
    actuation_rate_limiter: Arc<ActuationRateLimiter>,
    audit_logger: Arc<AuditLogger>,
    timers: Arc<TimerManager>,
) -> Vec<Box<dyn ToolEntry>> {
    let mut entries: Vec<Box<dyn ToolEntry>> = Vec::new();

    if let Some(ha) = &config.ha {
        entries.push(Box::new(HomeControlTool {
            ha: Arc::clone(ha),
            memory: config.memory.clone(),
            actuation_safety: config.actuation_safety.clone(),
            confirmations: Arc::clone(&confirmations),
            action_ledger: Arc::clone(&action_ledger),
            actuation_rate_limiter: Arc::clone(&actuation_rate_limiter),
            audit_logger: Arc::clone(&audit_logger),
        }));
        entries.push(Box::new(HomeStatusTool {
            ha: Arc::clone(ha),
            memory: config.memory.clone(),
        }));
        entries.push(Box::new(HomeUndoTool {
            ha: Arc::clone(ha),
            memory: config.memory.clone(),
            actuation_safety: config.actuation_safety.clone(),
            confirmations: Arc::clone(&confirmations),
            action_ledger: Arc::clone(&action_ledger),
            actuation_rate_limiter: Arc::clone(&actuation_rate_limiter),
            audit_logger: Arc::clone(&audit_logger),
        }));
        entries.push(Box::new(ActionHistoryTool {
            confirmations: Arc::clone(&confirmations),
            action_ledger: Arc::clone(&action_ledger),
        }));
    }

    entries.push(Box::new(SetTimerTool { timers }));
    entries.push(Box::new(GetTimeTool));
    entries.push(Box::new(GetWeatherTool));
    entries.push(Box::new(WebSearchTool {
        config: config.web_search.clone(),
    }));
    entries.push(Box::new(SystemInfoTool {
        ha: config.ha.clone(),
    }));
    entries.push(Box::new(CalculateTool));
    entries.push(Box::new(PlayMediaTool {
        memory: config.memory.clone(),
    }));

    if let Some(memory) = &config.memory {
        entries.push(Box::new(MemoryRecallTool {
            memory: Arc::clone(memory),
        }));
        entries.push(Box::new(MemoryStatusTool {
            memory: Arc::clone(memory),
        }));
        entries.push(Box::new(MemoryForgetTool {
            memory: Arc::clone(memory),
        }));
        entries.push(Box::new(MemoryStoreTool {
            memory: Arc::clone(memory),
        }));
    }

    if let Some(skills) = &config.skill_loader {
        let loader = skills.lock().ok();
        if let Some(loader) = loader {
            for skill in loader.loaded() {
                entries.push(Box::new(SkillRunnerTool {
                    name: skill.name.clone(),
                    description: runtime_skill_description(skill),
                    parameters: serde_json::from_str(&skill.parameters_json).unwrap_or_else(
                        |_| serde_json::json!({"type": "object", "properties": {}}),
                    ),
                    skills: Arc::clone(skills),
                }));
            }
        }
    }

    entries
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::{
        ActionResult, DeviceRef, HomeAction, HomeActionKind, HomeAutomationProvider, HomeGraph,
        HomeState, HomeTarget, HomeTargetKind, IntegrationHealth, SceneRef,
    };
    use crate::skills::SkillLoader;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubHomeProvider;

    struct RecordingHomeProvider {
        executed: Arc<std::sync::Mutex<Vec<HomeActionKind>>>,
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
        let time = super::super::builtin::get_time::get_current_time();
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
        use super::super::builtin::memory_recall::household_role_query;
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
        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: executed.clone(),
        })));

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
        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: executed.clone(),
        })))
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
        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: executed.clone(),
        })));

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
        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: executed.clone(),
        })))
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
        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: executed.clone(),
        })))
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
        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: executed.clone(),
        })));

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
    async fn action_history_hydrates_from_audit_log() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-dispatch-audit-test-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let dispatcher = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: Arc::new(std::sync::Mutex::new(Vec::new())),
        })))
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

        let restarted = ToolDispatcher::new(Some(Arc::new(RecordingHomeProvider {
            executed: Arc::new(std::sync::Mutex::new(Vec::new())),
        })))
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
    fn memory_recall_can_use_identity_context_when_provided() {
        let db = std::env::temp_dir().join(format!(
            "memory-recall-identity-test-{}.db",
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
                    "identity_confidence": "high"
                }),
                ToolExecutionContext::default(),
            )
            .unwrap();

        assert_eq!(output, "I remember: Maya likes oat milk");
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
