//! Voice interaction loop with persistent wake word listener.
//!
//! The wake word Python process loads once and stays running. On detection
//! it writes "WAKE <score>" to stdout, voice loop records + processes,
//! then sends "READY" to resume listening.
//!
//! Pipeline: [wake word] → record → STT → LLM → TTS → speaker → [resume wake]

use anyhow::Result;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::conversation::ConversationStore;
use crate::llm::LlmClient;
use crate::memory::Memory;
use crate::memory::{extract, inject};
use crate::prompt::ModelFamily;
use crate::reasoning::InteractionKind;
use crate::tools::ToolDispatcher;
use crate::voice::identity::{self, SpeakerIdentityProvider};
use crate::voice::intent::{self, VoiceIntentDecision};
use crate::voice::{aec, format, streaming, stt, tts};

/// Configuration for the voice loop.
pub struct VoiceConfig {
    pub whisper_model: String,
    pub whisper_cli_path: String,
    pub piper_model: String,
    pub piper_path: String,
    pub piper_pipe_mode: bool,
    pub stt_language: String,
    pub voice_tts_models: HashMap<String, String>,
    pub audio_device: String,
    pub sample_rate: u32,
    pub record_secs: u32,
    pub llm_model_path: String,
    pub wakeword_script: String,
    /// After response, auto-listen for follow-up without re-wake.
    pub voice_continuous: bool,
    /// Recording duration for follow-up (shorter than initial).
    pub voice_continuous_secs: u32,
    /// Speaker identity provider for voice memory context.
    pub speaker_identity: SpeakerIdentityProvider,
}

/// Run the voice interaction loop.
pub async fn run(
    voice_cfg: VoiceConfig,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
) -> Result<()> {
    // Auto-detect audio device if not specified or set to "auto".
    let audio_device = if voice_cfg.audio_device.is_empty() || voice_cfg.audio_device == "auto" {
        match detect_audio_device().await {
            Some(dev) => {
                tracing::info!(device = %dev, "auto-detected USB audio device");
                dev
            }
            None => {
                tracing::warn!("no USB audio device found, using plughw:0,0");
                "plughw:0,0".to_string()
            }
        }
    } else {
        voice_cfg.audio_device.clone()
    };

    eprintln!("[voice] Audio device: {}", audio_device);

    let stt_engine =
        stt::SttEngine::cli_with_path(&voice_cfg.whisper_model, &voice_cfg.whisper_cli_path)
            .with_language_hint(Some(voice_cfg.stt_language.clone()));

    let conv_id = conversations.create()?;
    tracing::info!(conv_id = %conv_id, "voice conversation started");

    if llm.health().await {
        eprintln!("[voice] LLM server connected");
    } else {
        eprintln!("[voice] WARNING: LLM not reachable — start llama-server first!");
    }

    let use_wakeword = !voice_cfg.wakeword_script.is_empty()
        && std::path::Path::new(&voice_cfg.wakeword_script).exists();

    if use_wakeword {
        eprintln!("\n=== GeniePod Voice Mode (Wake Word) ===");
        eprintln!(
            "Say the configured wake phrase to activate (default development wake phrase: \"Hey Jarvis\") ({} sec recording).\n",
            voice_cfg.record_secs
        );
        run_with_wakeword(
            &voice_cfg,
            &audio_device,
            &stt_engine,
            llm,
            tools,
            memory,
            conversations,
            system_prompt,
            max_history,
            model_family,
            &conv_id,
        )
        .await
    } else {
        eprintln!("\n=== GeniePod Voice Mode (Push-to-Talk) ===");
        eprintln!(
            "Press Enter to speak ({} sec), 'quit' to exit.\n",
            voice_cfg.record_secs
        );
        run_push_to_talk(
            &voice_cfg,
            &audio_device,
            &stt_engine,
            llm,
            tools,
            memory,
            conversations,
            system_prompt,
            max_history,
            model_family,
            &conv_id,
        )
        .await
    }
}

