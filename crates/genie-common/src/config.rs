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
        assert_eq!(config.timeout_secs, 2);
        assert_eq!(config.max_results, 5);
        assert!(!config.cache_enabled);
        assert_eq!(config.cache_ttl_secs, 60);
        assert_eq!(config.cache_max_entries, 12);
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
