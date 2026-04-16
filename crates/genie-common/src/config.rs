use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Top-level GeniePod system configuration.
///
/// Loaded from `/etc/geniepod/geniepod.toml` on the device.
/// Developers can override with `GENIEPOD_CONFIG` env var.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "defaults::data_dir")]
    pub data_dir: PathBuf,

    #[serde(default)]
    pub core: CoreConfig,

    #[serde(default)]
    pub governor: GovernorConfig,

    #[serde(default)]
    pub health: HealthConfig,

    #[serde(default)]
    pub services: ServicesConfig,
}

#[derive(Debug, Deserialize)]
pub struct CoreConfig {
    /// HTTP API port for genie-core.
    #[serde(default = "defaults::core_port")]
    pub port: u16,

    /// Home Assistant long-lived access token.
    /// Can also be set via HA_TOKEN env var.
    #[serde(default)]
    pub ha_token: String,

    /// LLM model name (for prompt optimization). Auto-detected from filename.
    #[serde(default = "defaults::llm_model_name")]
    pub llm_model_name: String,

    /// Whisper model path.
    #[serde(default = "defaults::whisper_model")]
    pub whisper_model: PathBuf,

    /// Whisper server port (0 = CLI mode).
    #[serde(default)]
    pub whisper_port: u16,

    /// Piper TTS model path.
    #[serde(default = "defaults::piper_model")]
    pub piper_model: PathBuf,

    /// Use pipe mode for TTS (lower latency, long-running subprocess).
    #[serde(default = "defaults::piper_pipe_mode")]
    pub piper_pipe_mode: bool,

    /// Max conversation history turns to keep.
    #[serde(default = "defaults::max_history_turns")]
    pub max_history_turns: usize,

    /// Path to whisper-cli binary.
    #[serde(default = "defaults::whisper_cli_path")]
    pub whisper_cli_path: PathBuf,

    /// Path to Piper TTS binary.
    #[serde(default = "defaults::piper_path")]
    pub piper_path: PathBuf,

    /// ALSA audio device for mic/speaker (e.g. "plughw:0,0").
    #[serde(default = "defaults::audio_device")]
    pub audio_device: String,

    /// Audio capture sample rate (Hz). USB headphones typically need 48000.
    #[serde(default = "defaults::audio_sample_rate")]
    pub audio_sample_rate: u32,

    /// Enable voice mode (mic → STT → LLM → TTS → speaker loop).
    #[serde(default)]
    pub voice_enabled: bool,

    /// Voice recording duration in seconds.
    #[serde(default = "defaults::voice_record_secs")]
    pub voice_record_secs: u32,

    /// Enable continuous conversation (auto-listen after response without re-wake).
    #[serde(default)]
    pub voice_continuous: bool,

    /// Recording duration for follow-up in continuous mode (shorter than initial).
    #[serde(default = "defaults::voice_continuous_secs")]
    pub voice_continuous_secs: u32,

    /// LLM model path (for GPU time-sharing — voice loop restarts llama-server).
    #[serde(default = "defaults::llm_model_path")]
    pub llm_model_path: PathBuf,

