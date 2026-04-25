use anyhow::Result;
use genie_common::config::{ActuationSafetyConfig, WebSearchConfig, WebSearchProvider};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::actuation::{
    ActionLedger, AuditEvent, AuditLogger, AuditStatus, ConfirmationManager, PendingConfirmation,
    RecordedAction, RequestOrigin, now_ms,
};
use super::home;
use super::timer;
use crate::ha::HomeAutomationProvider;
use crate::skills::SkillLoader;

const ACTUATION_RATE_WINDOW_MS: u64 = 60_000;

/// Tool definition for LLM function calling.
///
/// These are sent to llama.cpp as part of the system prompt or
/// via the `tools` parameter (OpenAI function-calling format).
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
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ToolExecutionContext {
    pub memory_read_context: Option<crate::memory::policy::MemoryReadContext>,
    pub request_origin: RequestOrigin,
    pub confirmed: bool,
}

/// LLM-generated tool call (parsed from model output).
/// Accepts both `{"tool": "..."}` and `{"name": "..."}` formats.
#[derive(Debug, Deserialize)]
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
    actuation_safety: ActuationSafetyConfig,
    confirmations: Arc<ConfirmationManager>,
    action_ledger: Arc<ActionLedger>,
    actuation_rate_limiter: Arc<ActuationRateLimiter>,
    audit_logger: AuditLogger,
    pub(crate) timers: timer::TimerManager,
}

#[derive(Debug, Default)]
struct ActuationRateLimiter {
    attempts: Mutex<HashMap<RequestOrigin, VecDeque<u64>>>,
}

