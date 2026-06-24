//! Voice interaction loop with persistent wake word listener.
//!
//! The wake word Python process loads once and stays running. On detection
//! it writes "WAKE <score>" to stdout, voice loop records + processes,
//! then sends "READY" to resume listening.
//!
//! Pipeline: [wake word] → record → STT → LLM → TTS → speaker → [resume wake]

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Whether the first-voice-reply latency banner (issue #19) has been printed
/// this process. Set true after the first successful voice cycle so subsequent
/// cycles only emit the normal one-liner.
static FIRST_REPLY_BANNER_PRINTED: AtomicBool = AtomicBool::new(false);

use crate::conversation::ConversationStore;
use crate::llm::{LlmClient, LlmRequestHints};
use crate::memory::{SharedMemory, with_shared_memory};
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
    /// TCP port of a long-running whisper-server (managed externally,
    /// typically by genie-whisper.service). Zero disables server mode and
    /// falls back to spawning whisper-cli for every utterance.
    pub whisper_port: u16,
    pub piper_model: String,
    pub piper_path: String,
    pub piper_pipe_mode: bool,
    pub stt_language: String,
    pub voice_tts_models: HashMap<String, String>,
    pub audio_device: String,
    /// ALSA playback device for TTS output (USB headphone, HDMI, 3.5 mm).
    /// May differ from `audio_device` when mic is on a separate card (e.g.
    /// LyraT/I2S input + USB headphone output).
    pub audio_output_device: String,
    pub sample_rate: u32,
    /// Capture denoiser: "deepfilternet", "sox", or "none". See issue #12.
    pub audio_denoiser: String,
    /// Path to the deep-filter binary (used when audio_denoiser == "deepfilternet").
    pub deep_filter_path: String,
    /// DeepFilterNet `--atten-lim` in dB. 100.0 = full denoising.
    pub deep_filter_atten_lim_db: f32,
    /// Half-duplex gate: wait this long after aplay exits before recording.
    /// See issue #15 — prevents the previous TTS from being captured as the
    /// next utterance.
    pub post_tts_silence_ms: u64,
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
    memory: &SharedMemory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
) -> Result<()> {
    // Auto-detect capture device if not specified or set to "auto".
    let audio_device = if voice_cfg.audio_device.is_empty() || voice_cfg.audio_device == "auto" {
        match detect_audio_device().await {
            Some(dev) => {
                tracing::info!(device = %dev, "auto-detected capture device");
                dev
            }
            None => {
                tracing::warn!("no capture device found, using plughw:0,0");
                "plughw:0,0".to_string()
            }
        }
    } else {
        voice_cfg.audio_device.clone()
    };

    // Auto-detect playback device. Uses a separate helper-script invocation
    // with output=1 so it prefers USB audio (headphone/headset) over the
    // capture-only LyraT I2S path.
    let audio_output_device =
        if voice_cfg.audio_output_device.is_empty() || voice_cfg.audio_output_device == "auto" {
            match detect_audio_output_device().await {
                Some(dev) => {
                    tracing::info!(device = %dev, "auto-detected playback device");
                    dev
                }
                None => {
                    tracing::warn!("no playback device found, falling back to 'default'");
                    "default".to_string()
                }
            }
        } else {
            voice_cfg.audio_output_device.clone()
        };

    eprintln!(
        "[voice] Capture device: {}  |  Playback device: {}",
        audio_device, audio_output_device
    );

    // Persist the resolved playback device in the config so downstream
    // tts_engine_for_language and play_wake_tone (which read from voice_cfg)
    // see the actually-bound device instead of the unresolved "auto" / "".
    let voice_cfg = {
        let mut cfg = voice_cfg;
        cfg.audio_output_device = audio_output_device.clone();
        cfg
    };

    let stt_engine = if voice_cfg.whisper_port > 0 {
        tracing::info!(
            port = voice_cfg.whisper_port,
            model = %voice_cfg.whisper_model,
            "STT using long-running whisper-server (model stays loaded in GPU)"
        );
        stt::SttEngine::server(&voice_cfg.whisper_model, voice_cfg.whisper_port)
            .with_language_hint(Some(voice_cfg.stt_language.clone()))
    } else {
        tracing::info!(
            cli = %voice_cfg.whisper_cli_path,
            "STT using whisper-cli (model reloaded each call — set whisper_port to use server mode)"
        );
        stt::SttEngine::cli_with_path(&voice_cfg.whisper_model, &voice_cfg.whisper_cli_path)
            .with_language_hint(Some(voice_cfg.stt_language.clone()))
    };

    let conv_id = conversations.create()?;
    tracing::info!(conv_id = %conv_id, "voice conversation started");

    if llm.health().await {
        eprintln!("{}", llm_connected_message(llm.backend_name()));
    } else {
        eprintln!("{}", llm_unreachable_message(llm.backend_name()));
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
    memory: &SharedMemory,
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

                // Drain stale samples (TTS residue, DMA carry-over) BEFORE
                // printing the prompt — otherwise the 1 s flush would
                // overlap the start of the user's follow-up utterance.
                stt::flush_mic_buffer(audio_device, voice_cfg.sample_rate).await;

                eprintln!(
                    "[voice] Listening for follow-up ({} sec)...",
                    voice_cfg.voice_continuous_secs
                );

                let followup_path = match stt::record_audio(
                    audio_device,
                    voice_cfg.sample_rate,
                    voice_cfg.voice_continuous_secs,
                    stt::Denoiser::from_config(
                        &voice_cfg.audio_denoiser,
                        &voice_cfg.deep_filter_path,
                        voice_cfg.deep_filter_atten_lim_db,
                    ),
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
                        let text = transcript.text.trim().to_string();
                        if !text.is_empty() {
                            // Security: scan for prompt injection (issue #196).
                            crate::security::injection::scan_and_warn(
                                &text,
                                crate::security::injection::source::VOICE_FOLLOWUP,
                            );
                            let response_language = transcript.language.clone().or_else(|| {
                                crate::voice::language::detect_language_from_text(&text)
                            });
                            let speaker = voice_cfg.speaker_identity.identify(
                                &identity::SpeakerIdentityRequest {
                                    wav_path: Some(&followup_path),
                                    transcript: &text,
                                    detected_language: response_language.as_deref(),
                                },
                            );
                            let read_context = identity::build_memory_read_context(&text, &speaker);
                            let _ = tokio::fs::remove_file(&followup_path).await;

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
                            conversations.append_or_log(conv_id, "user", &text, None);

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

                            let memory_context = with_shared_memory(memory, |memory| {
                                inject::build_memory_context_with_read_context(
                                    memory,
                                    &text,
                                    read_context,
                                )
                            });
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
                            let tts_engine = Arc::new(tts_engine_for_language(
                                voice_cfg,
                                audio_device,
                                response_language.as_deref(),
                            ));
                            let request_hints = LlmRequestHints::agent_turn(conv_id, 256);
                            match streaming::stream_and_speak_with_hints(
                                llm,
                                &messages,
                                256,
                                Arc::clone(&tts_engine),
                                Some(&request_hints),
                            )
                            .await
                            {
                                Ok(response) => {
                                    conversations.append_or_log(
                                        conv_id,
                                        "assistant",
                                        &response,
                                        None,
                                    );
                                    eprintln!("[voice] GeniePod: {}", format::for_voice(&response));
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[voice] Follow-up LLM backend error ({}): {}",
                                        llm.backend_name(),
                                        e
                                    );
                                }
                            }

                            // Auto-capture from follow-up.
                            with_shared_memory(memory, |memory| {
                                extract::extract_and_store(memory, &text);
                            });
                        } else {
                            let _ = tokio::fs::remove_file(&followup_path).await;
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
            if writer.write_all(b"READY\n").await.is_err() {
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
    memory: &SharedMemory,
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
        eprintln!("[voice] Total cycle: {} ms\n", total_ms);

        if !should_continue {
            break;
        }
    }

    tracing::info!("voice loop exited");
    Ok(())
}

/// Auto-detect the ALSA playback device. Prefers USB audio (headphone /
/// headset) over the Tegra APE / I2S frontend (which is usually capture-only
/// on a LyraT-style setup).
async fn detect_audio_output_device() -> Option<String> {
    const DETECT_SCRIPT: &str = "/opt/geniepod/bin/detect-audio-device.sh";

    if tokio::fs::metadata(DETECT_SCRIPT).await.is_ok() {
        // Pass --output so the script returns a sink-suitable device
        // (skipping LyraT/APE which our firmware leaves capture-only).
        match Command::new(DETECT_SCRIPT).arg("--output").output().await {
            Ok(out) if out.status.success() => {
                let dev = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !dev.is_empty() {
                    return Some(dev);
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "detect-audio-device.sh --output failed"),
        }
    }

    // In-process fallback: scan /proc/asound/cards for USB audio.
    let cards = tokio::fs::read_to_string("/proc/asound/cards").await.ok()?;
    for line in cards.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("usb-audio")
            || line_lower.contains("usb audio")
            || line_lower.contains("lenovo")
            || line_lower.contains("headphone")
            || line_lower.contains("headset")
        {
            let card_num = line.split_whitespace().next()?;
            if let Ok(num) = card_num.parse::<u32>() {
                return Some(format!("plughw:{},0", num));
            }
        }
    }
    None
}

/// Auto-detect the ALSA capture device.
///
/// First delegates to `/opt/geniepod/bin/detect-audio-device.sh` when present
/// — that script understands both Tegra APE (LyraT / I2S2 on the 40-pin
/// header, see `doc/lyrat-jetson-audio.md`) and USB audio. Falls back to an
/// in-process scan of `/proc/asound/cards` for USB keywords so a stand-alone
/// `cargo run` without the deploy layout still detects USB devices.
async fn detect_audio_device() -> Option<String> {
    const DETECT_SCRIPT: &str = "/opt/geniepod/bin/detect-audio-device.sh";

    if tokio::fs::metadata(DETECT_SCRIPT).await.is_ok() {
        match Command::new(DETECT_SCRIPT).output().await {
            Ok(out) if out.status.success() => {
                let dev = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !dev.is_empty() {
                    return Some(dev);
                }
            }
            Ok(_) => {} // script exit non-zero — fall through to in-process scan
            Err(e) => tracing::debug!(error = %e, "detect-audio-device.sh failed, falling back"),
        }
    }

    // In-process USB fallback (also used during dev when deploy script isn't installed).
    let cards = tokio::fs::read_to_string("/proc/asound/cards").await.ok()?;

    for line in cards.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("usb-audio")
            || line_lower.contains("usb audio")
            || line_lower.contains("lenovo")
            || line_lower.contains("headphone")
            || line_lower.contains("headset")
            || line_lower.contains("microphone")
        {
            let card_num = line.split_whitespace().next()?;
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
        whisper_port: cfg.whisper_port,
        piper_model: cfg.piper_model.clone(),
        piper_path: cfg.piper_path.clone(),
        piper_pipe_mode: cfg.piper_pipe_mode,
        stt_language: cfg.stt_language.clone(),
        voice_tts_models: cfg.voice_tts_models.clone(),
        audio_device: cfg.audio_device.clone(),
        audio_output_device: cfg.audio_output_device.clone(),
        sample_rate: cfg.sample_rate,
        audio_denoiser: cfg.audio_denoiser.clone(),
        deep_filter_path: cfg.deep_filter_path.clone(),
        deep_filter_atten_lim_db: cfg.deep_filter_atten_lim_db,
        post_tts_silence_ms: cfg.post_tts_silence_ms,
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
    _audio_device: &str,
    language: Option<&str>,
) -> tts::TtsEngine {
    let model = crate::voice::language::select_tts_model(
        language,
        &voice_cfg.voice_tts_models,
        &voice_cfg.piper_model,
    );
    // TTS always uses the playback device, not the capture device. The
    // `_audio_device` parameter is kept for call-site compatibility; the
    // playback device is read directly from voice_cfg.audio_output_device,
    // which voice_loop::run() has already resolved (auto-detect or config).
    tts::TtsEngine::configured(
        model,
        &voice_cfg.piper_path,
        &voice_cfg.audio_output_device,
        voice_cfg.piper_pipe_mode,
    )
    .with_post_silence_ms(voice_cfg.post_tts_silence_ms)
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
        // The voice web_search fast-path renders its own speech-optimized output
        // instead of dispatching through `execute_with_context`, so it must
        // still flow through the single gate chokepoint (ACLs, confirmation,
        // per-tool rate limits, audit) rather than reaching the provider
        // directly. Refusals are spoken back like any other tool result.
        let exec_ctx = crate::tools::ToolExecutionContext {
            memory_read_context: Some(read_context),
            request_origin: crate::tools::RequestOrigin::Voice,
            confirmed: false,
        };
        if let Some(rejected) = tools.gate_tool_call(&call, exec_ctx) {
            let response = crate::security::sandbox::sanitize_output(&rejected.output);
            conversations.append_or_log(conv_id, "assistant", &response, None);
            let tts_engine = tts_engine_for_language(voice_cfg, audio_device, response_language);
            let voice_text = format::for_voice(&response);
            if !voice_text.is_empty() {
                let _ = tts_engine.speak(&voice_text).await;
            }
            return Some(response);
        }

        let (query, limit, fresh) =
            match crate::tools::dispatch::parse_web_search_args(&call.arguments) {
                Ok(parsed) => parsed,
                Err(error) => {
                    let response =
                        crate::security::sandbox::sanitize_output(&format!("Tool error: {error}"));
                    let started = std::time::Instant::now();
                    tools.audit_gated_tool(&call, exec_ctx, started, false, &response);
                    conversations.append_or_log(conv_id, "assistant", &response, None);
                    let tts_engine =
                        tts_engine_for_language(voice_cfg, audio_device, response_language);
                    let voice_text = format::for_voice(&response);
                    if !voice_text.is_empty() {
                        let _ = tts_engine.speak(&voice_text).await;
                    }
                    return Some(response);
                }
            };

        let started = std::time::Instant::now();
        let (response, voice_response, success) =
            match tools.web_search_response(&query, limit, fresh).await {
                Ok(result) => {
                    let voice_response = result.render_voice();
                    (result.response, voice_response, true)
                }
                Err(e) => {
                    let error = format!("web_search failed: {}", e);
                    (error.clone(), error, false)
                }
            };
        tools.audit_gated_tool(&call, exec_ctx, started, success, &response);
        let response = crate::security::sandbox::sanitize_output(&response);
        let tool_json = serde_json::json!({
            "tool": call.name,
            "arguments": call.arguments,
        })
        .to_string();

        conversations.append_or_log(conv_id, "assistant", &tool_json, Some("web_search"));
        conversations.append_or_log(conv_id, "system", &format!("Tool: {}", response), None);
        conversations.append_or_log(conv_id, "assistant", &response, None);

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
                request_origin: crate::tools::RequestOrigin::Voice,
                confirmed: false,
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

    conversations.append_or_log(conv_id, "assistant", &tool_json, Some(&tool_result.tool));
    conversations.append_or_log(
        conv_id,
        "system",
        &format!("Tool: {}", tool_result.output),
        None,
    );
    conversations.append_or_log(conv_id, "assistant", &response, None);

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
    memory: &SharedMemory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
    conv_id: &str,
) -> bool {
    // Step 1: Record (fixed duration — reliable).
    //
    // Drain stale samples (TTS residue from the previous cycle, kernel-side
    // DMA carry-over) BEFORE printing the prompt. Doing the flush inside
    // record_audio would chop ~1 s off the start of the user's speech,
    // because the user starts talking the moment they see "speak now!".
    stt::flush_mic_buffer(audio_device, voice_cfg.sample_rate).await;
    eprintln!(
        "[voice] Recording {} seconds — speak now!",
        voice_cfg.record_secs
    );
    // Reset latency banner phase markers before this cycle's recording and
    // TTS pipeline run. The markers are stamped at distinct moments inside
    // record_audio (after arecord) and TtsEngine::speak() (entry + first
    // PCM write), letting us decompose first-reply latency into five phases.
    stt::reset_audio_captured_marker();
    tts::reset_first_audio_marker();

    let wav_path = match stt::record_audio(
        audio_device,
        voice_cfg.sample_rate,
        voice_cfg.record_secs,
        stt::Denoiser::from_config(
            &voice_cfg.audio_denoiser,
            &voice_cfg.deep_filter_path,
            voice_cfg.deep_filter_atten_lim_db,
        ),
    )
    .await
    {
        Ok(path) => path,
        Err(e) => {
            eprintln!("[voice] Recording failed: {}", e);
            return true;
        }
    };
    // T0 for the latency banner (#19): record_audio has returned, so DFN+sox
    // preprocessing is also done. The arecord-finished instant is captured
    // earlier inside record_audio via stt::audio_captured_at().
    let t_preprocess_done = std::time::Instant::now();

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
    let transcript = match stt_engine.transcribe_file(&wav_path).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[voice] STT failed: {}", e);
            let _ = tokio::fs::remove_file(&wav_path).await;
            return true;
        }
    };

    process_transcript(
        transcript,
        ProcessTranscriptInputs {
            voice_cfg,
            audio_device,
            llm,
            tools,
            memory,
            conversations,
            system_prompt,
            max_history,
            model_family,
            conv_id,
            wav_path: Some(&wav_path),
            tts_engine_override: None,
            t_preprocess_done,
        },
    )
    .await
}

/// Inputs threaded through `process_transcript` (extracted from
/// `voice_cycle` for #21 AC-B). Most fields are forwarded directly;
/// `wav_path` and `tts_engine_override` exist so the integration test
/// can drive the orchestration without a real WAV on disk and without
/// spawning Piper.
pub struct ProcessTranscriptInputs<'a> {
    pub voice_cfg: &'a VoiceConfig,
    pub audio_device: &'a str,
    pub llm: &'a LlmClient,
    pub tools: &'a ToolDispatcher,
    pub memory: &'a SharedMemory,
    pub conversations: &'a ConversationStore,
    pub system_prompt: &'a str,
    pub max_history: usize,
    pub model_family: ModelFamily,
    pub conv_id: &'a str,
    /// Recording path for speaker identity + cleanup. `None` in tests
    /// where the transcript came from `SttEngine::mock` and no WAV
    /// exists on disk.
    pub wav_path: Option<&'a str>,
    /// Test hook: when `Some`, the LLM-to-TTS streaming bridge uses a
    /// snapshot of this engine instead of building one via
    /// `tts_engine_for_language(voice_cfg, ...)`. Tests pass
    /// `Some(&TtsEngine::silent())` so Piper / aplay never spawn.
    pub tts_engine_override: Option<&'a tts::TtsEngine>,
    /// Latency-banner marker for the (preprocess -> STT) phase. Tests
    /// can pass `std::time::Instant::now()`; the banner output is
    /// informational and is fine with arbitrary tiny deltas.
    pub t_preprocess_done: std::time::Instant,
}