    /// Path to the wake word listener script (empty = push-to-talk mode).
    #[serde(default = "defaults::wakeword_script")]
    pub wakeword_script: PathBuf,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            port: defaults::core_port(),
            ha_token: String::new(),
            llm_model_name: defaults::llm_model_name(),
            whisper_model: defaults::whisper_model(),
            whisper_port: 0,
            piper_model: defaults::piper_model(),
            piper_pipe_mode: defaults::piper_pipe_mode(),
            max_history_turns: defaults::max_history_turns(),
            whisper_cli_path: defaults::whisper_cli_path(),
            piper_path: defaults::piper_path(),
            audio_device: defaults::audio_device(),
            audio_sample_rate: defaults::audio_sample_rate(),
            voice_enabled: false,
            voice_record_secs: defaults::voice_record_secs(),
            voice_continuous: true,
            voice_continuous_secs: defaults::voice_continuous_secs(),
            llm_model_path: defaults::llm_model_path(),
            wakeword_script: defaults::wakeword_script(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GovernorConfig {
    /// How often to sample tegrastats and /proc/meminfo (ms).
    #[serde(default = "defaults::poll_interval_ms")]
    pub poll_interval_ms: u64,

    /// Hour (0-23) when night mode begins.
    #[serde(default = "defaults::night_start_hour")]
    pub night_start_hour: u8,

    /// Hour (0-23) when day mode resumes.
    #[serde(default = "defaults::day_start_hour")]
    pub day_start_hour: u8,

    /// Enable night mode model swap (Nemotron 4B → 9B).
    #[serde(default)]
    pub night_model_swap: bool,

    /// Memory pressure thresholds (MB available).
    #[serde(default)]
    pub pressure: PressureConfig,
}

#[derive(Debug, Deserialize)]
pub struct PressureConfig {
    /// Stop opt-in Docker containers below this threshold (MB).
    #[serde(default = "defaults::pressure_stop_optins_mb")]
    pub stop_optins_mb: u64,

    /// Reduce LLM context cap below this threshold (MB).
    #[serde(default = "defaults::pressure_reduce_context_mb")]
    pub reduce_context_mb: u64,

    /// Swap STT to whisper-tiny below this threshold (MB).
    #[serde(default = "defaults::pressure_swap_stt_mb")]
    pub swap_stt_mb: u64,

    /// Enable zram below this threshold (MB).
    #[serde(default = "defaults::pressure_zram_mb")]
    pub zram_mb: u64,
}

#[derive(Debug, Deserialize)]
pub struct HealthConfig {
    /// How often to poll service health endpoints (seconds).
    #[serde(default = "defaults::health_interval_secs")]
    pub interval_secs: u64,

    /// Forward alerts to an optional local webhook on service failure.
    #[serde(default = "defaults::health_alert_enabled")]
    pub alert_enabled: bool,

    /// Local webhook base URL for alert forwarding.
    #[serde(default = "defaults::alert_webhook_url")]
    pub alert_webhook_url: String,
}

#[derive(Debug, Deserialize)]
pub struct ServicesConfig {
    pub core: ServiceEndpoint,
    pub llm: ServiceEndpoint,
    pub homeassistant: Option<ServiceEndpoint>,

    #[serde(default)]
    pub nextcloud: Option<ServiceEndpoint>,

    #[serde(default)]
    pub jellyfin: Option<ServiceEndpoint>,
}

#[derive(Debug, Deserialize)]
pub struct ServiceEndpoint {
    pub url: String,
    pub systemd_unit: String,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let path = std::env::var("GENIEPOD_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/geniepod/geniepod.toml"));

        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config {}: {}", path.display(), e))?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Resolve the configured Home Assistant endpoint, if this deployment uses one.
    pub fn homeassistant_service(&self) -> Option<&ServiceEndpoint> {
        self.services.homeassistant.as_ref()
    }

    /// Resolve the Home Assistant token from config first, then the environment.
    pub fn homeassistant_token(&self) -> Option<String> {
        let token = if self.core.ha_token.is_empty() {
            std::env::var("HA_TOKEN").unwrap_or_default()
        } else {
            self.core.ha_token.clone()
        };

        let token = token.trim().to_string();
        if token.is_empty() { None } else { Some(token) }
    }

    /// Whether the current deployment should manage a given service alias.
    pub fn manages_service_alias(&self, alias: &str) -> bool {
        match alias {
            "homeassistant" => self.services.homeassistant.is_some(),
            "nextcloud" => self.services.nextcloud.is_some(),
            "jellyfin" => self.services.jellyfin.is_some(),
            _ => true,
        }
    }
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: defaults::poll_interval_ms(),
            night_start_hour: defaults::night_start_hour(),
            day_start_hour: defaults::day_start_hour(),
            night_model_swap: false,
            pressure: PressureConfig::default(),
        }
    }
}

