use std::sync::Arc;

use anyhow::Result;
use genie_common::config::Config;
use genie_core::*;
use tracing_subscriber::EnvFilter;

/// GeniePod Core — the voice AI orchestrator.
///
/// Runs two interfaces concurrently:
/// 1. HTTP chat API on :3000 (for the local web UI, app surfaces, and future adapters)
/// 2. Stdin text mode (for development and testing)
///
/// In production, a third interface is added:
/// 3. Voice pipeline (wake word → STT → LLM → TTS → speaker)
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let config = Config::load()?;
    let port = config.core.port;
    let bind_host = config.core.bind_host.clone();
    tracing::info!("GeniePod core starting");

    // Security audit on startup.
    let config_path = std::env::var("GENIEPOD_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/etc/geniepod/geniepod.toml"));
    genie_core::security::audit::run_audit(&config_path, &config.data_dir);

    let blocked_env = genie_core::security::env_sanitize::count_blocked();
    if blocked_env > 0 {
        tracing::info!(
            blocked_env,
            "sensitive env vars will be excluded from tool execution"
        );
    }

    // Build shared components.
    let llm = llm::LlmClient::from_service_config(&config.services.llm);
    tracing::info!(backend = %llm.backend_name(), "LLM backend configured");

    let ha = ha::provider_from_config(&config);

    let mem_path = config.data_dir.join("memory.db");
    let mem = memory::Memory::open(&mem_path)?;
    tracing::info!(memories = mem.count()?, "memory loaded");

    let mem_arc = Arc::new(std::sync::Mutex::new(memory::Memory::open(&mem_path)?));
    let skill_loader =
        skills::load_all_with_policy(skills::SkillLoadPolicy::from(&config.core.skill_policy));
    let connectivity = Arc::new(connectivity::NullConnectivityController::from_config(
        &config.connectivity,
    ));

    let tool_dispatcher = tools::ToolDispatcher::new(ha)
        .with_web_search_config(config.web_search.clone())
        .with_tool_policy_config(config.core.tool_policy.clone())
        .with_actuation_safety_config(config.core.actuation_safety.clone())
        .with_actuation_audit_path(config.data_dir.join("safety/actuation-audit.jsonl"))
        .with_tool_audit_path(config.data_dir.join("runtime/tool-audit.jsonl"))
        .with_memory(Arc::clone(&mem_arc))
        .with_skill_loader(skill_loader);

    let connectivity_health = connectivity.health().await;
    tracing::info!(
        state = ?connectivity_health.state,
        transport = %connectivity_health.transport,
        device = %connectivity_health.device,
        message = %connectivity_health.message,
        "connectivity subsystem initialized"
    );

    // Load user profile from /opt/geniepod/data/profile/.
    let profile_dir = config.data_dir.join("profile");
    match genie_core::profile::load_profile(&profile_dir, &mem) {
        Ok(report) if report.total() > 0 => {
            tracing::info!(
                toml = report.toml_facts,
                docs = report.doc_facts,
                files = report.files_processed,
                "profile loaded ({} facts)",
                report.total()
            );
        }
        Ok(_) => {
            tracing::debug!(
                "no profile data found — user can add files to {:?}",
                profile_dir
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "profile loading failed");
        }
    }

    // Build system prompt optimized for the LLM model.
    let model_name = &config.core.llm_model_name;
    let model_family = prompt::ModelFamily::from_model_name(model_name);
    let prompt_builder = prompt::PromptBuilder::from_model_name(model_name);
    let system_prompt = prompt_builder.build(&tool_dispatcher.tool_defs(), &mem);
    tracing::info!(
        model = model_name,
        family = ?model_family,
        "system prompt built"
    );

    // Check if stdin is a terminal (REPL mode) or pipe/systemd (server-only).
    let interactive = atty_check();

    // Open conversation store.
    let conv_path = config.data_dir.join("conversations.db");
    let conversations = conversation::ConversationStore::open(&conv_path)?;
    let conv_list = conversations.list()?;
    tracing::info!(conversations = conv_list.len(), "conversation store loaded");

    let boot_contract = genie_core::server::build_runtime_contract_snapshot(
        &tool_dispatcher,
        &mem,
        &conversations,
        &system_prompt,
        config.core.max_history_turns,
        model_family,
        &connectivity_health,
    );
    let contract_hash = boot_contract.contract_hash.clone();
    let contract_validation = genie_core::runtime_contract::validate_runtime_contract(
        &contract_hash,
        &config.core.expected_runtime_contract_hash,
    );
    match contract_validation.status.as_str() {
        "drift" => tracing::warn!(
            contract_hash = %contract_hash,
            expected_hash = ?contract_validation.expected_hash,
            "runtime contract drift detected"
        ),
        "ok" => tracing::info!(
            contract_hash = %contract_hash,
            "runtime contract matches expected hash"
        ),
        _ => tracing::debug!(
            contract_hash = %contract_hash,
            "runtime contract is not pinned"
        ),
    }
    let contract_log_path = config.data_dir.join("runtime/contracts.jsonl");
    match genie_core::runtime_contract::append_runtime_contract_log(
        &contract_log_path,
        &boot_contract,
    ) {
        Ok(()) => tracing::info!(
            contract_hash = %contract_hash,
            path = %contract_log_path.display(),
            "runtime contract logged"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            path = %contract_log_path.display(),
            "failed to log runtime contract"
        ),
    }

    // Check for voice mode: --voice flag or GENIEPOD_VOICE=1 or config voice_enabled.
    let voice_requested = std::env::args().any(|a| a == "--voice")
        || std::env::var("GENIEPOD_VOICE").unwrap_or_default() == "1"
        || config.core.voice_enabled;
    let wakeword_available =
        !config.core.wakeword_script.as_os_str().is_empty() && config.core.wakeword_script.exists();
    let startup_decision =
        runtime_mode::decide_startup_mode(voice_requested, interactive, wakeword_available);

    // Whether the running binary actually has the voice subsystem compiled in.
    // When voice was requested but the binary is chat-only (issue #41 feature
    // gate), emit one warning and fall through to the chat path so an existing
    // voice-tagged `geniepod.toml` still deploys cleanly on a chat-only build.
    #[cfg(feature = "voice")]
    let voice_mode = {
        if startup_decision.blocked_push_to_talk() {
            tracing::warn!(
                "push-to-talk voice mode requested without an interactive terminal and no wakeword script is available; \
                 running HTTP API only"
            );
        }
        startup_decision.enters_voice()
    };
    #[cfg(not(feature = "voice"))]
    let voice_mode = {
        if voice_requested {
            tracing::warn!(
                "voice mode requested (--voice / GENIEPOD_VOICE=1 / core.voice_enabled) but \
                 this genie-core build was compiled without the 'voice' feature; falling back \
                 to chat-only mode. Rebuild with default features (or --features voice) to \
                 enable the voice loop."
            );
        }
        let _ = startup_decision;
        false
    };

    if voice_mode {
        #[cfg(feature = "voice")]
        {
            tracing::info!("voice mode — starting voice interaction loop");
            let voice_cfg = genie_core::voice_loop::VoiceConfig {
                whisper_model: config.core.whisper_model.to_string_lossy().to_string(),
                whisper_cli_path: config.core.whisper_cli_path.to_string_lossy().to_string(),
                whisper_port: config.core.whisper_port,
                piper_model: config.core.piper_model.to_string_lossy().to_string(),
                piper_path: config.core.piper_path.to_string_lossy().to_string(),
                piper_pipe_mode: config.core.piper_pipe_mode,
                stt_language: config.core.stt_language.clone(),
                voice_tts_models: config
                    .core
                    .voice_tts_models
                    .iter()
                    .map(|(language, path)| (language.clone(), path.to_string_lossy().to_string()))
                    .collect(),
                audio_device: config.core.audio_device.clone(),
                audio_output_device: config.core.audio_output_device.clone(),
                sample_rate: config.core.audio_sample_rate,
                audio_denoiser: config.core.audio_denoiser.clone(),
                deep_filter_path: config.core.deep_filter_path.to_string_lossy().to_string(),
                deep_filter_atten_lim_db: config.core.deep_filter_atten_lim_db,
                post_tts_silence_ms: config.core.post_tts_silence_ms,
                record_secs: config.core.voice_record_secs,
                llm_model_path: config.core.llm_model_path.to_string_lossy().to_string(),
                wakeword_script: config.core.wakeword_script.to_string_lossy().to_string(),
                voice_continuous: config.core.voice_continuous,
                voice_continuous_secs: config.core.voice_continuous_secs,
                speaker_identity: genie_core::voice::identity::SpeakerIdentityProvider::from_config(
                    &config.core.speaker_identity,
                ),
            };
            genie_core::voice_loop::run(
                voice_cfg,
                &llm,
                &tool_dispatcher,
                &mem,
                &conversations,
                &system_prompt,
                config.core.max_history_turns,
                model_family,
            )
            .await
        }
        #[cfg(not(feature = "voice"))]
        {
            // Unreachable: voice_mode is forced false above when the feature is off.
            unreachable!("voice_mode true with feature 'voice' disabled");
        }
    } else if interactive {
        tracing::info!("interactive mode — starting REPL");
        genie_core::repl::run(
            &llm,
            &tool_dispatcher,
            &mem,
            &conversations,
            &system_prompt,
            config.core.max_history_turns,
            model_family,
        )
        .await
    } else {
        // Daemon mode: run HTTP server.
        let chat_server = genie_core::server::ChatServer::new(
            llm,
            tool_dispatcher,
            connectivity,
            mem,
            conversations,
            system_prompt,
            config.core.max_history_turns,
            model_family,
            config.core.expected_runtime_contract_hash.clone(),
        )?;

        tracing::info!(port, "starting HTTP chat API");
        if config.telegram.enabled {
            #[cfg(not(feature = "telegram"))]
            {
                tracing::warn!(
                    "telegram is enabled in config but this genie-core build does not include the 'telegram' feature"
                );
                return chat_server.serve(&bind_host, port).await;
            }

            #[cfg(feature = "telegram")]
            {
                let Some(bot_token) = config.telegram_bot_token() else {
                    tracing::warn!(
                        "telegram enabled but no bot token configured; skipping adapter"
                    );
                    return chat_server.serve(&bind_host, port).await;
                };

                if !config.telegram.allow_all_chats && config.telegram.allowed_chat_ids.is_empty() {
                    tracing::warn!(
                        "telegram enabled with no allowed_chat_ids; set allow_all_chats=true or configure an allowlist"
                    );
                }

                let telegram_cfg = genie_core::telegram::TelegramRuntimeConfig {
                    api_base: config.telegram.api_base.clone(),
                    bot_token,
                    core_base_url: format!("http://{}:{port}", local_http_host(&bind_host)),
                    poll_timeout_secs: config.telegram.poll_timeout_secs,
                    allowed_chat_ids: config.telegram.allowed_chat_ids.clone(),
                    allow_all_chats: config.telegram.allow_all_chats,
                    voice: genie_core::telegram::TelegramVoiceRuntimeConfig {
                        enabled: config.telegram.voice.enabled,
                        max_voice_duration_secs: config.telegram.voice.max_voice_duration_secs,
                        delete_temp_audio: config.telegram.voice.delete_temp_audio,
                        ffmpeg_path: config.telegram.voice.ffmpeg_path.clone(),
                        whisper_port: config.core.whisper_port,
                        whisper_cli_path: config.core.whisper_cli_path.clone(),
                        whisper_model: config.core.whisper_model.clone(),
                        stt_language: config.core.stt_language.clone(),
                        reply_as_voice: config.telegram.voice.reply_as_voice,
                        max_reply_chars: config.telegram.voice.max_reply_chars,
                        piper_path: config.core.piper_path.clone(),
                        piper_model: config.core.piper_model.clone(),
                    },
                };

                tracing::info!(
                    poll_timeout_secs = telegram_cfg.poll_timeout_secs,
                    allowed_chats = telegram_cfg.allowed_chat_ids.len(),
                    allow_all_chats = telegram_cfg.allow_all_chats,
                    "starting Telegram adapter"
                );

                tokio::try_join!(
                    chat_server.serve(&bind_host, port),
                    genie_core::telegram::run(telegram_cfg)
                )?;
                Ok(())
            }
        } else {
            chat_server.serve(&bind_host, port).await
        }
    }
}

