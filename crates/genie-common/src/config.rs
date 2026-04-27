use serde::Deserialize;
use std::collections::HashMap;
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

    #[serde(default)]
    pub telegram: TelegramConfig,

    #[serde(default)]
    pub web_search: WebSearchConfig,

    #[serde(default)]
    pub connectivity: ConnectivityConfig,
}

#[derive(Debug, Deserialize)]
pub struct CoreConfig {
    /// HTTP API port for genie-core.
    #[serde(default = "defaults::core_port")]
    pub port: u16,

    /// HTTP bind host for genie-core.
    ///
    /// Defaults to localhost because this API can trigger physical actions.
    /// Use 0.0.0.0 only behind a trusted LAN, firewall, or first-party gateway.
    #[serde(default = "defaults::core_bind_host")]
    pub bind_host: String,

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

    /// Optional pinned runtime contract hash for drift detection.
    #[serde(default)]
    pub expected_runtime_contract_hash: String,

    /// Path to whisper-cli binary.
    #[serde(default = "defaults::whisper_cli_path")]
    pub whisper_cli_path: PathBuf,

    /// Path to Piper TTS binary.
    #[serde(default = "defaults::piper_path")]
    pub piper_path: PathBuf,

    /// Whisper transcription language. Use "auto" for auto-detection.
    #[serde(default = "defaults::stt_language")]
    pub stt_language: String,

    /// Optional Piper voices keyed by language code, e.g. "en", "es", "de", "zh".
    #[serde(default)]
    pub voice_tts_models: HashMap<String, PathBuf>,

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

    /// Optional speaker identity provider for voice memory context.
    #[serde(default)]
    pub speaker_identity: SpeakerIdentityConfig,

    /// Runtime policy for loadable native skills.
    #[serde(default)]
    pub skill_policy: SkillPolicyConfig,

    /// Runtime policy for model-callable tools by request origin.
    #[serde(default)]
    pub tool_policy: ToolPolicyConfig,