impl Default for PressureConfig {
    fn default() -> Self {
        Self {
            stop_optins_mb: defaults::pressure_stop_optins_mb(),
            reduce_context_mb: defaults::pressure_reduce_context_mb(),
            swap_stt_mb: defaults::pressure_swap_stt_mb(),
            zram_mb: defaults::pressure_zram_mb(),
        }
    }
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            interval_secs: defaults::health_interval_secs(),
            alert_enabled: defaults::health_alert_enabled(),
            alert_webhook_url: defaults::alert_webhook_url(),
        }
    }
}

impl Default for ServicesConfig {
    fn default() -> Self {
        Self {
            core: ServiceEndpoint {
                url: "http://127.0.0.1:3000/api/health".into(),
                systemd_unit: "genie-core.service".into(),
            },
            llm: ServiceEndpoint {
                url: "http://127.0.0.1:8080/health".into(),
                systemd_unit: "genie-llm.service".into(),
            },
            homeassistant: None,
            nextcloud: None,
            jellyfin: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            data_dir: defaults::data_dir(),
            core: CoreConfig::default(),
            governor: GovernorConfig::default(),
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
        }
    }

    #[test]
    fn homeassistant_is_optional_by_default() {
        let config = test_config();
        assert!(config.homeassistant_service().is_none());
        assert!(!config.manages_service_alias("homeassistant"));
    }

    #[test]
    fn configured_homeassistant_token_is_used() {
        let mut config = test_config();
        config.core.ha_token = "secret-token".into();

        assert_eq!(
            config.homeassistant_token().as_deref(),
            Some("secret-token")
        );
    }

    #[test]
    fn only_configured_optional_services_are_managed() {
        let mut config = test_config();
        config.services.nextcloud = Some(ServiceEndpoint {
            url: "http://127.0.0.1:8180/status.php".into(),
            systemd_unit: "nextcloud.service".into(),
        });

        assert!(config.manages_service_alias("genie-core"));
        assert!(!config.manages_service_alias("homeassistant"));
        assert!(config.manages_service_alias("nextcloud"));
        assert!(!config.manages_service_alias("jellyfin"));
    }
}

mod defaults {
    use std::path::PathBuf;

    pub fn data_dir() -> PathBuf {
        PathBuf::from("/opt/geniepod/data")
    }
    pub fn poll_interval_ms() -> u64 {
        5000
    }
    pub fn night_start_hour() -> u8 {
        23
    }
    pub fn day_start_hour() -> u8 {
        6
    }
    pub fn pressure_stop_optins_mb() -> u64 {
        500
    }
    pub fn pressure_reduce_context_mb() -> u64 {
        300
    }
    pub fn pressure_swap_stt_mb() -> u64 {
        200
    }
    pub fn pressure_zram_mb() -> u64 {
        100
    }
    pub fn health_interval_secs() -> u64 {
        30
    }
    pub fn health_alert_enabled() -> bool {
        false
    }
    pub fn alert_webhook_url() -> String {
        String::new()
    }
    pub fn core_port() -> u16 {
        3000
    }
    pub fn llm_model_name() -> String {
        "nemotron-4b".into()
    }
    pub fn whisper_model() -> PathBuf {
        PathBuf::from("/opt/geniepod/models/whisper-small.bin")
    }
    pub fn piper_model() -> PathBuf {
        PathBuf::from("/opt/geniepod/voices/en_US-amy-medium.onnx")
    }
    pub fn piper_pipe_mode() -> bool {
        false
    }
    pub fn max_history_turns() -> usize {
        20
    }
    pub fn whisper_cli_path() -> PathBuf {
        PathBuf::from("/opt/geniepod/bin/whisper-cli")
    }
    pub fn piper_path() -> PathBuf {
        PathBuf::from("/opt/geniepod/piper/piper")
    }
    pub fn audio_device() -> String {
        "auto".into()
    }
    pub fn audio_sample_rate() -> u32 {
        48000
    }
    pub fn voice_record_secs() -> u32 {
        5
    }
    pub fn voice_continuous_secs() -> u32 {
        3
    }
    pub fn llm_model_path() -> PathBuf {
        PathBuf::from("/opt/geniepod/models/nemotron-4b-q4_k_m.gguf")
    }
    pub fn wakeword_script() -> PathBuf {
        PathBuf::from("/opt/geniepod/bin/genie-wake-listen.py")
    }
}
