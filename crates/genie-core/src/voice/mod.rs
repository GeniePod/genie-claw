pub mod aec;
pub mod dsp;
pub mod format;
pub mod identity;
pub mod intent;
pub mod language;
pub mod noise;
pub mod pipeline;
pub mod streaming;
pub mod stt;
pub mod tts;
pub mod vad;

use anyhow::Result;
use genie_common::config::Config;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::{Duration, interval};

use crate::llm::{self, LlmClient};
use crate::memory::Memory;
use crate::tools::{ToolDispatcher, ToolResult};

/// The main voice AI runtime.
///
/// Pipeline:
///   [Wake word detected] → STT (Whisper) → intent analysis → tool dispatch / LLM → TTS (Piper) → speaker
///
/// In the current development phase, the runtime:
/// - Accepts text input via stdin (simulating STT output)
/// - Routes to tools or LLM
/// - Prints response (simulating TTS)
///
/// Production adds: Whisper subprocess, Piper subprocess, ALSA I/O, and wake-word integration.
pub struct VoiceOrchestrator {
    llm: LlmClient,
    tools: ToolDispatcher,
    memory: Memory,
    conversation: Vec<llm::Message>,
    system_prompt: String,
}

impl VoiceOrchestrator {
    pub async fn new(config: Config) -> Result<Self> {
        let llm_url = &config.services.llm.url;
        let llm = LlmClient::from_url(llm_url);

        let ha = crate::ha::provider_from_config(&config);
        let skill_loader = crate::skills::load_all();

        let tools = ToolDispatcher::new(ha)
            .with_web_search_config(config.web_search.clone())
            .with_skill_loader(skill_loader);

        let mem_path = config.data_dir.join("memory.db");
        let memory = Memory::open(&mem_path)?;
        tracing::info!(memories = memory.count()?, "memory loaded");

        // Build system prompt with tool definitions.
        let tool_json = serde_json::to_string_pretty(&tools.tool_defs())?;
        let role_summary = if tools.has_home_automation() {
            "You help with home control, timers, questions, and useful household context."
        } else {
            "You help with timers, questions, and useful household context. Home control is currently unavailable."
        };
        let system_prompt = format!(
            "You are GeniePod Home, a local home AI for a shared living space. \
             {}\n\n\
             You have these tools available. To use a tool, respond with a JSON object:\n\
             {{\"tool\": \"tool_name\", \"arguments\": {{...}}}}\n\n\
             Available tools:\n{}\n\n\
             If the user's request doesn't need a tool, respond naturally in a concise, \
             conversational voice. Keep responses under 3 sentences for voice output. \
             Assume replies may be heard in a shared room.\n\n\
             Current household context:\n{}",
            role_summary,
            tool_json,
            format_memories(&memory),
        );

        Ok(Self {
            llm,
            tools,
            memory,
            conversation: Vec::new(),
            system_prompt,
        })
    }

    /// Main run loop.
    ///
    /// In prototype mode: reads from stdin, prints to stdout.
    /// In production: integrates with wake word, Whisper STT, Piper TTS.
    pub async fn run(&mut self) -> Result<()> {
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut timer_tick = interval(Duration::from_secs(1));

        tracing::info!("GeniePod core ready — type a message (or Ctrl+C to quit)");

        // Check LLM health.
        if self.llm.health().await {
            tracing::info!("LLM server connected");
        } else {
            tracing::warn!("LLM server not reachable — will retry on first request");
        }

        let stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut lines = tokio::io::AsyncBufReadExt::lines(stdin);

        loop {
            // Print prompt.
            eprint!("\n> ");

            tokio::select! {
                line = lines.next_line() => {
                    match line {
                        Ok(Some(text)) => {
                            let text = text.trim().to_string();
                            if text.is_empty() {
                                continue;
                            }
                            if text == "quit" || text == "exit" {
                                break;
                            }
                            self.handle_input(&text).await;
                        }
                        Ok(None) => break, // EOF
                        Err(e) => {
                            tracing::error!(error = %e, "stdin error");
                            break;
                        }
                    }
                }
                _ = timer_tick.tick() => {
                    // Check for fired timers.
                    let fired = self.tools.check_timers();
                    for label in fired {
                        println!("\n[TIMER] {} — time's up!", label);
                    }
                }
                _ = sigterm.recv() => {
                    tracing::info!("SIGTERM received");
                    break;
                }
            }
        }

        tracing::info!("GeniePod core shutting down");
        Ok(())
    }

