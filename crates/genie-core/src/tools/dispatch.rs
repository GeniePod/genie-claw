use anyhow::Result;
use genie_common::config::{WebSearchConfig, WebSearchProvider};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::home;
use super::timer;
use crate::ha::HomeAutomationProvider;
use crate::skills::SkillLoader;

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
    pub(crate) timers: timer::TimerManager,
}

impl ToolDispatcher {
    pub fn new(ha: Option<Arc<dyn HomeAutomationProvider>>) -> Self {
        Self {
            ha,
            memory: None,
            skills: None,
            web_search: WebSearchConfig::default(),
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
            "timeout_secs": self.web_search.timeout_secs,
            "max_results": self.web_search.max_results,
        })
    }

    /// Set public web search provider configuration.
    pub fn with_web_search_config(mut self, config: WebSearchConfig) -> Self {
        self.web_search = config;
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
                        "limit": {"type": "integer", "minimum": 1, "maximum": 5, "description": "Maximum number of results to return"}
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
        let result = match call.name.as_str() {
            "home_control" => self.exec_home_control(&call.arguments).await,
            "home_status" => self.exec_home_status(&call.arguments).await,
            "set_timer" => self.exec_set_timer(&call.arguments),
            "get_time" => Ok(get_current_time()),
            "get_weather" => exec_weather(&call.arguments).await,
            "web_search" => exec_web_search(&call.arguments, &self.web_search).await,
            "system_info" => super::system::system_info(self.ha.as_deref()).await,
            "calculate" => exec_calculate(&call.arguments),
            "play_media" => self.exec_play_media(&call.arguments).await,
            "memory_recall" => self.exec_memory_recall(&call.arguments),
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

    async fn exec_home_control(&self, args: &serde_json::Value) -> Result<String> {
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

        home::control(ha.as_ref(), entity_name, action, value).await
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

    fn exec_memory_recall(&self, args: &serde_json::Value) -> Result<String> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let query = memory_query(args);

        let results = mem.search(query, 10)?;
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
                .find(|entry| entry.content.to_lowercase().contains("name is "))
        {
            return Ok(entry.content.replace("User's name is ", "Your name is "));
        }

        if query == "user" || query == "me" {
            let items = results
                .iter()
                .take(3)
                .map(|entry| entry.content.clone())
                .collect::<Vec<_>>();
            return Ok(format!("I remember:\n- {}", items.join("\n- ")));
        }

        if results.len() == 1 {
            return Ok(format!("I remember: {}", results[0].content));
        }

        let items = results
            .iter()
            .map(|entry| format!("- [{}] {}", entry.kind, entry.content))
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
            "Memory status: {}. Rows: {}. FTS rows: {}. FTS consistent: {}. Promoted memories: {}.",
            state,
            health.memory_rows,
            health.fts_rows,
            if health.fts_consistent { "yes" } else { "no" },
            promoted
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

    super::web_search::search_with_config(query, limit, config).await
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
        ActionResult, DeviceRef, HomeAction, HomeAutomationProvider, HomeGraph, HomeState,
        HomeTarget, IntegrationHealth, SceneRef,
    };
    use crate::skills::SkillLoader;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubHomeProvider;

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
    }

    #[test]
    fn tool_defs_hide_web_search_when_disabled() {
        let mut web_search = WebSearchConfig::default();
        web_search.enabled = false;
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
            .exec_memory_recall(&serde_json::json!({"query": "did you remember my name"}))
            .unwrap();

        assert_eq!(output, "Your name is Jared");
    }

    #[test]
    fn memory_status_reports_health() {
        let db = std::env::temp_dir().join(format!("memory-status-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let memory = crate::memory::Memory::open(&db).unwrap();
        memory.store("fact", "GenieClaw has local memory").unwrap();
        let dispatcher =
            ToolDispatcher::new(None).with_memory(Arc::new(std::sync::Mutex::new(memory)));

        let output = dispatcher.exec_memory_status().unwrap();

        assert!(output.contains("Memory status: ok"));
        assert!(output.contains("Rows: 1"));
        assert!(output.contains("FTS consistent: yes"));
    }
}