/// Wake word mode: persistent Python process, model loaded once.
async fn run_with_wakeword(
    voice_cfg: &VoiceConfig,
    audio_device: &str,
    stt_engine: &stt::SttEngine,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
    conv_id: &str,
) -> Result<()> {
    // Outer loop: restarts the wake word listener if it crashes or pipe breaks.
    loop {
        eprintln!("[voice] Starting wake word listener...");

        let mut child = match Command::new("python3")
            .args([&voice_cfg.wakeword_script, "--threshold", "0.3"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[voice] Failed to start wake word listener: {}", e);
                anyhow::bail!("wake word listener failed: {}", e);
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;

        let mut reader = BufReader::new(stdout);
        let mut writer = stdin;

        // Wait for "LISTENING" signal.
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if !line.trim().starts_with("LISTENING") {
            eprintln!("[voice] Wake word listener failed: {}", line.trim());
            let _ = child.kill().await;
            anyhow::bail!("wake word listener failed to start: {}", line.trim());
        }
        eprintln!(
            "[voice] Wake word listener ready — default development wake phrase: \"Hey Jarvis\""
        );

        // Inner loop: process wake events until pipe breaks.
        let mut restart_needed = false;
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                eprintln!("[voice] Wake word listener exited — restarting...");
                restart_needed = true;
                break;
            }

            let trimmed = line.trim();
            if !trimmed.starts_with("WAKE") {
                continue;
            }

            eprintln!("[voice] Wake word detected! ({})", trimmed);

            // Note: wake confirmation tone disabled on USB headphone — acoustic
            // coupling causes tone to leak into mic and confuse Whisper.
            // Re-enable on production hardware (separate speaker + side mics):
            // play_wake_tone(audio_device).await;
            // tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            let cycle_start = std::time::Instant::now();

            // Run voice interaction cycle.
            let should_continue = voice_cycle(
                voice_cfg,
                audio_device,
                stt_engine,
                llm,
                tools,
                memory,
                conversations,
                system_prompt,
                max_history,
                model_family,
                conv_id,
            )
            .await;

            let total_ms = cycle_start.elapsed().as_millis();
            eprintln!("[voice] Total cycle: {} ms", total_ms);

            if !should_continue {
                let _ = child.kill().await;
                tracing::info!("voice loop exited");
                return Ok(());
            }

            // Continuous conversation mode: record follow-up BEFORE restarting listener.
            // The wake word process has released the mic, so arecord can use it.
            if voice_cfg.voice_continuous {
                // Small delay to ensure mic is fully released by wake word process.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;

                eprintln!(
                    "[voice] Listening for follow-up ({} sec)...",
                    voice_cfg.voice_continuous_secs
                );

                let followup_path = match stt::record_audio(
                    audio_device,
                    voice_cfg.sample_rate,
                    voice_cfg.voice_continuous_secs,
                )
                .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[voice] Follow-up recording failed: {}", e);
                        String::new()
                    }
                };

                if !followup_path.is_empty() {
                    eprintln!("[voice] Transcribing follow-up...");
                    if let Ok(transcript) = stt_engine.transcribe_file(&followup_path).await {
                        let _ = tokio::fs::remove_file(&followup_path).await;
                        let text = transcript.text.trim().to_string();
                        let response_language = transcript
                            .language
                            .clone()
                            .or_else(|| crate::voice::language::detect_language_from_text(&text));
                        let speaker = voice_cfg.speaker_identity.identify(
                            &identity::SpeakerIdentityRequest {
                                wav_path: Some(&followup_path),
                                transcript: &text,
                                detected_language: response_language.as_deref(),
                            },
                        );
                        let read_context = identity::build_memory_read_context(&text, &speaker);

                        if !text.is_empty() {
                            if let VoiceIntentDecision::Reject(reason) =
                                intent::assess_transcript(&text)
                            {
                                eprintln!(
                                    "[voice] Ignoring follow-up transcript ({}): \"{}\"",
                                    reason, text
                                );
                                continue;
                            }

                            eprintln!(
                                "[voice] Follow-up: \"{}\" (STT: {} ms)",
                                text, transcript.duration_ms
                            );

                            // Build context and process — reuse voice_cycle but skip recording
                            // (we already have the text).
                            let _ = conversations.append(conv_id, "user", &text, None);

                            if handle_quick_tool_for_voice(
                                tools,
                                conversations,
                                conv_id,
                                &text,
                                read_context,
                                voice_cfg,
                                audio_device,
                                response_language.as_deref(),
                            )
                            .await
                            .is_some()
                            {
                                continue;
                            }

                            let memory_context = inject::build_memory_context_with_read_context(
                                memory,
                                &text,
                                read_context,
                            );
                            let full_prompt = format!(
                                "{}\n\nRelevant household context:\n{}",
                                system_prompt, memory_context
                            );
                            let history = conversations
                                .get_recent(conv_id, max_history)
                                .unwrap_or_default();
                            let mut messages = vec![crate::llm::Message {
                                role: "system".into(),
                                content: full_prompt,
                            }];
                            messages.extend(history);

                            eprintln!("[voice] Thinking...");
                            let tts_engine = tts_engine_for_language(
                                voice_cfg,
                                audio_device,
                                response_language.as_deref(),
                            );
                            match streaming::stream_and_speak(llm, &messages, 256, &tts_engine)
                                .await
                            {
                                Ok(response) => {
                                    let _ =
                                        conversations.append(conv_id, "assistant", &response, None);
                                    eprintln!("[voice] GeniePod: {}", format::for_voice(&response));
                                }
                                Err(e) => {
                                    eprintln!("[voice] Follow-up LLM error: {}", e);
                                }
                            }

                            // Auto-capture from follow-up.
                            extract::extract_and_store(memory, &text);
                        } else {
                            eprintln!("[voice] No follow-up speech — returning to wake word.");
                        }
                    } else {
                        let _ = tokio::fs::remove_file(&followup_path).await;
                        eprintln!("[voice] Follow-up STT failed.");
                    }
                }
            }

            eprintln!();

            // Tell listener to resume. If pipe broke, restart the listener.
            if let Err(_) = writer.write_all(b"READY\n").await {
                eprintln!("[voice] Restarting wake word listener...");
                restart_needed = true;
                break;
            }
            let _ = writer.flush().await;
            eprintln!("[voice] Listening for the configured wake phrase...");
        }

        // Clean up old listener before restarting.
        let _ = child.kill().await;

        if !restart_needed {
            break;
        }

        // Brief pause before restart to avoid rapid cycling.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    tracing::info!("voice loop exited");
    Ok(())
}

