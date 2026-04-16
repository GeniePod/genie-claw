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
    let llm_url = &config.services.llm.url;
    let llm = llm::LlmClient::from_url(llm_url);

    let ha = ha::provider_from_config(&config);

    let mem_path = config.data_dir.join("memory.db");
    let mem = memory::Memory::open(&mem_path)?;
    tracing::info!(memories = mem.count()?, "memory loaded");

    let mem_arc = Arc::new(std::sync::Mutex::new(memory::Memory::open(&mem_path)?));
    let skill_loader = skills::load_all();

    let tool_dispatcher = tools::ToolDispatcher::new(ha)
        .with_memory(Arc::clone(&mem_arc))
        .with_skill_loader(skill_loader);

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
    let prompt_builder = prompt::PromptBuilder::from_model_name(model_name);
    let system_prompt = prompt_builder.build(&tool_dispatcher.tool_defs(), &mem);
    tracing::info!(
        model = model_name,
        family = ?prompt::ModelFamily::from_model_name(model_name),
        "system prompt built"
    );

    // Check if stdin is a terminal (REPL mode) or pipe/systemd (server-only).
    let interactive = atty_check();

    // Open conversation store.
    let conv_path = config.data_dir.join("conversations.db");
    let conversations = conversation::ConversationStore::open(&conv_path)?;
    let conv_list = conversations.list()?;
    tracing::info!(conversations = conv_list.len(), "conversation store loaded");

    // Check for voice mode: --voice flag or GENIEPOD_VOICE=1 or config voice_enabled.
    let voice_mode = std::env::args().any(|a| a == "--voice")
        || std::env::var("GENIEPOD_VOICE").unwrap_or_default() == "1"
        || config.core.voice_enabled;

    if voice_mode {
        tracing::info!("voice mode — starting voice interaction loop");
        let voice_cfg = genie_core::voice_loop::VoiceConfig {
            whisper_model: config.core.whisper_model.to_string_lossy().to_string(),
            whisper_cli_path: config.core.whisper_cli_path.to_string_lossy().to_string(),
            piper_model: config.core.piper_model.to_string_lossy().to_string(),
            piper_path: config.core.piper_path.to_string_lossy().to_string(),
            audio_device: config.core.audio_device.clone(),
            sample_rate: config.core.audio_sample_rate,
            record_secs: config.core.voice_record_secs,
            llm_model_path: config.core.llm_model_path.to_string_lossy().to_string(),
            wakeword_script: config.core.wakeword_script.to_string_lossy().to_string(),
            voice_continuous: config.core.voice_continuous,
            voice_continuous_secs: config.core.voice_continuous_secs,
        };
        genie_core::voice_loop::run(
            voice_cfg,
            &llm,
            &tool_dispatcher,
            &mem,
            &conversations,
            &system_prompt,
            config.core.max_history_turns,
        )
        .await
    } else if interactive {
        tracing::info!("interactive mode — starting REPL");
        genie_core::repl::run(
            &llm,
            &tool_dispatcher,
            &mem,
            &conversations,
            &system_prompt,
            config.core.max_history_turns,
        )
        .await
    } else {
        // Daemon mode: run HTTP server.
        let chat_server = genie_core::server::ChatServer::new(
            llm,
            tool_dispatcher,
            mem,
            conversations,
            system_prompt,
            config.core.max_history_turns,
        )?;

        tracing::info!(port, "starting HTTP chat API");
        chat_server.serve(port).await
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