impl ToolDispatcher {
    pub fn new(ha: Option<Arc<dyn HomeAutomationProvider>>) -> Self {
        Self {
            ha,
            memory: None,
            skills: None,
            web_search: WebSearchConfig::default(),
            actuation_safety: ActuationSafetyConfig::default(),
            confirmations: Arc::new(ConfirmationManager::default()),
            action_ledger: Arc::new(ActionLedger::default()),
            actuation_rate_limiter: Arc::new(ActuationRateLimiter::default()),
            audit_logger: AuditLogger::disabled(),
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
        let (loaded_skills, skill_manifests) = self
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
                    (loader.loaded().len(), loaded)
                })
            })
            .unwrap_or_else(|| (0, Vec::new()));

        serde_json::json!({
            "home_automation": {
                "available": self.has_home_automation(),
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
            "skills": {
                "loader_attached": self.skills.is_some(),
                "loaded_count": loaded_skills,
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
            description: "Explicitly store a safe household fact or preference. Use when the user says 'remember that...' or asks you to save something. Do not store passwords, one-time codes, payment details, keys, tokens, or private secrets.".into(),
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
            "memory_forget" => self.exec_memory_forget(&call.arguments),
            "memory_store" => self.exec_memory_store(&call.arguments),
            other => self.exec_skill(other, &call.arguments),
        };

        match result {
            Ok(output) => ToolResult {
                tool: call.name.clone(),
                success: true,
                output,
            },
            Err(e) => ToolResult {
                tool: call.name.clone(),
                success: false,
                output: e.to_string(),
            },
        }
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
        let entity_name = args.get("entity").and_then(|v| v.as_str()).unwrap_or("");
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("toggle");
        let value = args.get("value").and_then(|v| v.as_f64());
        if !actuation_origin_allowed(&self.actuation_safety, exec_ctx.request_origin) {
            let reason = format!(
                "actuation from '{}' is not allowed by channel policy",
                exec_ctx.request_origin.as_policy_key()
            );
            self.audit_logger.append(AuditEvent {
                ts_ms: now_ms(),
                status: AuditStatus::BlockedPolicy,
                origin: exec_ctx.request_origin,
                entity: entity_name.to_string(),
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
        if let Err(err) = self
            .actuation_rate_limiter
            .check_and_record(&self.actuation_safety, exec_ctx.request_origin)
        {
            let reason = err.to_string();
            self.audit_logger.append(AuditEvent {
                ts_ms: now_ms(),
                status: AuditStatus::BlockedRuntime,
                origin: exec_ctx.request_origin,
                entity: entity_name.to_string(),
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
        match home::control(
            ha.as_ref(),
            entity_name,
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
                        entity_name,
                        action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                    )
                } else {
                    self.action_ledger.record(
                        entity_name,
                        action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                    )
                };
                self.audit_logger.append(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::Executed,
                    origin: exec_ctx.request_origin,
                    entity: entity_name.to_string(),
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
                let pending = self.confirmations.issue(
                    entity_name,
                    action,
                    value,
                    &reason,
                    exec_ctx.request_origin,
                );
                self.audit_logger.append(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::ConfirmationIssued,
                    origin: exec_ctx.request_origin,
                    entity: entity_name.to_string(),
                    action: action.to_string(),
                    value,
                    reason: reason.clone(),
                    token: Some(pending.token.clone()),
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                });
                Ok(format!(
                    "Confirmation required before I can do that: {}. Pending token: {}. Confirm from the local dashboard or POST /api/actuation/confirm.",
                    reason, pending.token
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
                self.audit_logger.append(AuditEvent {
                    ts_ms: now_ms(),
                    status,
                    origin: exec_ctx.request_origin,
                    entity: entity_name.to_string(),
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
        let inverse = action
            .inverse_action
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?;
        let args = serde_json::json!({
            "entity": action.entity.clone(),
            "action": inverse,
        });
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
        let result = self
            .execute_with_context(
                &call,
                ToolExecutionContext {
                    request_origin: RequestOrigin::Confirmation,
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
        let entity_name = args.get("entity").and_then(|v| v.as_str()).unwrap_or("");

        home::status(ha.as_ref(), entity_name).await
    }

    fn exec_set_timer(&self, args: &serde_json::Value) -> Result<String> {
        let seconds = args.get("seconds").and_then(|v| v.as_u64()).unwrap_or(60);
        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("timer");
        self.timers.set(seconds, label);
        Ok(format!("Timer set for {} seconds: {}", seconds, label))
    }

    fn exec_memory_recall(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let query = memory_query(args);
        let read_context = exec_ctx
            .memory_read_context
            .unwrap_or_else(|| memory_read_context(args));

        let results = crate::memory::recall::recall_with_context(&mem, query, 10, read_context)?;
        if results.is_empty() {
            return Ok(match query {
                "name" => "I don't remember your name yet.".to_string(),
                "user" => "I don't remember anything about you yet.".to_string(),
                other => format!("I don't remember anything about {} yet.", other),
            });
        }

        if query == "name"
            && let Some(entry) = results
                .iter()
                .find(|entry| entry.entry.content.to_lowercase().contains("name is "))
        {
            return Ok(entry
                .entry
                .content
                .replace("User's name is ", "Your name is "));
        }

        if query == "user" || query == "me" {
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
        let state = if health.quick_check_ok && health.fts_consistent {
            "ok"
        } else {
            "degraded"
        };

        Ok(format!(
            "Memory status: {}. Rows: {}. FTS rows: {}. FTS consistent: {}. Promoted memories: {}. Canonical root: {}. Daily notes: {}. Event logs: {}. Person-scoped memories: {}. Private memories: {}. Restricted memories: {}.",
            state,
            health.memory_rows,
            health.fts_rows,
            if health.fts_consistent { "yes" } else { "no" },
            promoted,
            if health.canonical_root_exists {
                "present"
            } else {
                "missing"
            },
            health.canonical_daily_files,
            health.canonical_event_logs,
            health.person_rows,
            health.private_rows,
            health.restricted_rows,
        ))
    }

    fn exec_memory_forget(&self, args: &serde_json::Value) -> Result<String> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        if query.is_empty() {
            return Ok("Please specify what to forget.".to_string());
        }

        let deleted = mem.delete_matching(query)?;
        if deleted == 0 {
            Ok(format!("No memories found matching '{}'.", query))
        } else {
            Ok(format!("Forgot {} memory(ies) about '{}'.", deleted, query))
        }
    }

    fn exec_memory_store(&self, args: &serde_json::Value) -> Result<String> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let memories = normalize_memories_to_store(args);
        if memories.is_empty() {
            return Ok("Please specify what to remember.".to_string());
        }

        let mut stored = Vec::new();
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
            stored.push(content);
        }

        if stored.is_empty() {
            return Ok(rejected
                .first()
                .copied()
                .unwrap_or("I could not store that memory.")
                .to_string());
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

    fn exec_skill(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        let skills = self
            .skills
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
        let mut loader = skills
            .lock()
            .map_err(|e| anyhow::anyhow!("skill loader lock: {}", e))?;

        let args_json = serde_json::to_string(args)?;
        let (success, output) = {
            let skill = loader
                .get_mut(name)
                .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
            skill.execute_parsed(&args_json)
        };

        let pruned = loader.prune_faulted();
        if pruned.iter().any(|skill_name| skill_name == name) {
            tracing::warn!(skill = name, "skill auto-unloaded after repeated faults");
        }

        if success {
            Ok(output)
        } else {
            Err(anyhow::anyhow!("{}", output))
        }
    }

    async fn exec_play_media(&self, args: &serde_json::Value) -> Result<String> {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        tracing::info!(query, "triggering media mode via governor control socket");

        // Send media_start command to the governor via its Unix control socket.
        let response = governor_command(r#"{"cmd":"media_start"}"#).await;

        match response {
            Some(resp) => {
                let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    Ok(format!(
                        "Playing: {}. Switched to media mode — LLM unloaded, HDMI ready.",
                        query
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
                    query
                ))
            }
        }
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

fn memory_query(args: &serde_json::Value) -> &str {
    let raw = args
        .get("query")
        .or_else(|| args.get("topic"))
        .or_else(|| args.get("what"))
        .and_then(|v| v.as_str())
        .unwrap_or("user");

    let lower = raw.to_lowercase();
    if lower.contains("my name") || lower == "name" || lower.contains("who am i") {
        "name"
    } else if lower.contains("about me") || lower == "me" || lower == "user" {
        "user"
    } else {
        raw
    }
}

fn memory_read_context(args: &serde_json::Value) -> crate::memory::policy::MemoryReadContext {
    let identity_confidence = match args
        .get("identity_confidence")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
        .as_str()
    {
        "high" => crate::memory::policy::IdentityConfidence::High,
        "medium" => crate::memory::policy::IdentityConfidence::Medium,
        "low" => crate::memory::policy::IdentityConfidence::Low,
        _ => crate::memory::policy::IdentityConfidence::Unknown,
    };

    crate::memory::policy::MemoryReadContext {
        identity_confidence,
        explicit_named_person: args
            .get("explicit_named_person")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        explicit_private_intent: args
            .get("explicit_private_intent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        shared_space_voice: args
            .get("shared_space_voice")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    }
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

fn exec_calculate(args: &serde_json::Value) -> Result<String> {
    let expr = args
        .get("expression")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
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
    let location = args
        .get("location")
        .and_then(|v| v.as_str())
        .unwrap_or("Denver");
    let forecast = args
        .get("forecast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if forecast {
        super::weather::get_forecast(location).await
    } else {
        super::weather::get_weather(location).await
    }
}

async fn exec_web_search(args: &serde_json::Value, config: &WebSearchConfig) -> Result<String> {
    let query = args
        .get("query")
        .or_else(|| args.get("q"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(3)
        .clamp(1, 5) as usize;
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
    async fn execute_get_time() {
        let dispatcher = ToolDispatcher::new(None);
        let call = ToolCall {
            name: "get_time".into(),
            arguments: serde_json::json!({}),
        };
        let result = dispatcher.execute(&call).await;
        assert!(result.success);
        assert!(!result.output.is_empty());
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
        assert!(output.contains("Canonical root:"));
        assert!(output.contains("Daily notes: 1"));
        assert!(output.contains("Event logs: 1"));
        assert!(output.contains("Person-scoped memories: 0"));
        assert!(output.contains("Private memories: 0"));
        assert!(output.contains("Restricted memories: 0"));
    }
}