/// Push-to-talk mode: Enter key triggers recording.
async fn run_push_to_talk(
    voice_cfg: &VoiceConfig,
    audio_device: &str,
    stt_engine: &stt::SttEngine,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
    conv_id: &str,
) -> Result<()> {
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    loop {
        eprint!("[voice] Press Enter to speak > ");

        let line = match lines.next_line().await? {
            Some(l) => l,
            None => break,
        };
        if line.trim() == "quit" || line.trim() == "exit" {
            break;
        }

        let cycle_start = std::time::Instant::now();

        let should_continue = voice_cycle(
            voice_cfg,
            &audio_device,
            stt_engine,
            llm,
            tools,
            memory,
            conversations,
            system_prompt,
            max_history,
            model_family,
            conv_id,
        )
        .await;

        let total_ms = cycle_start.elapsed().as_millis();
        eprintln!("[voice] Total cycle: {} ms\n", total_ms);

        if !should_continue {
            break;
        }
    }

    tracing::info!("voice loop exited");
    Ok(())
}

/// Auto-detect USB audio device by scanning /proc/asound/cards.
/// Returns the ALSA device string (e.g., "plughw:2,0") or None.
async fn detect_audio_device() -> Option<String> {
    let cards = tokio::fs::read_to_string("/proc/asound/cards").await.ok()?;

    for line in cards.lines() {
        let line_lower = line.to_lowercase();
        // Look for USB audio devices (common keywords)
        if line_lower.contains("usb-audio")
            || line_lower.contains("usb audio")
            || line_lower.contains("lenovo")
            || line_lower.contains("headphone")
            || line_lower.contains("headset")
            || line_lower.contains("microphone")
        {
            // Extract card number (first field on the line)
            let card_num = line.trim().split_whitespace().next()?;
            if let Ok(num) = card_num.parse::<u32>() {
                return Some(format!("plughw:{},0", num));
            }
        }
    }

    None
}