// Only used by the telegram adapter (issue #41: dead in chat-only / no-telegram
// builds, so gate the definition together with its only caller).
#[cfg(feature = "telegram")]
fn local_http_host(bind_host: &str) -> String {
    let bind_host = bind_host.trim();
    if bind_host.is_empty() || matches!(bind_host, "0.0.0.0" | "::") {
        "127.0.0.1".into()
    } else if bind_host.contains(':') && !bind_host.starts_with('[') {
        format!("[{}]", bind_host)
    } else {
        bind_host.into()
    }
}

/// Check if stdin is a terminal (interactive) or a pipe/systemd.
fn atty_check() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(0) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// The only tests in this bin target are local_http_host_* which exercise the
// telegram adapter's bind-host helper. Gate the whole module so chat-only /
// no-telegram builds don't emit an `unused_imports` lint on `use super::*;`.
#[cfg(all(test, feature = "telegram"))]
mod tests {
    use super::*;

    #[test]
    fn local_http_host_uses_loopback_for_wildcard_binds() {
        assert_eq!(local_http_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(local_http_host("::"), "127.0.0.1");
        assert_eq!(local_http_host(""), "127.0.0.1");
    }

    #[test]
    fn local_http_host_brackets_ipv6_literals() {
        assert_eq!(local_http_host("::1"), "[::1]");
        assert_eq!(local_http_host("[::1]"), "[::1]");
    }
}