    async fn handle_input(&mut self, text: &str) {
        // Add user message to conversation.
        self.conversation.push(llm::Message {
            role: "user".into(),
            content: text.to_string(),
        });

        // Build full message list with system prompt.
        let mut messages = vec![llm::Message {
            role: "system".into(),
            content: self.system_prompt.clone(),
        }];
        // Keep last 10 conversation turns for context.
        let history_start = self.conversation.len().saturating_sub(20);
        messages.extend_from_slice(&self.conversation[history_start..]);

        // Stream LLM response.
        let mut full_response = String::new();
        print!("\nGeniePod: ");
        match self
            .llm
            .chat_stream(&messages, Some(512), |token| {
                print!("{}", token);
            })
            .await
        {
            Ok(response) => {
                full_response = response;
                println!();
            }
            Err(e) => {
                println!("\n[ERROR] LLM: {}", e);
                return;
            }
        }

        // Check if the response contains a tool call.
        if let Some(tool_result) = self.try_tool_call(&full_response).await {
            println!("[TOOL] {}: {}", tool_result.tool, tool_result.output);

            // Add tool result to conversation and get a follow-up response.
            self.conversation.push(llm::Message {
                role: "assistant".into(),
                content: full_response.clone(),
            });
            self.conversation.push(llm::Message {
                role: "system".into(),
                content: format!("Tool result: {}", tool_result.output),
            });

            // Get natural language summary from LLM.
            let mut messages = vec![llm::Message {
                role: "system".into(),
                content: "Summarize the tool result in one natural sentence for voice output."
                    .into(),
            }];
            let history_start = self.conversation.len().saturating_sub(6);
            messages.extend_from_slice(&self.conversation[history_start..]);

            print!("GeniePod: ");
            match self
                .llm
                .chat_stream(&messages, Some(128), |token| {
                    print!("{}", token);
                })
                .await
            {
                Ok(summary) => {
                    println!();
                    self.conversation.push(llm::Message {
                        role: "assistant".into(),
                        content: summary,
                    });
                }
                Err(_) => println!(),
            }
        } else {
            // Normal conversational response — add to history.
            self.conversation.push(llm::Message {
                role: "assistant".into(),
                content: full_response,
            });
        }

        // Extract and store any memorable facts.
        self.extract_memories(text);
    }

    /// Try to parse a tool call from the LLM response.
    async fn try_tool_call(&self, response: &str) -> Option<ToolResult> {
        // Look for JSON in the response.
        let trimmed = response.trim();

        // Try parsing the whole response as a tool call.
        if let Ok(call) = serde_json::from_str::<ToolCallJson>(trimmed)
            && !call.tool.is_empty()
        {
            let tc = crate::tools::dispatch::ToolCall {
                name: call.tool,
                arguments: call.arguments,
            };
            return Some(self.tools.execute(&tc).await);
        }

        // Try finding JSON embedded in the response.
        if let Some(start) = trimmed.find('{')
            && let Some(end) = trimmed.rfind('}')
        {
            let json_str = &trimmed[start..=end];
            if let Ok(call) = serde_json::from_str::<ToolCallJson>(json_str)
                && !call.tool.is_empty()
            {
                let tc = crate::tools::dispatch::ToolCall {
                    name: call.tool,
                    arguments: call.arguments,
                };
                return Some(self.tools.execute(&tc).await);
            }
        }

        None
    }

    fn extract_memories(&self, user_text: &str) {
        let stored = crate::memory::extract::extract_and_store(&self.memory, user_text);
        if stored > 0 {
            tracing::debug!(count = stored, "auto-captured memories");
        }
    }
}

impl ToolDispatcher {
    /// Expose timer check to the orchestrator.
    pub fn check_timers(&self) -> Vec<String> {
        self.timers.check_fired()
    }
}

#[derive(serde::Deserialize)]
struct ToolCallJson {
    tool: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

fn format_memories(memory: &Memory) -> String {
    crate::memory::inject::build_memory_context(memory, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_memory() -> Memory {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-voice-memory-test-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        Memory::open(&path).unwrap()
    }

    #[test]
    fn voice_memory_context_filters_person_memory() {
        let mem = temp_memory();
        mem.store("person_preference", "Maya likes oat milk")
            .unwrap();

        let context = format_memories(&mem);
        assert_eq!(context, "(no household context yet)");
    }
}