/// Clone VoiceConfig (can't derive Clone due to String fields, but they're cheap).
fn clone_voice_config(cfg: &VoiceConfig) -> VoiceConfig {
    VoiceConfig {
        whisper_model: cfg.whisper_model.clone(),
        whisper_cli_path: cfg.whisper_cli_path.clone(),
        piper_model: cfg.piper_model.clone(),
        piper_path: cfg.piper_path.clone(),
        piper_pipe_mode: cfg.piper_pipe_mode,
        stt_language: cfg.stt_language.clone(),
        voice_tts_models: cfg.voice_tts_models.clone(),
        audio_device: cfg.audio_device.clone(),
        sample_rate: cfg.sample_rate,
        record_secs: cfg.record_secs,
        llm_model_path: cfg.llm_model_path.clone(),
        wakeword_script: cfg.wakeword_script.clone(),
        voice_continuous: cfg.voice_continuous,
        voice_continuous_secs: cfg.voice_continuous_secs,
        speaker_identity: cfg.speaker_identity.clone(),
    }
}

fn tts_engine_for_language(
    voice_cfg: &VoiceConfig,
    audio_device: &str,
    language: Option<&str>,
) -> tts::TtsEngine {
    let model = crate::voice::language::select_tts_model(
        language,
        &voice_cfg.voice_tts_models,
        &voice_cfg.piper_model,
    );
    tts::TtsEngine::configured(
        model,
        &voice_cfg.piper_path,
        audio_device,
        voice_cfg.piper_pipe_mode,
    )
}

async fn handle_quick_tool_for_voice(
    tools: &ToolDispatcher,
    conversations: &ConversationStore,
    conv_id: &str,
    text: &str,
    read_context: crate::memory::policy::MemoryReadContext,
    voice_cfg: &VoiceConfig,
    audio_device: &str,
    response_language: Option<&str>,
) -> Option<String> {
    let call = crate::tools::quick::route_for_available_tools(
        text,
        tools.has_home_automation(),
        tools.has_web_search(),
    )?;
    if call.name == "web_search" {
        let query = call
            .arguments
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();
        let limit = call
            .arguments
            .get("limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(3) as usize;
        let fresh = call
            .arguments
            .get("fresh")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        let (response, voice_response) = match tools.web_search_response(query, limit, fresh).await
        {
            Ok(result) => {
                let voice_response = result.render_voice();
                (result.response, voice_response)
            }
            Err(e) => {
                let error = format!("web_search failed: {}", e);
                (error.clone(), error)
            }
        };
        let response = crate::security::sandbox::sanitize_output(&response);
        let tool_json = serde_json::json!({
            "tool": call.name,
            "arguments": call.arguments,
        })
        .to_string();

        let _ = conversations.append(&conv_id, "assistant", &tool_json, Some("web_search"));
        let _ = conversations.append(&conv_id, "system", &format!("Tool: {}", response), None);
        let _ = conversations.append(&conv_id, "assistant", &response, None);

        let tts_engine = tts_engine_for_language(voice_cfg, audio_device, response_language);
        let voice_text = format::for_voice(&voice_response);
        if !voice_text.is_empty() {
            let _ = tts_engine.speak(&voice_text).await;
        }

        return Some(response);
    }

    let tool_result = tools
        .execute_with_context(
            &call,
            crate::tools::ToolExecutionContext {
                memory_read_context: Some(read_context),
            },
        )
        .await;
    let response = if tool_result.success {
        tool_result.output.clone()
    } else {
        format!("{} failed: {}", tool_result.tool, tool_result.output)
    };
    let response = crate::security::sandbox::sanitize_output(&response);
    let tool_json = serde_json::json!({
        "tool": call.name,
        "arguments": call.arguments,
    })
    .to_string();

    let _ = conversations.append(conv_id, "assistant", &tool_json, Some(&tool_result.tool));
    let _ = conversations.append(
        conv_id,
        "system",
        &format!("Tool: {}", tool_result.output),
        None,
    );
    let _ = conversations.append(conv_id, "assistant", &response, None);

    let tts_engine = tts_engine_for_language(voice_cfg, audio_device, response_language);
    let voice_text = format::for_voice(&response);
    if !voice_text.is_empty() {
        let _ = tts_engine.speak(&voice_text).await;
    }

    Some(response)
}

/// Play a short confirmation tone when wake word is detected.
/// Gives the user immediate feedback that the device heard them.
async fn play_wake_tone(audio_device: &str) {
    // Generate a short 440Hz + 880Hz dual tone (200ms).
    // 22050 Hz sample rate, 16-bit signed LE, mono.
    let sample_rate = 22050u32;
    let duration_samples = (sample_rate as f64 * 0.15) as usize; // 150ms
    let mut pcm = Vec::with_capacity(duration_samples * 2);

    for i in 0..duration_samples {
        let t = i as f64 / sample_rate as f64;
        // Dual tone: 440Hz + 880Hz, with quick fade-in/out.
        let envelope = if i < 500 {
            i as f64 / 500.0
        } else if i > duration_samples - 500 {
            (duration_samples - i) as f64 / 500.0
        } else {
            1.0
        };
        let sample = (((t * 440.0 * 2.0 * std::f64::consts::PI).sin() * 0.3
            + (t * 880.0 * 2.0 * std::f64::consts::PI).sin() * 0.2)
            * envelope
            * 16000.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
    }

    // Play via aplay.
    let mut args: Vec<&str> = Vec::new();
    if !audio_device.is_empty() {
        args.push("-D");
        args.push(audio_device);
    }
    args.extend_from_slice(&["-f", "S16_LE", "-r", "22050", "-c", "1", "-t", "raw"]);

    let child = Command::new("aplay")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stdin, &pcm).await;
        }
        let _ = child.wait().await;
    }
}