/// Post-record orchestration of a voice cycle: intent gate, speaker
/// identity, memory recall, quick-tool fast path, LLM streaming + TTS,
/// tool dispatch, conversation persistence, latency banner, memory
/// extract. Extracted from `voice_cycle` so
/// `tests/voice_loop_integration.rs` can drive the full path end-to-end
/// with `SttEngine::mock`, `LlmClient::mock`, and `TtsEngine::silent`
/// (issue #21 AC-B / IS-1).
///
/// Returns `false` only when the caller should exit the outer voice
/// loop — today nothing here ever signals exit, so this always returns
/// `true`.
pub async fn process_transcript(
    transcript: stt::Transcript,
    inputs: ProcessTranscriptInputs<'_>,
) -> bool {
    let ProcessTranscriptInputs {
        voice_cfg,
        audio_device,
        llm,
        tools,
        memory,
        conversations,
        system_prompt,
        max_history,
        model_family,
        conv_id,
        wav_path,
        tts_engine_override,
        t_preprocess_done,
    } = inputs;

    let text = transcript.text.trim().to_string();
    if text.is_empty() {
        if let Some(path) = wav_path {
            let _ = tokio::fs::remove_file(path).await;
        }
        eprintln!("[voice] No speech detected.");
        return true;
    }
    // Security: scan for prompt injection (issue #196).
    crate::security::injection::scan_and_warn(&text, crate::security::injection::source::VOICE);
    if let VoiceIntentDecision::Reject(reason) = intent::assess_transcript(&text) {
        if let Some(path) = wav_path {
            let _ = tokio::fs::remove_file(path).await;
        }
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
            wav_path,
            transcript: &text,
            detected_language: response_language.as_deref(),
        });
    let read_context = identity::build_memory_read_context(&text, &speaker);
    if let Some(path) = wav_path {
        let _ = tokio::fs::remove_file(path).await;
    }

    // T1 for the latency banner (#19): STT response is in.
    let t_stt_done = std::time::Instant::now();

    eprintln!(
        "[voice] You said: \"{}\" (STT: {} ms)",
        text, transcript.duration_ms
    );
    conversations.append_or_log(conv_id, "user", &text, None);

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
        let stored = with_shared_memory(memory, |memory| extract::extract_and_store(memory, &text));
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
    let memory_context = with_shared_memory(memory, |memory| {
        inject::build_memory_context_with_read_context(memory, &text, read_context)
    });
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
    let tts_engine = Arc::new(match tts_engine_override {
        // Test path (#21 AC-B): caller passes in `&TtsEngine::silent()`.
        // Snapshot it into an owned copy so we can wrap in `Arc` for the
        // streaming task without forcing the caller to give up ownership.
        Some(engine) => engine.snapshot(),
        None => tts_engine_for_language(voice_cfg, audio_device, response_language.as_deref()),
    });

    let request_hints = LlmRequestHints::agent_turn(conv_id, 256);
    let response = match streaming::stream_and_speak_with_hints(
        llm,
        &messages,
        256,
        Arc::clone(&tts_engine),
        Some(&request_hints),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // Fallback: try non-streaming if streaming fails.
            eprintln!(
                "[voice] Streaming failed for {} backend ({}), trying blocking...",
                llm.backend_name(),
                e
            );
            match llm
                .chat_with_hints(&messages, Some(256), &request_hints)
                .await
            {
                Ok(r) => {
                    let voice_text = format::for_voice(&r);
                    if !voice_text.is_empty() {
                        let _ = tts_engine.speak(&voice_text).await;
                    }
                    r
                }
                Err(e2) => {
                    eprintln!("[voice] LLM backend error ({}): {}", llm.backend_name(), e2);
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
                request_origin: crate::tools::RequestOrigin::Voice,
                confirmed: false,
            },
        )
        .await
    {
        eprintln!(
            "[voice] Tool: {} → {}",
            tool_result.tool, tool_result.output
        );
        conversations.append_or_log(conv_id, "assistant", &response, Some(&tool_result.tool));
        conversations.append_or_log(
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
                    "Answer the user's request conversationally using the tool result. \
                     Be brief, ideally 5 to 10 words. Speak as a household assistant — \
                     never say 'tool result', 'the tool indicates', 'the value is', or \
                     similar machine phrasing. Use the user's language: {}.",
                    language
                )
            } else {
                "Answer the user's request conversationally using the tool result. \
                 Be brief, ideally 5 to 10 words. Speak as a household assistant — \
                 never say 'tool result', 'the tool indicates', 'the value is', or \
                 similar machine phrasing."
                    .into()
            },
        }];
        summary_msgs.extend(recent);
        let (summary_msgs, _) = crate::reasoning::apply_reasoning_mode(
            model_family,
            &summary_msgs,
            "",
            InteractionKind::ToolSummary,
        );

        let summary_hints = LlmRequestHints::tool_summary(conv_id, 128);
        let summary = match streaming::stream_and_speak_with_hints(
            llm,
            &summary_msgs,
            128,
            Arc::clone(&tts_engine),
            Some(&summary_hints),
        )
        .await
        {
            Ok(s) => s,
            Err(_) => {
                let s = llm
                    .chat_with_hints(&summary_msgs, Some(128), &summary_hints)
                    .await
                    .unwrap_or_else(|_| tool_result.output.clone());
                let voice_text = format::for_voice(&s);
                if !voice_text.is_empty() {
                    let _ = tts_engine.speak(&voice_text).await;
                }
                s
            }
        };

        conversations.append_or_log(conv_id, "assistant", &summary, None);
        (summary, Some(tool_result.tool))
    } else {
        conversations.append_or_log(conv_id, "assistant", &response, None);
        (response, None)
    };

    eprintln!(
        "[voice] GeniePod: {} (LLM+TTS: {} ms)",
        format::for_voice(&final_response),
        llm_tts_ms
    );

    // First-voice-reply latency banner (#19). Print once per process so an
    // operator can see at a glance whether the LLM/whisper warmup services
    // pre-loaded the iGPU (target: a few hundred ms STT, low-second total)
    // vs the cold path (60-90 s STT, multi-minute total). Subsequent cycles
    // keep only the existing one-liner.
    //
    // The breakdown decomposes "speech end -> first audio" into five phases
    // so the operator can see exactly where time went — LLM cold-start time
    // looks identical to slow Piper synth in a single number.
    //
    //   capture (DFN+sox) = preprocess_done - audio_captured
    //   STT               = stt_done        - preprocess_done
    //   LLM thinking      = first_speak     - stt_done
    //   TTS first synth   = first_audio     - first_speak
    //   total             = first_audio     - audio_captured
    if !FIRST_REPLY_BANNER_PRINTED.swap(true, Ordering::SeqCst) {
        let audio_captured = stt::audio_captured_at();
        let first_speak = tts::first_speak_called_at();
        let first_audio = tts::first_audio_at();

        let fmt = |v: Option<u128>| match v {
            Some(ms) => format!("{} ms", ms),
            None => "n/a".into(),
        };
        let preprocess_ms = audio_captured.map(|t| (t_preprocess_done - t).as_millis());
        let stt_phase_ms = Some((t_stt_done - t_preprocess_done).as_millis());
        let llm_ms = first_speak.map(|t| (t - t_stt_done).as_millis());
        let tts_synth_ms = first_speak.and_then(|fs| first_audio.map(|fa| (fa - fs).as_millis()));
        let total_ms = audio_captured.and_then(|t0| first_audio.map(|fa| (fa - t0).as_millis()));

        eprintln!();
        eprintln!("=== first voice reply latency ===");
        eprintln!("  preprocess (DFN+sox):      {}", fmt(preprocess_ms));
        eprintln!("  STT:                       {}", fmt(stt_phase_ms));
        eprintln!("  LLM until first sentence:  {}", fmt(llm_ms));
        eprintln!("  TTS first synth:           {}", fmt(tts_synth_ms));
        eprintln!("  --------------------------------");
        eprintln!("  speech end -> first audio: {}", fmt(total_ms));
        eprintln!("=================================");
        eprintln!();
    }

    // Auto-capture facts from user's speech (runs after TTS, non-blocking).
    let stored = with_shared_memory(memory, |memory| extract::extract_and_store(memory, &text));
    if stored > 0 {
        eprintln!(
            "[voice] (remembered {} fact{})",
            stored,
            if stored == 1 { "" } else { "s" }
        );
    }

    true
}

fn llm_connected_message(backend_name: &str) -> String {
    format!("[voice] LLM backend connected ({backend_name})")
}

fn llm_unreachable_message(backend_name: &str) -> String {
    format!(
        "[voice] WARNING: LLM backend not reachable ({backend_name}) - check configured service"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_status_messages_include_backend_name() {
        let connected = llm_connected_message("genie-ai-runtime");
        let unreachable = llm_unreachable_message("genie-ai-runtime");

        assert!(connected.contains("genie-ai-runtime"));
        assert!(unreachable.contains("genie-ai-runtime"));
    }

    #[test]
    fn llm_unreachable_message_is_backend_neutral() {
        let unreachable = llm_unreachable_message("genie-ai-runtime");

        assert!(!unreachable.contains("llama-server"));
        assert!(!unreachable.contains("llama.cpp"));
    }
}
