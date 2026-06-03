use anyhow::Result;

use crate::llm::{LlmClient, Message};
use crate::tools::ToolDispatcher;

use super::format;
use super::intent::{self, VoiceIntentDecision};
use super::stt::SttEngine;
use super::tts::TtsEngine;

/// Full voice pipeline: Audio → STT → LLM/Tools → TTS → Speaker.
///
/// This struct manages the end-to-end flow for a single voice interaction.
/// The orchestrator in mod.rs calls `process_audio()` when the wake word fires.
pub struct VoicePipeline {
    stt: SttEngine,
    tts: TtsEngine,
    llm: LlmClient,
    tools: ToolDispatcher,
    conversation: Vec<Message>,
    system_prompt: String,
    audio_device: String,
}

impl VoicePipeline {
    pub fn new(
        stt: SttEngine,
        tts: TtsEngine,
        llm: LlmClient,
        tools: ToolDispatcher,
        system_prompt: String,
    ) -> Self {
        Self {
            stt,
            tts,
            llm,
            tools,
            conversation: Vec::new(),
            system_prompt,
            audio_device: String::new(),
        }
    }

    pub fn with_audio_device(mut self, device: &str) -> Self {
        self.audio_device = device.to_string();
        self
    }

    /// Process a single voice interaction from WAV audio.
    ///
    /// 1. Transcribe audio → text (STT)
    /// 2. Route to LLM with tool dispatch
    /// 3. Format response for voice
    /// 4. Synthesize response → audio (TTS)
    /// 5. Play audio through speaker
    ///
    /// Returns the transcript and response text for logging.
    pub async fn process_audio(&mut self, wav_path: &str) -> Result<InteractionResult> {
        // Step 1: STT
        let transcript = self.stt.transcribe_file(wav_path).await?;
        tracing::info!(
            text = %transcript.text,
            duration_ms = transcript.duration_ms,
            "STT complete"
        );

        if transcript.text.trim().is_empty() {
            return Ok(InteractionResult {
                transcript: transcript.text,
                response: String::new(),
                tool_used: None,
            });
        }

        if let VoiceIntentDecision::Reject(_) = intent::assess_transcript(&transcript.text) {
            return Ok(InteractionResult {
                transcript: transcript.text,
                response: String::new(),
                tool_used: None,
            });
        }

        // Step 2: LLM + tool dispatch
        let (response_text, tool_used) = self.process_text(&transcript.text).await?;

        // Step 3: Format for voice
        let voice_text = format::for_voice(&response_text);

        // Step 4+5: TTS → Speaker
        if !voice_text.is_empty() {
            self.tts.speak(&voice_text).await?;
            tracing::info!("TTS playback complete");
        }

        Ok(InteractionResult {
            transcript: transcript.text,
            response: response_text,
            tool_used,
        })
    }

    /// Process text input (used by both voice pipeline and stdin mode).
    pub async fn process_text(&mut self, text: &str) -> Result<(String, Option<String>)> {
        // Add to conversation history.
        self.conversation.push(Message {
            role: "user".into(),
            content: text.to_string(),
        });

        // Build message list.
        let mut messages = vec![Message {
            role: "system".into(),
            content: self.system_prompt.clone(),
        }];
        let start = self.conversation.len().saturating_sub(20);
        messages.extend_from_slice(&self.conversation[start..]);

        // Stream LLM response.
        let mut full_response = String::new();
        let stream_result = self
            .llm
            .chat_stream(&messages, Some(512), |token| {
                full_response.push_str(token);
            })
            .await;

        let response = match stream_result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    backend = %self.llm.backend_name(),
                    "LLM backend error"
                );
                return Ok(("Sorry, I couldn't process that.".into(), None));
            }
        };

        // Check for tool call in response.
        let mut tool_used = None;
        if let Some(tool_result) = self.try_tool_call(&response).await {
            tool_used = Some(tool_result.tool.clone());

            // Add tool result to conversation.
            self.conversation.push(Message {
                role: "assistant".into(),
                content: response.clone(),
            });
            self.conversation.push(Message {
                role: "system".into(),
                content: format!("Tool result: {}", tool_result.output),
            });

            // Get natural language summary.
            let mut summary_messages = vec![Message {
                role: "system".into(),
                content: "Summarize the tool result in one natural sentence for voice.".into(),
            }];
            let start = self.conversation.len().saturating_sub(6);
            summary_messages.extend_from_slice(&self.conversation[start..]);

            let summary = self
                .llm
                .chat(&summary_messages, Some(128))
                .await
                .unwrap_or_else(|_| tool_result.output.clone());

            self.conversation.push(Message {
                role: "assistant".into(),
                content: summary.clone(),
            });

            return Ok((summary, tool_used));
        }

        // Normal response.
        self.conversation.push(Message {
            role: "assistant".into(),
            content: response.clone(),
        });

        Ok((response, tool_used))
    }

    async fn try_tool_call(&self, response: &str) -> Option<crate::tools::ToolResult> {
        let trimmed = response.trim();

        // Try whole response as tool call JSON.
        if let Some(result) = self.parse_and_execute_tool(trimmed).await {
            return Some(result);
        }

        // Try extracting JSON from within the response.
        if let Some(start) = trimmed.find('{')
            && let Some(end) = trimmed.rfind('}')
            && let Some(result) = self.parse_and_execute_tool(&trimmed[start..=end]).await
        {
            return Some(result);
        }

        None
    }

    async fn parse_and_execute_tool(&self, json_str: &str) -> Option<crate::tools::ToolResult> {
        #[derive(serde::Deserialize)]
        struct ToolCallJson {
            tool: String,
            #[serde(default)]
            arguments: serde_json::Value,
        }

        let call: ToolCallJson = serde_json::from_str(json_str).ok()?;
        if call.tool.is_empty() {
            return None;
        }

        let tc = crate::tools::ToolCall {
            name: call.tool,
            arguments: call.arguments,
        };
        Some(
            self.tools
                .execute_with_context(
                    &tc,
                    crate::tools::ToolExecutionContext {
                        request_origin: crate::tools::RequestOrigin::Voice,
                        ..Default::default()
                    },
                )
                .await,
        )
    }

    /// Clear conversation history.
    pub fn clear_history(&mut self) {
        self.conversation.clear();
    }
}

/// Result of a single voice interaction.
#[derive(Debug)]
pub struct InteractionResult {
    pub transcript: String,
    pub response: String,
    pub tool_used: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::intent::VoiceIntentDecision;

    #[test]
    fn interaction_result_fields() {
        let result = InteractionResult {
            transcript: "turn on the lights".into(),
            response: "Done!".into(),
            tool_used: Some("home_control".into()),
        };
        assert_eq!(result.transcript, "turn on the lights");
        assert!(result.tool_used.is_some());
    }

    #[test]
    fn voice_intent_gate_rejects_ambient_narration() {
        assert_eq!(
            intent::assess_transcript("the old house stood alone at the end of the road"),
            VoiceIntentDecision::Reject("ambient narration")
        );
    }
}