/// Single voice interaction cycle: record → STT → LLM (streaming) → TTS (per-sentence).
/// Returns false if the loop should exit.
async fn voice_cycle(
    voice_cfg: &VoiceConfig,
    audio_device: &str,
    stt_engine: &stt::SttEngine,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
    conv_id: &str,
) -> bool {
    // Step 1: Record (fixed duration — reliable).
    eprintln!(
        "[voice] Recording {} seconds — speak now!",
        voice_cfg.record_secs
    );
    let wav_path =
        match stt::record_audio(audio_device, voice_cfg.sample_rate, voice_cfg.record_secs).await {
            Ok(path) => path,
            Err(e) => {
                eprintln!("[voice] Recording failed: {}", e);
                return true;
            }
        };

    // Step 2: Light noise processing only.
    // Full noise processing (gate, spectral suppression) disabled for now —
    // it degrades Whisper accuracy on USB headphone. Whisper handles noise well natively.
    // AEC and full noise pipeline will be re-enabled for production hardware
    // (separate speaker + side mics where echo/noise is a real problem).
    //
    // For now: just AEC reference tracking (no-op on headphone) + let Whisper handle it.
    aec::process_aec(&wav_path, voice_cfg.sample_rate).await;

    // Step 3: Transcribe.
    eprintln!("[voice] Transcribing...");
    let stt_start = std::time::Instant::now();
    let transcript = match stt_engine.transcribe_file(&wav_path).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[voice] STT failed: {}", e);
            let _ = tokio::fs::remove_file(&wav_path).await;
            return true;
        }
    };

    let _ = tokio::fs::remove_file(&wav_path).await;

    let text = transcript.text.trim().to_string();
    if text.is_empty() {
        eprintln!("[voice] No speech detected.");
        return true;
    }
    if let VoiceIntentDecision::Reject(reason) = intent::assess_transcript(&text) {
        eprintln!(
            "[voice] Ignoring low-confidence transcript ({}): \"{}\"",
            reason, text
        );
        return true;
    }
    let response_language = transcript
        .language
        .clone()
        .or_else(|| crate::voice::language::detect_language_from_text(&text));
    let speaker = voice_cfg
        .speaker_identity
        .identify(&identity::SpeakerIdentityRequest {
            wav_path: Some(&wav_path),
            transcript: &text,
            detected_language: response_language.as_deref(),
        });
    let read_context = identity::build_memory_read_context(&text, &speaker);

    eprintln!(
        "[voice] You said: \"{}\" (STT: {} ms)",
        text, transcript.duration_ms
    );
    let _ = conversations.append(conv_id, "user", &text, None);

    if let Some(final_response) = handle_quick_tool_for_voice(
        tools,
        conversations,
        conv_id,
        &text,
        read_context,
        voice_cfg,
        audio_device,
        response_language.as_deref(),
    )
    .await
    {
        eprintln!(
            "[voice] GeniePod: {} (quick tool)",
            format::for_voice(&final_response)
        );
        let stored = extract::extract_and_store(memory, &text);
        if stored > 0 {
            eprintln!(
                "[voice] (remembered {} fact{})",
                stored,
                if stored == 1 { "" } else { "s" }
            );
        }
        return true;
    }

    // Step 3: Build LLM context with per-query memory injection.
    let memory_context =
        inject::build_memory_context_with_read_context(memory, &text, read_context);
    let full_prompt = format!(
        "{}\n\nRelevant household context:\n{}",
        system_prompt, memory_context
    );

    let history = conversations
        .get_recent(conv_id, max_history)
        .unwrap_or_default();
    let mut messages = vec![crate::llm::Message {
        role: "system".into(),
        content: full_prompt,
    }];
    messages.extend(history);
    let (messages, decision) = crate::reasoning::apply_reasoning_mode(
        model_family,
        &messages,
        &text,
        InteractionKind::Voice,
    );
    tracing::debug!(?model_family, ?decision, "applied reasoning mode for voice");

    // Step 4: LLM → streaming TTS (speak each sentence as it completes).
    eprintln!("[voice] Thinking...");
    let llm_start = std::time::Instant::now();
    let tts_engine = tts_engine_for_language(voice_cfg, audio_device, response_language.as_deref());

    let response = match streaming::stream_and_speak(llm, &messages, 256, &tts_engine).await {
        Ok(r) => r,
        Err(e) => {
            // Fallback: try non-streaming if streaming fails.
            eprintln!("[voice] Streaming failed ({}), trying blocking...", e);
            match llm.chat(&messages, Some(256)).await {
                Ok(r) => {
                    let voice_text = format::for_voice(&r);
                    if !voice_text.is_empty() {
                        let _ = tts_engine.speak(&voice_text).await;
                    }
                    r
                }
                Err(e2) => {
                    eprintln!("[voice] LLM error: {}", e2);
                    return true;
                }
            }
        }
    };

    let llm_tts_ms = llm_start.elapsed().as_millis();

    // Tool dispatch — if LLM output is a tool call, execute and speak result.
    let (final_response, _tool_name) = if let Some(tool_result) =
        crate::tools::try_tool_call_with_context(
            &response,
            tools,
            crate::tools::ToolExecutionContext {
                memory_read_context: Some(read_context),
            },
        )
        .await
    {
        eprintln!(
            "[voice] Tool: {} → {}",
            tool_result.tool, tool_result.output
        );
        let _ = conversations.append(conv_id, "assistant", &response, Some(&tool_result.tool));
        let _ = conversations.append(
            conv_id,
            "system",
            &format!("Tool: {}", tool_result.output),
            None,
        );

        // Get summary and speak it.
        let recent = conversations.get_recent(conv_id, 6).unwrap_or_default();
        let mut summary_msgs = vec![crate::llm::Message {
            role: "system".into(),
            content: if let Some(language) = response_language.as_deref() {
                format!(
                    "Summarize the tool result in one natural sentence for voice. Use the user's language: {}.",
                    language
                )
            } else {
                "Summarize the tool result in one natural sentence for voice.".into()
            },
        }];
        summary_msgs.extend(recent);
        let (summary_msgs, _) = crate::reasoning::apply_reasoning_mode(
            model_family,
            &summary_msgs,
            "",
            InteractionKind::ToolSummary,
        );

        let summary = match streaming::stream_and_speak(llm, &summary_msgs, 128, &tts_engine).await
        {
            Ok(s) => s,
            Err(_) => {
                let s = llm
                    .chat(&summary_msgs, Some(128))
                    .await
                    .unwrap_or_else(|_| tool_result.output.clone());
                let voice_text = format::for_voice(&s);
                if !voice_text.is_empty() {
                    let _ = tts_engine.speak(&voice_text).await;
                }
                s
            }
        };

        let _ = conversations.append(conv_id, "assistant", &summary, None);
        (summary, Some(tool_result.tool))
    } else {
        let _ = conversations.append(conv_id, "assistant", &response, None);
        (response, None)
    };

    eprintln!(
        "[voice] GeniePod: {} (LLM+TTS: {} ms)",
        format::for_voice(&final_response),
        llm_tts_ms
    );

    // Auto-capture facts from user's speech (runs after TTS, non-blocking).
    let stored = extract::extract_and_store(memory, &text);
    if stored > 0 {
        eprintln!(
            "[voice] (remembered {} fact{})",
            stored,
            if stored == 1 { "" } else { "s" }
        );
    }

    true
}
