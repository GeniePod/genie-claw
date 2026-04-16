use anyhow::Result;
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
    pub(crate) timers: timer::TimerManager,
}

impl ToolDispatcher {
    pub fn new(ha: Option<Arc<dyn HomeAutomationProvider>>) -> Self {
        Self {
            ha,
            memory: None,
            skills: None,
            timers: timer::TimerManager::new(),
        }
    }

    pub fn has_home_automation(&self) -> bool {
        self.ha.is_some()
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
                description: "Control Home Assistant devices, scenes, and voice-safe routines. Use for lights, switches, climate, covers, locks, and scene activation.".into(),
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

        defs.push(ToolDef {
            name: "system_info".into(),
            description: "Get GeniePod system status: memory, uptime, governor mode, load average."
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
            description: "Explicitly store a fact about the user. Use when the user says 'remember that...' or asks you to save something.".into(),
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
            "system_info" => super::system::system_info().await,
            "calculate" => exec_calculate(&call.arguments),
            "play_media" => self.exec_play_media(&call.arguments).await,
            "memory_recall" => self.exec_memory_recall(&call.arguments),
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
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("user");

        let results = mem.search(query, 10)?;
        if results.is_empty() {
            return Ok("I don't have any memories about that.".to_string());
        }

        let mut output = format!("Found {} memories:\n", results.len());
        for entry in &results {
            output.push_str(&format!("- [{}] {}\n", entry.kind, entry.content));
        }
        Ok(output)
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
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("fact");

        if content.is_empty() {
            return Ok("Please specify what to remember.".to_string());
        }

        mem.store(category, content)?;
        Ok(format!("Remembered: [{}] {}", category, content))
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
                    description: skill.description.clone(),
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
    }

    #[test]
    fn tool_defs_include_home_tools_when_available() {
        let dispatcher = ToolDispatcher::new(Some(Arc::new(StubHomeProvider)));
        let defs = dispatcher.tool_defs();
        assert!(defs.iter().any(|d| d.name == "home_control"));
        assert!(defs.iter().any(|d| d.name == "home_status"));
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

    #[test]
    fn tool_defs_include_loaded_skills() {
        let dispatcher = ToolDispatcher::new(None).with_skill_loader(sample_skill_loader());
        let defs = dispatcher.tool_defs();

        assert!(defs.iter().any(|d| d.name == "hello_world"));
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
}