    /// Final actuation safety gate for home-control execution.
    #[serde(default)]
    pub actuation_safety: ActuationSafetyConfig,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            port: defaults::core_port(),
            bind_host: defaults::core_bind_host(),
            ha_token: String::new(),
            llm_model_name: defaults::llm_model_name(),
            whisper_model: defaults::whisper_model(),
            whisper_port: 0,
            piper_model: defaults::piper_model(),
            piper_pipe_mode: defaults::piper_pipe_mode(),
            max_history_turns: defaults::max_history_turns(),
            expected_runtime_contract_hash: String::new(),
            whisper_cli_path: defaults::whisper_cli_path(),
            piper_path: defaults::piper_path(),
            stt_language: defaults::stt_language(),
            voice_tts_models: HashMap::new(),
            audio_device: defaults::audio_device(),
            audio_sample_rate: defaults::audio_sample_rate(),
            voice_enabled: false,
            voice_record_secs: defaults::voice_record_secs(),
            voice_continuous: true,
            voice_continuous_secs: defaults::voice_continuous_secs(),
            llm_model_path: defaults::llm_model_path(),
            wakeword_script: defaults::wakeword_script(),
            speaker_identity: SpeakerIdentityConfig::default(),
            skill_policy: SkillPolicyConfig::default(),
            tool_policy: ToolPolicyConfig::default(),
            actuation_safety: ActuationSafetyConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SkillPolicyConfig {
    /// Reject skills without a valid sidecar manifest.
    #[serde(default)]
    pub require_manifest: bool,

    /// Reject skills whose manifest does not declare signature material.
    #[serde(default)]
    pub require_signature: bool,

    /// Reject skills requesting any of these permission labels.
    #[serde(default)]
    pub denied_permissions: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ToolPolicyConfig {
    /// Enable runtime tool allow/deny checks.
    #[serde(default = "defaults::tool_policy_enabled")]
    pub enabled: bool,

    /// If an origin has an allowlist, only those tools can run from that origin.
    #[serde(default)]
    pub allowed_tools_by_origin: HashMap<String, Vec<String>>,

    /// Tools blocked by origin. Deny rules override allow rules.
    #[serde(default)]
    pub denied_tools_by_origin: HashMap<String, Vec<String>>,
}

impl Default for ToolPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::tool_policy_enabled(),
            allowed_tools_by_origin: HashMap::new(),
            denied_tools_by_origin: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ActuationSafetyConfig {
    #[serde(default = "defaults::actuation_safety_enabled")]
    pub enabled: bool,

    #[serde(default = "defaults::actuation_min_target_confidence")]
    pub min_target_confidence: f32,

    #[serde(default = "defaults::actuation_min_sensitive_confidence")]
    pub min_sensitive_confidence: f32,

    #[serde(default = "defaults::actuation_deny_multi_target_sensitive")]
    pub deny_multi_target_sensitive: bool,

    #[serde(default = "defaults::actuation_require_available_state")]
    pub require_available_state: bool,

    #[serde(default = "defaults::actuation_allowed_origins")]
    pub allowed_origins: Vec<String>,

    #[serde(default = "defaults::actuation_max_actions_per_minute")]
    pub max_actions_per_minute: usize,

    #[serde(default)]
    pub max_actions_per_minute_by_origin: HashMap<String, usize>,
}

impl Default for ActuationSafetyConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::actuation_safety_enabled(),
            min_target_confidence: defaults::actuation_min_target_confidence(),
            min_sensitive_confidence: defaults::actuation_min_sensitive_confidence(),
            deny_multi_target_sensitive: defaults::actuation_deny_multi_target_sensitive(),
            require_available_state: defaults::actuation_require_available_state(),
            allowed_origins: defaults::actuation_allowed_origins(),
            max_actions_per_minute: defaults::actuation_max_actions_per_minute(),
            max_actions_per_minute_by_origin: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SpeakerIdentityConfig {
    /// Enable speaker identity enrichment for voice flows.
    #[serde(default)]
    pub enabled: bool,

    /// Identity provider implementation.
    #[serde(default)]
    pub provider: SpeakerIdentityProvider,

    /// Fixed speaker label for single-user or test deployments.
    #[serde(default)]
    pub fixed_name: String,

    /// Confidence to report for the fixed provider.
    #[serde(default = "defaults::speaker_identity_confidence")]
    pub fixed_confidence: String,

    /// Local speaker profile directory for biometric recognition.
    #[serde(default = "defaults::speaker_identity_profile_dir")]
    pub local_profile_dir: PathBuf,

    /// Minimum score for accepting a local biometric match.
    #[serde(default = "defaults::speaker_identity_min_score")]
    pub local_min_score: f32,
}

impl Default for SpeakerIdentityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: SpeakerIdentityProvider::None,
            fixed_name: String::new(),
            fixed_confidence: defaults::speaker_identity_confidence(),
            local_profile_dir: defaults::speaker_identity_profile_dir(),
            local_min_score: defaults::speaker_identity_min_score(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerIdentityProvider {
    #[default]
    None,
    Fixed,
    LocalBiometric,
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
pub struct TelegramConfig {
    /// Enable Telegram long-poll channel integration.
    #[serde(default)]
    pub enabled: bool,

    /// Telegram Bot API token. Can also be provided via TELEGRAM_BOT_TOKEN.
    #[serde(default)]
    pub bot_token: String,

    /// Optional Telegram Bot API base URL.
    #[serde(default = "defaults::telegram_api_base")]
    pub api_base: String,

    /// Long-poll timeout passed to getUpdates.
    #[serde(default = "defaults::telegram_poll_timeout_secs")]
    pub poll_timeout_secs: u64,

    /// Explicit allowlist of Telegram chat IDs allowed to talk to GenieClaw.
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,

    /// Bypass the allowlist and accept messages from any chat.
    #[serde(default)]
    pub allow_all_chats: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WebSearchConfig {
    /// Enable public web search tools.
    #[serde(default = "defaults::web_search_enabled")]
    pub enabled: bool,

    /// No-key provider backend.
    #[serde(default)]
    pub provider: WebSearchProvider,

    /// Optional provider base URL. Required for SearXNG unless GENIEPOD_WEB_SEARCH_BASE_URL is set.
    #[serde(default)]
    pub base_url: String,

    /// Allow SearXNG base_url to point to a non-localhost service.
    #[serde(default)]
    pub allow_remote_base_url: bool,

    /// Request timeout in seconds.
    #[serde(default = "defaults::web_search_timeout_secs")]
    pub timeout_secs: u64,

    /// Upper bound for returned results.
    #[serde(default = "defaults::web_search_max_results")]
    pub max_results: usize,

    /// Cache successful search responses in-process to reduce repeated network calls.
    #[serde(default = "defaults::web_search_cache_enabled")]
    pub cache_enabled: bool,

    /// How long cached search responses remain fresh.
    #[serde(default = "defaults::web_search_cache_ttl_secs")]
    pub cache_ttl_secs: u64,

    /// Maximum number of cached search responses kept in memory.
    #[serde(default = "defaults::web_search_cache_max_entries")]
    pub cache_max_entries: usize,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProvider {
    #[default]
    Duckduckgo,
    Searxng,
}

#[derive(Debug, Deserialize)]
pub struct ConnectivityConfig {
    /// Enable the external connectivity coprocessor path.
    #[serde(default)]
    pub enabled: bool,

    /// Transport used to talk to the connectivity coprocessor.
    #[serde(default)]
    pub transport: ConnectivityTransport,

    /// Optional logical role name for the connected coprocessor.
    #[serde(default = "defaults::connectivity_device")]
    pub device: String,

    /// ESP32-C6 over UART transport settings.
    #[serde(default, alias = "esp32c6_spi")]
    pub esp32c6_uart: Esp32C6UartConfig,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectivityTransport {
    #[default]
    None,
    #[serde(rename = "esp32c6_uart", alias = "esp32c6_spi")]
    Esp32c6Uart,
}

#[derive(Debug, Deserialize)]
pub struct Esp32C6UartConfig {
    /// Linux serial device exposed by the Jetson kernel.
    #[serde(default = "defaults::esp32c6_uart_device")]
    pub device_path: String,

    /// UART baud rate.
    #[serde(default = "defaults::esp32c6_uart_baud_rate")]
    pub baud_rate: u32,

    /// Optional GPIO used to hard-reset the ESP32-C6.
    #[serde(default)]
    pub reset_gpio: Option<u32>,

    /// Enable RTS/CTS hardware flow control if the wiring supports it.
    #[serde(default = "defaults::esp32c6_uart_hardware_flow_control")]
    pub hardware_flow_control: bool,

    /// Maximum UART payload size for one frame.
    #[serde(default = "defaults::esp32c6_uart_mtu_bytes")]
    pub mtu_bytes: usize,

    /// Timeout waiting for a response frame from the ESP32-C6.
    #[serde(default = "defaults::esp32c6_uart_response_timeout_ms")]
    pub response_timeout_ms: u64,
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

    /// Resolve the Telegram bot token from config first, then the environment.
    pub fn telegram_bot_token(&self) -> Option<String> {
        let token = if self.telegram.bot_token.is_empty() {
            std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default()
        } else {
            self.telegram.bot_token.clone()
        };

        let token = token.trim().to_string();
        if token.is_empty() { None } else { Some(token) }
    }

    pub fn connectivity_enabled(&self) -> bool {
        self.connectivity.enabled && self.connectivity.transport != ConnectivityTransport::None
    }

    /// Redacted posture for dashboards and support tools.
    ///
    /// This intentionally reports capability and risk state instead of raw TOML,
    /// file paths, endpoint URLs, tokens, or speaker labels.
    pub fn household_security_summary(&self) -> serde_json::Value {
        let mut risk_flags = Vec::new();

        if !matches!(
            self.core.bind_host.as_str(),
            "127.0.0.1" | "localhost" | "::1"
        ) {
            risk_flags.push("core_api_not_localhost");
        }
        if self.telegram.enabled && self.telegram.allow_all_chats {
            risk_flags.push("telegram_accepts_any_chat");
        }
        if self.telegram.enabled
            && !self.telegram.allow_all_chats
            && self.telegram.allowed_chat_ids.is_empty()
        {
            risk_flags.push("telegram_enabled_without_chat_allowlist");
        }
        if self.web_search.enabled
            && self.web_search.provider == WebSearchProvider::Searxng
            && self.web_search.allow_remote_base_url
        {
            risk_flags.push("web_search_remote_base_url_allowed");
        }
        if !self.core.tool_policy.enabled {
            risk_flags.push("tool_policy_disabled");
        }
        if !self.core.actuation_safety.enabled {
            risk_flags.push("actuation_safety_disabled");
        }
        if !self.core.ha_token.trim().is_empty() {
            risk_flags.push("homeassistant_token_in_config_file");
        }
        if self.telegram.enabled && !self.telegram.bot_token.trim().is_empty() {
            risk_flags.push("telegram_token_in_config_file");
        }
        if !self.core.skill_policy.require_manifest {
            risk_flags.push("skill_manifest_not_required");
        }
        if !self.core.skill_policy.require_signature {
            risk_flags.push("skill_signature_not_required");
        }

        serde_json::json!({
            "trust_model": "single_household_operator_boundary",
            "raw_config_exposed": false,
            "raw_config_policy": "local_operator_file_only",
            "shared_memory": {
                "mode": "household_shared_by_default",
                "dashboard_manager_enabled": true,
                "shared_room_safe_prompt_filtering": true,
                "speaker_identity_enabled": self.core.speaker_identity.enabled,
                "speaker_identity_provider": match self.core.speaker_identity.provider {
                    SpeakerIdentityProvider::None => "none",
                    SpeakerIdentityProvider::Fixed => "fixed",
                    SpeakerIdentityProvider::LocalBiometric => "local_biometric",
                },
                "speaker_label_exposed": false
            },
            "control_surfaces": {
                "core_api_local_only": matches!(self.core.bind_host.as_str(), "127.0.0.1" | "localhost" | "::1"),
                "dashboard_local_only": true,
                "telegram_enabled": self.telegram.enabled,
                "telegram_allowlist_enabled": self.telegram.enabled && !self.telegram.allow_all_chats && !self.telegram.allowed_chat_ids.is_empty(),
                "homeassistant_bridge_configured": self.services.homeassistant.is_some(),
                "connectivity_coprocessor_enabled": self.connectivity_enabled()
            },
            "policy": {
                "tool_policy_enabled": self.core.tool_policy.enabled,
                "actuation_safety_enabled": self.core.actuation_safety.enabled,
                "sensitive_multi_target_denied": self.core.actuation_safety.deny_multi_target_sensitive,
                "available_state_required": self.core.actuation_safety.require_available_state,
                "skill_manifest_required": self.core.skill_policy.require_manifest,
                "skill_signature_required": self.core.skill_policy.require_signature
            },
            "secret_presence": {
                "homeassistant_token_configured": self.homeassistant_token().is_some(),
                "homeassistant_token_source": if self.core.ha_token.trim().is_empty() { "environment_or_absent" } else { "config_file" },
                "telegram_token_configured": self.telegram_bot_token().is_some(),
                "telegram_token_source": if self.telegram.bot_token.trim().is_empty() { "environment_or_absent" } else { "config_file" }
            },
            "risk_flags": risk_flags
        })
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

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            api_base: defaults::telegram_api_base(),
            poll_timeout_secs: defaults::telegram_poll_timeout_secs(),
            allowed_chat_ids: Vec::new(),
            allow_all_chats: false,
        }
    }
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::web_search_enabled(),
            provider: WebSearchProvider::default(),
            base_url: String::new(),
            allow_remote_base_url: false,
            timeout_secs: defaults::web_search_timeout_secs(),
            max_results: defaults::web_search_max_results(),
            cache_enabled: defaults::web_search_cache_enabled(),
            cache_ttl_secs: defaults::web_search_cache_ttl_secs(),
            cache_max_entries: defaults::web_search_cache_max_entries(),
        }
    }
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: ConnectivityTransport::None,
            device: defaults::connectivity_device(),
            esp32c6_uart: Esp32C6UartConfig::default(),
        }
    }
}

impl Default for Esp32C6UartConfig {
    fn default() -> Self {
        Self {
            device_path: defaults::esp32c6_uart_device(),
            baud_rate: defaults::esp32c6_uart_baud_rate(),
            reset_gpio: None,
            hardware_flow_control: defaults::esp32c6_uart_hardware_flow_control(),
            mtu_bytes: defaults::esp32c6_uart_mtu_bytes(),
            response_timeout_ms: defaults::esp32c6_uart_response_timeout_ms(),
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
            telegram: TelegramConfig::default(),
            web_search: WebSearchConfig::default(),
            connectivity: ConnectivityConfig::default(),
        }
    }

    #[test]
    fn homeassistant_is_optional_by_default() {
        let config = test_config();
        assert!(config.homeassistant_service().is_none());
        assert!(!config.manages_service_alias("homeassistant"));
    }

    #[test]
    fn core_bind_host_defaults_to_localhost() {
        let config = test_config();
        assert_eq!(config.core.bind_host, "127.0.0.1");
    }

    #[test]
    fn core_bind_host_can_be_configured() {
        let config: CoreConfig = toml::from_str(
            r#"
port = 3001
bind_host = "0.0.0.0"
"#,
        )
        .unwrap();

        assert_eq!(config.port, 3001);
        assert_eq!(config.bind_host, "0.0.0.0");
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

    #[test]
    fn configured_telegram_token_is_used() {
        let mut config = test_config();
        config.telegram.bot_token = "telegram-secret".into();

        assert_eq!(
            config.telegram_bot_token().as_deref(),
            Some("telegram-secret")
        );
    }

    #[test]
    fn web_search_defaults_to_enabled_duckduckgo() {
        let config = test_config();
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.provider, WebSearchProvider::Duckduckgo);
        assert_eq!(config.web_search.max_results, 3);
        assert!(config.web_search.cache_enabled);
        assert_eq!(config.web_search.cache_ttl_secs, 900);
    }

    #[test]
    fn web_search_config_parses_searxng() {
        let config: WebSearchConfig = toml::from_str(
            r#"
enabled = true
provider = "searxng"
base_url = "http://127.0.0.1:8888"
allow_remote_base_url = true
timeout_secs = 2
max_results = 5
cache_enabled = false
cache_ttl_secs = 60
cache_max_entries = 12
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.provider, WebSearchProvider::Searxng);
        assert_eq!(config.base_url, "http://127.0.0.1:8888");
        assert!(config.allow_remote_base_url);
        assert_eq!(config.timeout_secs, 2);
        assert_eq!(config.max_results, 5);
        assert!(!config.cache_enabled);
        assert_eq!(config.cache_ttl_secs, 60);
        assert_eq!(config.cache_max_entries, 12);
    }

    #[test]
    fn speaker_identity_defaults_to_disabled_none() {
        let config = test_config();
        assert!(!config.core.speaker_identity.enabled);
        assert_eq!(
            config.core.speaker_identity.provider,
            SpeakerIdentityProvider::None
        );
        assert!(config.core.speaker_identity.fixed_name.is_empty());
        assert_eq!(config.core.speaker_identity.fixed_confidence, "high");
        assert_eq!(
            config.core.speaker_identity.local_profile_dir,
            defaults::speaker_identity_profile_dir()
        );
        assert_eq!(config.core.speaker_identity.local_min_score, 0.82);
    }

    #[test]
    fn speaker_identity_config_parses_fixed_provider() {
        let config: SpeakerIdentityConfig = toml::from_str(
            r#"
enabled = true
provider = "fixed"
fixed_name = "Jared"
fixed_confidence = "medium"
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.provider, SpeakerIdentityProvider::Fixed);
        assert_eq!(config.fixed_name, "Jared");
        assert_eq!(config.fixed_confidence, "medium");
    }

    #[test]
    fn speaker_identity_config_parses_local_biometric_provider() {
        let config: SpeakerIdentityConfig = toml::from_str(
            r#"
enabled = true
provider = "local_biometric"
local_profile_dir = "/opt/geniepod/data/speakers"
local_min_score = 0.91
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.provider, SpeakerIdentityProvider::LocalBiometric);
        assert_eq!(
            config.local_profile_dir,
            PathBuf::from("/opt/geniepod/data/speakers")
        );
        assert!((config.local_min_score - 0.91).abs() < f32::EPSILON);
    }

    #[test]
    fn skill_policy_defaults_to_audit_only() {
        let config = test_config();
        assert!(!config.core.skill_policy.require_manifest);
        assert!(!config.core.skill_policy.require_signature);
        assert!(config.core.skill_policy.denied_permissions.is_empty());
    }

    #[test]
    fn skill_policy_config_parses() {
        let config: SkillPolicyConfig = toml::from_str(
            r#"
require_manifest = true
require_signature = true
denied_permissions = ["network.raw", "filesystem.write"]
"#,
        )
        .unwrap();

        assert!(config.require_manifest);
        assert!(config.require_signature);
        assert_eq!(
            config.denied_permissions,
            vec!["network.raw", "filesystem.write"]
        );
    }

    #[test]
    fn tool_policy_defaults_to_enabled_without_rules() {
        let config = test_config();
        assert!(config.core.tool_policy.enabled);
        assert!(config.core.tool_policy.allowed_tools_by_origin.is_empty());
        assert!(config.core.tool_policy.denied_tools_by_origin.is_empty());
    }

    #[test]
    fn tool_policy_config_parses() {
        let config: ToolPolicyConfig = toml::from_str(
            r#"
enabled = true
allowed_tools_by_origin = { telegram = ["get_time", "memory_recall"] }
denied_tools_by_origin = { voice = ["web_search"], "*" = ["play_media"] }
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(
            config.allowed_tools_by_origin["telegram"],
            vec!["get_time", "memory_recall"]
        );
        assert_eq!(config.denied_tools_by_origin["voice"], vec!["web_search"]);
        assert_eq!(config.denied_tools_by_origin["*"], vec!["play_media"]);
    }

    #[test]
    fn actuation_safety_defaults_to_enabled_fail_closed_settings() {
        let config = test_config();
        assert!(config.core.actuation_safety.enabled);
        assert!((config.core.actuation_safety.min_target_confidence - 0.78).abs() < f32::EPSILON);
        assert!(
            (config.core.actuation_safety.min_sensitive_confidence - 0.90).abs() < f32::EPSILON
        );
        assert!(config.core.actuation_safety.deny_multi_target_sensitive);
        assert!(config.core.actuation_safety.require_available_state);
        assert!(
            config
                .core
                .actuation_safety
                .allowed_origins
                .contains(&"voice".to_string())
        );
        assert!(
            !config
                .core
                .actuation_safety
                .allowed_origins
                .contains(&"unknown".to_string())
        );
        assert_eq!(config.core.actuation_safety.max_actions_per_minute, 12);
    }

    #[test]
    fn actuation_safety_config_parses() {
        let config: ActuationSafetyConfig = toml::from_str(
            r#"
enabled = true
min_target_confidence = 0.81
min_sensitive_confidence = 0.95
deny_multi_target_sensitive = false
require_available_state = false
allowed_origins = ["dashboard", "confirmation"]
max_actions_per_minute = 4
max_actions_per_minute_by_origin = { telegram = 1, voice = 2 }
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!((config.min_target_confidence - 0.81).abs() < f32::EPSILON);
        assert!((config.min_sensitive_confidence - 0.95).abs() < f32::EPSILON);
        assert!(!config.deny_multi_target_sensitive);
        assert!(!config.require_available_state);
        assert_eq!(config.allowed_origins, vec!["dashboard", "confirmation"]);
        assert_eq!(config.max_actions_per_minute, 4);
        assert_eq!(config.max_actions_per_minute_by_origin["telegram"], 1);
        assert_eq!(config.max_actions_per_minute_by_origin["voice"], 2);
    }

    #[test]
    fn core_config_parses_expected_runtime_contract_hash() {
        let config: CoreConfig = toml::from_str(
            r#"
expected_runtime_contract_hash = "abc123"
"#,
        )
        .unwrap();

        assert_eq!(config.expected_runtime_contract_hash, "abc123");
    }

    #[test]
    fn connectivity_is_disabled_by_default() {
        let config = test_config();
        assert!(!config.connectivity_enabled());
        assert_eq!(config.connectivity.transport, ConnectivityTransport::None);
        assert_eq!(config.connectivity.device, "esp32c6");
    }

    #[test]
    fn connectivity_requires_non_none_transport() {
        let mut config = test_config();
        config.connectivity.enabled = true;
        assert!(!config.connectivity_enabled());

        config.connectivity.transport = ConnectivityTransport::Esp32c6Uart;
        assert!(config.connectivity_enabled());
    }

    #[test]
    fn household_security_summary_redacts_raw_config() {
        let mut config = test_config();
        config.telegram.enabled = true;
        config.telegram.bot_token = "telegram-secret".into();
        config.telegram.allow_all_chats = true;
        config.core.ha_token = "ha-secret".into();

        let summary = config.household_security_summary();

        assert_eq!(summary["raw_config_exposed"], false);
        assert_eq!(summary["shared_memory"]["speaker_label_exposed"], false);
        assert_eq!(
            summary["secret_presence"]["homeassistant_token_configured"],
            true
        );
        assert_eq!(
            summary["secret_presence"]["telegram_token_configured"],
            true
        );
        let text = summary.to_string();
        assert!(!text.contains("telegram-secret"));
        assert!(!text.contains("ha-secret"));
        assert!(text.contains("telegram_accepts_any_chat"));
        assert!(text.contains("homeassistant_token_in_config_file"));
    }

    #[test]
    fn legacy_spi_connectivity_config_still_parses() {
        let config: ConnectivityConfig = toml::from_str(
            r#"
enabled = true
transport = "esp32c6_spi"

[esp32c6_spi]
device_path = "/dev/spidev1.0"
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.transport, ConnectivityTransport::Esp32c6Uart);
        assert_eq!(config.esp32c6_uart.device_path, "/dev/spidev1.0");
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
    pub fn core_bind_host() -> String {
        "127.0.0.1".into()
    }
    pub fn llm_model_name() -> String {
        "phi".into()
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
    pub fn stt_language() -> String {
        "auto".into()
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
        PathBuf::from("/opt/geniepod/models/phi-4-mini-instruct-q4_k_m.gguf")
    }
    pub fn wakeword_script() -> PathBuf {
        PathBuf::from("/opt/geniepod/bin/genie-wake-listen.py")
    }
    pub fn speaker_identity_confidence() -> String {
        "high".into()
    }
    pub fn speaker_identity_profile_dir() -> PathBuf {
        PathBuf::from("/opt/geniepod/data/speakers")
    }
    pub fn speaker_identity_min_score() -> f32 {
        0.82
    }
    pub fn tool_policy_enabled() -> bool {
        true
    }
    pub fn actuation_safety_enabled() -> bool {
        true
    }
    pub fn actuation_min_target_confidence() -> f32 {
        0.78
    }
    pub fn actuation_min_sensitive_confidence() -> f32 {
        0.90
    }
    pub fn actuation_deny_multi_target_sensitive() -> bool {
        true
    }
    pub fn actuation_require_available_state() -> bool {
        true
    }
    pub fn actuation_allowed_origins() -> Vec<String> {
        [
            "voice",
            "dashboard",
            "api",
            "telegram",
            "repl",
            "confirmation",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }
    pub fn actuation_max_actions_per_minute() -> usize {
        12
    }
    pub fn telegram_api_base() -> String {
        "https://api.telegram.org".into()
    }
    pub fn telegram_poll_timeout_secs() -> u64 {
        30
    }
    pub fn web_search_enabled() -> bool {
        true
    }
    pub fn web_search_timeout_secs() -> u64 {
        8
    }
    pub fn web_search_max_results() -> usize {
        3
    }
    pub fn web_search_cache_enabled() -> bool {
        true
    }
    pub fn web_search_cache_ttl_secs() -> u64 {
        900
    }
    pub fn web_search_cache_max_entries() -> usize {
        64
    }
    pub fn connectivity_device() -> String {
        "esp32c6".into()
    }
    pub fn esp32c6_uart_device() -> String {
        "/dev/ttyTHS1".into()
    }
    pub fn esp32c6_uart_baud_rate() -> u32 {
        115_200
    }
    pub fn esp32c6_uart_hardware_flow_control() -> bool {
        false
    }
    pub fn esp32c6_uart_mtu_bytes() -> usize {
        1024
    }
    pub fn esp32c6_uart_response_timeout_ms() -> u64 {
        250
    }
}
