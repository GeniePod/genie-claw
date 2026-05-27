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
    pub agent: AgentConfig,

    #[serde(default)]
    pub optional_ai_provider: OptionalAiProviderConfig,

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

    #[serde(default)]
    pub http: HttpServerConfig,
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

    /// ALSA capture device for the microphone (e.g. "plughw:APE,0" on Jetson
    /// with a LyraT I2S frontend, or "plughw:N,0" for a USB mic). "auto"
    /// runs the helper script which prefers Tegra APE then USB then card 0.
    #[serde(default = "defaults::audio_device")]
    pub audio_device: String,

    /// ALSA playback device for TTS output. Often different from `audio_device`
    /// when the mic is on one card (e.g. LyraT/I2S) and the speaker on another
    /// (e.g. USB headphone, HDMI, 3.5 mm jack). Use "default" for the system
    /// default sink, "plughw:N,0" for a specific card, or "auto" to run the
    /// helper script with a USB-output preference.
    #[serde(default = "defaults::audio_output_device")]
    pub audio_output_device: String,

    /// Audio capture sample rate (Hz). USB headphones typically need 48000.
    #[serde(default = "defaults::audio_sample_rate")]
    pub audio_sample_rate: u32,

    /// Capture denoiser. Options:
    ///   "deepfilternet" — DeepFilterNet (neural, handles non-stationary noise)
    ///   "sox"           — sox `noisered` spectral subtraction (alpha.6 baseline)
    ///   "none"          — no denoise; only bandpass + peak-normalize
    /// Falls back to "sox" then "none" at runtime if the configured backend's
    /// binary or noise profile is missing. See issue #12 for evaluation criteria.
    #[serde(default = "defaults::audio_denoiser")]
    pub audio_denoiser: String,

    /// Path to the DeepFilterNet binary (released as `deep-filter-<ver>-aarch64-unknown-linux-gnu`).
    /// Auto-downloaded by setup-jetson.sh into this location.
    #[serde(default = "defaults::deep_filter_path")]
    pub deep_filter_path: PathBuf,

    /// Attenuation limit in dB for DeepFilterNet (`--atten-lim`). 100.0 = full
    /// denoising (default upstream); lower values mix some of the noisy signal
    /// back in. Drop to ~30 if DFN over-suppresses quiet phonemes.
    #[serde(default = "defaults::deep_filter_atten_lim_db")]
    pub deep_filter_atten_lim_db: f32,

    /// Half-duplex gate (issue #15): milliseconds to wait after Piper's `aplay`
    /// subprocess exits before allowing the next mic capture. Lets the ALSA
    /// hardware buffer drain and the speaker/room reverb decay below the
    /// whisper-server no-speech threshold. Without this, the next cycle's
    /// recording contains the assistant's own TTS bleed and whisper
    /// transcribes the assistant's voice instead of the user's. Set to 0
    /// on installs with full physical isolation (headphones / headset).
    #[serde(default = "defaults::post_tts_silence_ms")]
    pub post_tts_silence_ms: u64,

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

    /// LLM model path used when runtime tooling swaps the configured LLM service.
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

    /// Authentication for assuming a privileged request origin over HTTP.
    #[serde(default)]
    pub origin_auth: OriginAuthConfig,
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
            audio_output_device: defaults::audio_output_device(),
            audio_sample_rate: defaults::audio_sample_rate(),
            audio_denoiser: defaults::audio_denoiser(),
            deep_filter_path: defaults::deep_filter_path(),
            deep_filter_atten_lim_db: defaults::deep_filter_atten_lim_db(),
            post_tts_silence_ms: defaults::post_tts_silence_ms(),
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
            origin_auth: OriginAuthConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    /// Primary deployment profile. Jetson stays the flagship default, but the
    /// agent contract must also run on portable dev hosts and SBCs.
    #[serde(default)]
    pub runtime_profile: AgentRuntimeProfile,

    /// Non-negotiable context budget for the Jetson baseline. Stronger models
    /// may adapt upward later, but production paths must pass this budget first.
    #[serde(default = "defaults::agent_context_window_tokens")]
    pub context_window_tokens: u32,

    /// AI runtime boundary owned below GenieClaw.
    #[serde(default = "defaults::agent_ai_boundary")]
    pub ai_runtime_boundary: RuntimeBoundaryMode,

    /// Voice runtime boundary. The in-repo path is transitional only.
    #[serde(default = "defaults::agent_voice_boundary")]
    pub voice_runtime_boundary: RuntimeBoundaryMode,

    /// Home runtime boundary. Home Assistant is a transitional provider today.
    #[serde(default = "defaults::agent_home_boundary")]
    pub home_runtime_boundary: RuntimeBoundaryMode,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            runtime_profile: AgentRuntimeProfile::Jetson,
            context_window_tokens: defaults::agent_context_window_tokens(),
            ai_runtime_boundary: defaults::agent_ai_boundary(),
            voice_runtime_boundary: defaults::agent_voice_boundary(),
            home_runtime_boundary: defaults::agent_home_boundary(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeProfile {
    #[default]
    Jetson,
    RaspberryPi,
    PortableSbc,
    Laptop,
    Mac,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBoundaryMode {
    /// Stable external runtime contract. This is the target shape.
    #[default]
    ExternalRuntime,
    /// In-repo adapter kept only until the external runtime takes ownership.
    TransitionalAdapter,
    /// Disabled on this profile.
    Disabled,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OptionalAiProviderConfig {
    /// API-key provider support is opt-in and must not change the default
    /// Jetson local binary path.
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub provider: OptionalAiProviderKind,

    /// Base URL or endpoint identifier for provider clients. Empty while
    /// disabled. Remote endpoints require allow_remote_base_url = true.
    #[serde(default)]
    pub base_url: String,

    /// Environment variable that contains the API key. The key value itself is
    /// never stored in config and is not included in support summaries.
    #[serde(default = "defaults::optional_ai_provider_api_key_env")]
    pub api_key_env: String,

    /// Provider path must pass the same limited-context harness before it is
    /// promoted. Keep this at or below [agent].context_window_tokens.
    #[serde(default = "defaults::agent_context_window_tokens")]
    pub context_window_tokens: u32,

    /// Explicit opt-in for non-local endpoints.
    #[serde(default)]
    pub allow_remote_base_url: bool,
}

impl OptionalAiProviderConfig {
    pub fn limited_context_compatible(&self, agent: &AgentConfig) -> bool {
        !self.enabled || self.context_window_tokens <= agent.context_window_tokens
    }

    pub fn production_candidate(&self, agent: &AgentConfig) -> bool {
        self.enabled
            && self.limited_context_compatible(agent)
            && !self.api_key_env.trim().is_empty()
            && (!is_remote_url(&self.base_url) || self.allow_remote_base_url)
    }
}

impl Default for OptionalAiProviderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            base_url: String::new(),
            api_key_env: defaults::optional_ai_provider_api_key_env(),
            context_window_tokens: defaults::agent_context_window_tokens(),
            allow_remote_base_url: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OptionalAiProviderKind {
    /// Generic OpenAI-compatible API surface. Concrete providers can map into
    /// this without adding a heavyweight SDK dependency.
    #[default]
    #[serde(alias = "openai_compatible")]
    OpenAiCompatible,
    #[serde(alias = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
    Custom,
}

fn is_remote_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    let stripped = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let authority = stripped.split('/').next().unwrap_or(stripped);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.find(']')
            .map(|idx| &authority[..=idx + 1])
            .unwrap_or(authority)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    !matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

#[derive(Debug, Deserialize, Clone)]
pub struct SkillPolicyConfig {
    /// Reject skills without a valid sidecar manifest.
    #[serde(default)]
    pub require_manifest: bool,

    /// Reject skills whose `.so` bytes are not verified by a detached Ed25519
    /// signature against a trusted key in `signature_key_dir`. The signature
    /// is cryptographically checked before the library is loaded; a non-empty
    /// `signature` field alone does not satisfy this.
    #[serde(default)]
    pub require_signature: bool,

    /// Directory of trusted Ed25519 public keys (`<key_id>.pub`, base64) used
    /// to verify skill signatures. Lives outside the skills directory so a
    /// party who can drop a `.so` cannot also add a trusting key.
    #[serde(default = "defaults::skill_signature_key_dir")]
    pub signature_key_dir: PathBuf,

    /// Reject skills requesting any of these permission labels.
    #[serde(default)]
    pub denied_permissions: Vec<String>,
}

impl Default for SkillPolicyConfig {
    fn default() -> Self {
        Self {
            require_manifest: false,
            require_signature: false,
            signature_key_dir: defaults::skill_signature_key_dir(),
            denied_permissions: Vec::new(),
        }
    }
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

/// Authentication for assuming a privileged request origin over HTTP (issue
/// #232).
///
/// The request origin (`voice`, `dashboard`, `telegram`, …) drives per-origin
/// tool ACLs, actuation ACLs, rate limits, audit attribution, and NLU
/// confidence thresholds. That origin arrives as the client-supplied
/// `X-Genie-Origin` header, so on its own it is a *forgeable* security
/// principal: any client that can reach the port could claim `voice` to clear
/// a higher-trust bar, or rotate origins to dodge a per-origin rate limit.
///
/// `genie-core` therefore only honors an origin more privileged than the
/// untrusted `api` baseline when the request proves entitlement to it. By
/// default that proof is the transport itself — a loopback peer is the
/// documented single-host trust boundary (see `doc/household-security.md`) —
/// which keeps the local dashboard, CLI, and in-process adapters working with
/// no configuration. A non-loopback peer (i.e. `bind_host = "0.0.0.0"` reached
/// across the LAN) cannot assume a privileged origin from the header alone; it
/// must present a matching shared-secret token.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct OriginAuthConfig {
    /// Require a valid token for every privileged origin, even from loopback
    /// peers. Off by default so a single trusted host needs no setup; turn it
    /// on to also stop one local process from impersonating another's channel.
    #[serde(default)]
    pub require_token: bool,

    /// Map of origin name (`voice`, `dashboard`, `telegram`, `repl`) to the
    /// shared secret a request must present in the `X-Genie-Origin-Token`
    /// header to assume that origin. Prefer leaving the value empty here and
    /// supplying it via the `GENIE_ORIGIN_TOKEN_<ORIGIN>` environment variable
    /// (keep config files `0600`). An origin with no resolved token cannot be
    /// claimed by a non-loopback peer at all.
    #[serde(default)]
    pub tokens: HashMap<String, String>,
}

impl OriginAuthConfig {
    /// Resolve the effective `origin -> secret` map: the configured value
    /// first, then the `GENIE_ORIGIN_TOKEN_<ORIGIN>` environment variable.
    /// Blank/whitespace tokens are dropped so an empty entry can never
    /// authenticate anything.
    pub fn resolved_tokens(&self) -> HashMap<String, String> {
        const KNOWN_ORIGINS: [&str; 4] = ["voice", "dashboard", "telegram", "repl"];

        let mut out: HashMap<String, String> = HashMap::new();
        // Configured origins first, then fill any remaining known origins from
        // the environment so `GENIE_ORIGIN_TOKEN_TELEGRAM` alone is enough.
        let names = self.tokens.keys().map(String::as_str).chain(KNOWN_ORIGINS);
        for name in names {
            let key = name.trim().to_ascii_lowercase();
            if key.is_empty() || out.contains_key(&key) {
                continue;
            }
            let configured = self.tokens.get(name).map(|s| s.trim().to_string());
            let token = match configured {
                Some(value) if !value.is_empty() => value,
                _ => std::env::var(format!("GENIE_ORIGIN_TOKEN_{}", key.to_ascii_uppercase()))
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            };
            if !token.is_empty() {
                out.insert(key, token);
            }
        }
        out
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

    /// genie-api HTTP service. Falls back to the documented default
    /// (`http://127.0.0.1:3080/api/status`) when absent so existing
    /// deployments keep working after this field was added.
    #[serde(default = "defaults::api_service")]
    pub api: ServiceEndpoint,

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

    /// Voice-message handling for the Telegram channel (issue #42).
    #[serde(default)]
    pub voice: TelegramVoiceConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramVoiceConfig {
    /// Enable voice-message ingestion. When false, voice messages get a polite
    /// text reply explaining that voice is not enabled on this deployment.
    #[serde(default)]
    pub enabled: bool,

    /// Hard cap on accepted voice duration. Telegram includes a `duration`
    /// field; anything longer is rejected before download.
    #[serde(default = "defaults::telegram_voice_max_duration_secs")]
    pub max_voice_duration_secs: u32,

    /// Delete the downloaded `.ogg` and transcoded `.wav` after handling.
    #[serde(default = "defaults::telegram_voice_delete_temp_audio")]
    pub delete_temp_audio: bool,

    /// Path to the `ffmpeg` binary used to transcode Telegram OGG/Opus to the
    /// 16 kHz mono WAV that Whisper consumes.
    #[serde(default = "defaults::telegram_voice_ffmpeg_path")]
    pub ffmpeg_path: PathBuf,

    /// Reply to incoming voice messages with a synthesized voice message
    /// instead of (or in addition to) text. Phase 2 of issue #42: Piper
    /// synthesizes WAV, ffmpeg encodes it as OGG/Opus, the bot uploads via
    /// the Telegram `sendVoice` endpoint. Falls back to text on any failure
    /// (Piper missing, ffmpeg missing, sendVoice error, etc.) so no reply is
    /// ever silently dropped.
    #[serde(default)]
    pub reply_as_voice: bool,

    /// Hard cap on the assistant text fed to Piper. Long-form responses
    /// produce long voice messages that hit Telegram's 1 MB sendVoice limit;
    /// when the text is over this length the bot falls back to text reply.
    /// Tuned for the 60–90 s of OGG/Opus that comfortably fits under 1 MB.
    #[serde(default = "defaults::telegram_voice_max_reply_chars")]
    pub max_reply_chars: usize,

    /// Bound on concurrent voice pipelines (download → ffmpeg → whisper-server
    /// → /api/chat) the adapter will run at once. Issue #77: the poll loop
    /// now spawns each update, but unbounded fan-out under burst could
    /// overload ffmpeg / whisper-server. Text-only updates are not gated by
    /// this knob.
    #[serde(default = "defaults::telegram_voice_max_parallel_voice")]
    pub max_parallel_voice: usize,
}

impl Default for TelegramVoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_voice_duration_secs: defaults::telegram_voice_max_duration_secs(),
            delete_temp_audio: defaults::telegram_voice_delete_temp_audio(),
            ffmpeg_path: defaults::telegram_voice_ffmpeg_path(),
            reply_as_voice: false,
            max_reply_chars: defaults::telegram_voice_max_reply_chars(),
            max_parallel_voice: defaults::telegram_voice_max_parallel_voice(),
        }
    }
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

/// Inbound HTTP-server hardening shared by `genie-core` (`:3000`) and
/// `genie-api` (`:3080`).
///
/// These bounds protect the always-on daemon from an unauthenticated peer on
/// the LAN: oversized request lines/headers are rejected, a stalled connection
/// is dropped after `read_timeout_secs`, and the number of concurrent
/// connections is capped (issue #195). The per-request body cap is fixed per
/// server (64 KiB for genie-core, 4 KiB for genie-api) and is not part of this
/// section.
///
/// It also carries the cross-origin request gate (issue #228): both servers
/// reflect only allowlisted `Origin`s (never the old wildcard), reject
/// non-allowlisted `Host`s (DNS-rebinding), and — when `local_api_token` is
/// set — require that token on mutating/actuating endpoints.
#[derive(Debug, Deserialize, Clone)]
pub struct HttpServerConfig {
    /// Max bytes in the request line, newline included.
    #[serde(default = "defaults::http_max_request_line_bytes")]
    pub max_request_line_bytes: usize,

    /// Max bytes in any single header line, newline included.
    #[serde(default = "defaults::http_max_header_line_bytes")]
    pub max_header_line_bytes: usize,

    /// Max number of header lines per request.
    #[serde(default = "defaults::http_max_header_count")]
    pub max_header_count: usize,

    /// Max total bytes across all header lines (the header-phase ceiling).
    /// Mirrors the existing body cap upward into the header phase.
    #[serde(default = "defaults::http_max_header_bytes")]
    pub max_header_bytes: usize,

    /// Deadline for reading one whole request (line + headers + body).
    #[serde(default = "defaults::http_read_timeout_secs")]
    pub read_timeout_secs: u64,

    /// Ceiling on concurrently handled connections.
    #[serde(default = "defaults::http_max_connections")]
    pub max_connections: usize,

    /// Extra browser `Origin`s to allow cross-origin (exact, scheme-qualified,
    /// e.g. `http://genie.local:3000`). Loopback origins for the bound port are
    /// always allowed; add LAN hostnames or alternate UI origins here.
    #[serde(default)]
    pub allowed_origins: Vec<String>,

    /// Extra `Host` header values to allow (exact `host` or `host:port`, e.g.
    /// `genie.local:3000`). Loopback hosts for the bound port are always
    /// allowed; add the LAN hostname/IP the daemon is reached by here. Required
    /// for any non-loopback access — it closes the DNS-rebinding hole.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,

    /// Shared local API token for mutating/actuating endpoints (chat, memory
    /// edits, actuation confirm). When set, both servers require it via
    /// `X-Genie-Token` or `Authorization: Bearer …`; the on-device UIs receive
    /// it automatically and genie-api forwards it to genie-core. Blank disables
    /// token enforcement (the Origin/Host gate still applies). Can also be set
    /// via the `GENIEPOD_LOCAL_API_TOKEN` env var.
    #[serde(default)]
    pub local_api_token: String,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            max_request_line_bytes: defaults::http_max_request_line_bytes(),
            max_header_line_bytes: defaults::http_max_header_line_bytes(),
            max_header_count: defaults::http_max_header_count(),
            max_header_bytes: defaults::http_max_header_bytes(),
            read_timeout_secs: defaults::http_read_timeout_secs(),
            max_connections: defaults::http_max_connections(),
            allowed_origins: Vec::new(),
            allowed_hosts: Vec::new(),
            local_api_token: String::new(),
        }
    }
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
    /// LLM backend selector. Only meaningful for `services.llm`.
    #[serde(default)]
    pub backend: LlmBackendKind,
}

/// Result of resolving a configured service URL for the simple TCP probe
/// path used by `genie-ctl status` / `diag` / `support-bundle`.
///
/// `Http` and `Https` targets are probed with the shared client in
/// [`crate::probe`]. Unknown schemes are returned as
/// [`ServiceProbeTarget::UnsupportedScheme`] so callers can label the row
/// instead of mis-reporting a healthy service as DOWN by sending plaintext
/// to a TLS port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceProbeTarget {
    /// Plain-HTTP probe target.
    Http {
        /// `host:port`, with IPv6 hosts kept bracketed (e.g. `[::1]:80`)
        /// so the string round-trips through `to_socket_addrs`.
        addr: String,
        /// Request path, always starting with `/`.
        path: String,
    },
    /// TLS HTTP probe target (`https://` URLs).
    Https { addr: String, path: String },
    /// Scheme the probe client cannot service (neither plain nor TLS).
    UnsupportedScheme {
        /// The scheme as found in the URL (lowercased), e.g. `"wss"`.
        scheme: String,
    },
}

/// Parse a configured service URL into a probe target for `genie-ctl`'s
/// HTTP/TLS probe client.
///
/// Behavior:
/// - Bare URLs without a scheme are treated as `http://…`.
/// - `http://` URLs produce [`ServiceProbeTarget::Http`].
/// - `https://` URLs produce [`ServiceProbeTarget::Https`].
/// - Other recognized schemes produce [`ServiceProbeTarget::UnsupportedScheme`].
/// - Missing port defaults to 80 for `http` and 443 for `https`.
/// - Missing path defaults to `/`.
/// - IPv6 hosts must be bracketed (`[::1]`, `[::1]:8123`); brackets are
///   preserved in the returned `addr` so the string parses with
///   `std::net::ToSocketAddrs`.
pub fn parse_service_probe_target(url: &str) -> ServiceProbeTarget {
    // Scheme split. A leading `scheme://` is recognized when the scheme is
    // ASCII letters followed by `://`. Anything else falls through as a
    // bare `http` authority — keeps existing config files that wrote
    // `127.0.0.1:3080/api/status` working.
    let (scheme, rest) = match split_scheme(url) {
        Some((scheme, rest)) => (scheme, rest),
        None => ("http", url),
    };

    let (authority, path) = split_authority_and_path(rest);
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };

    match scheme {
        "http" => ServiceProbeTarget::Http {
            addr: ensure_port(authority, 80),
            path,
        },
        "https" => ServiceProbeTarget::Https {
            addr: ensure_port(authority, 443),
            path,
        },
        _ => ServiceProbeTarget::UnsupportedScheme {
            scheme: scheme.to_string(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceUrlDrift {
    service: &'static str,
    configured_url: String,
    services_url_addr: String,
    listen_addr: String,
}

fn record_http_url_drift(
    drifts: &mut Vec<ServiceUrlDrift>,
    service: &'static str,
    configured_url: &str,
    listen_addr: &str,
) {
    match parse_service_probe_target(configured_url) {
        ServiceProbeTarget::Http { addr, .. } if addr != listen_addr => {
            drifts.push(ServiceUrlDrift {
                service,
                configured_url: configured_url.to_string(),
                services_url_addr: addr,
                listen_addr: listen_addr.to_string(),
            });
        }
        ServiceProbeTarget::Http { .. }
        | ServiceProbeTarget::Https { .. }
        | ServiceProbeTarget::UnsupportedScheme { .. } => {}
    }
}

/// Split a URL into `(lowercased_scheme, rest_after_://)` when it starts
/// with a `scheme://` prefix; otherwise `None`.
fn split_scheme(url: &str) -> Option<(&'static str, &str)> {
    // Only recognize the two schemes this codebase actually uses; anything
    // else falls through and is reported as unsupported via the caller's
    // exhaustive match. Keeping this small avoids pretending we understand
    // arbitrary URLs.
    for scheme in ["http", "https"] {
        let prefix = match scheme {
            "http" => "http://",
            "https" => "https://",
            _ => unreachable!(),
        };
        if let Some(rest) = url.strip_prefix(prefix) {
            return Some((scheme, rest));
        }
    }
    None
}

/// True if `dir` holds at least one `*.pub` trusted-key file. Used only for
/// the security-posture report — the loader does the real key parsing — so a
/// cheap presence check is enough to tell an operator whether
/// `require_signature` is actually usable as configured.
fn has_trusted_skill_keys(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("pub"))
        })
        .unwrap_or(false)
}

/// Split `authority[path]` into (authority, path). IPv6 brackets are
/// respected: the first `/` *after* a closing `]` is the path delimiter,
/// not any earlier slash that might appear inside `[…]` (it can't today,
/// but the rule is the simplest correct one).
fn split_authority_and_path(rest: &str) -> (&str, &str) {
    // For `[…]…` find the closing bracket first and split on the first
    // `/` that follows it. Otherwise split on the first `/`.
    let scan_from = if rest.starts_with('[') {
        rest.find(']').map(|i| i + 1).unwrap_or(rest.len())
    } else {
        0
    };

    match rest[scan_from..].find('/') {
        Some(idx) => rest.split_at(scan_from + idx),
        None => (rest, "/"),
    }
}

/// Append `:default_port` to `authority` unless it already carries an
/// explicit port. Bracket-aware so a bare `[::1]` correctly gets the
/// default added (a naive `contains(':')` check would treat the colons
/// inside the brackets as a port separator).
fn ensure_port(authority: &str, default_port: u16) -> String {
    let has_explicit_port = if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6. A port, if present, follows the closing `]`.
        rest.find(']')
            .map(|i| rest[i + 1..].starts_with(':'))
            .unwrap_or(false)
    } else {
        // Hostname or IPv4 — a single colon means `host:port`.
        authority.contains(':')
    };

    if has_explicit_port {
        authority.to_string()
    } else {
        format!("{authority}:{default_port}")
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackendKind {
    #[default]
    #[serde(alias = "genie-ai-runtime")]
    GenieAiRuntime,
    #[serde(alias = "llama-cpp")]
    LlamaCpp,
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
        let mut config: Config = toml::from_str(&contents)?;
        config.resolve_env_overrides();
        config.validate_service_url_drift();
        Ok(config)
    }

    /// Fold environment-provided secrets into the parsed config so every
    /// consumer reads them off the struct. Config file wins when set; otherwise
    /// the env var is used. Currently only the shared local API token
    /// (`GENIEPOD_LOCAL_API_TOKEN`, issue #228).
    fn resolve_env_overrides(&mut self) {
        if self.http.local_api_token.trim().is_empty()
            && let Ok(token) = std::env::var("GENIEPOD_LOCAL_API_TOKEN")
            && !token.trim().is_empty()
        {
            self.http.local_api_token = token.trim().to_string();
        }
    }

    /// TCP `host:port` for local HTTP clients proxying to genie-core.
    ///
    /// Uses `[core].bind_host` and `[core].port`. Maps `0.0.0.0` / `::` to
    /// `127.0.0.1` because local callers should use loopback even when core
    /// listens on all interfaces.
    pub fn core_http_addr(&self) -> String {
        let host = self.core.bind_host.trim();
        let host = if host.is_empty() || host == "0.0.0.0" || host == "::" {
            "127.0.0.1"
        } else {
            host
        };
        format!("{host}:{}", self.core.port)
    }

    /// Health-probe URL for genie-core, derived from `[core].bind_host` and
    /// `[core].port` instead of `[services.core].url`.
    ///
    /// Local probes should follow where core actually listens. Reading
    /// `[services.core].url` can drift when an operator changes `[core].port`
    /// but leaves the service URL at its default, producing false DOWN signals.
    pub fn core_health_url(&self) -> String {
        format!("http://{}/api/health", self.core_http_addr())
    }

    /// TCP `host:port` for `genie-api` to bind, derived from `[services.api].url`.
    ///
    /// Keeps the listen socket aligned with health probes and `genie-ctl` that
    /// already read the same configured URL (issue #140).
    pub fn api_http_addr(&self) -> anyhow::Result<String> {
        match parse_service_probe_target(&self.services.api.url) {
            ServiceProbeTarget::Http { addr, .. } => Ok(addr),
            ServiceProbeTarget::Https { .. } => anyhow::bail!(
                "genie-api cannot bind from [services.api].url: unsupported scheme \
                 \"https\" (use http:// for the local bind address)"
            ),
            ServiceProbeTarget::UnsupportedScheme { scheme } => anyhow::bail!(
                "genie-api cannot bind from [services.api].url: unsupported scheme \
                 \"{scheme}\" (use http://)"
            ),
        }
    }

    /// Status-probe URL for genie-api, derived from `[services.api].url` host:port.
    ///
    /// Normalizes bare authorities (e.g. `127.0.0.1:4080/api/status`) to a full
    /// `http://…/api/status` URL for dashboard latency probes.
    pub fn api_status_url(&self) -> anyhow::Result<String> {
        Ok(format!("http://{}/api/status", self.api_http_addr()?))
    }

    /// Compare configured service URLs against derived listen addresses and log
    /// warnings when they disagree. Does not fail startup.
    pub fn validate_service_url_drift(&self) {
        for drift in self.service_url_drifts() {
            tracing::warn!(
                service = drift.service,
                configured_url = %drift.configured_url,
                services_url_addr = %drift.services_url_addr,
                listen_addr = %drift.listen_addr,
                "service URL host:port disagrees with listen address"
            );
        }
    }

    fn service_url_drifts(&self) -> Vec<ServiceUrlDrift> {
        let mut drifts = Vec::new();

        record_http_url_drift(
            &mut drifts,
            "core",
            &self.services.core.url,
            &self.core_http_addr(),
        );

        if let Ok(listen_addr) = self.api_http_addr() {
            record_http_url_drift(&mut drifts, "api", &self.services.api.url, &listen_addr);
        }

        drifts
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
            "core" | "genie-core" | "llm" | "genie-llm" | "api" | "genie-api" => true,
            "homeassistant" => self.services.homeassistant.is_some(),
            "nextcloud" => self.services.nextcloud.is_some(),
            "jellyfin" => self.services.jellyfin.is_some(),
            _ => true,
        }
    }

    /// Resolve a service alias used by runtime tooling to its configured
    /// systemd unit. Optional services return `None` when they are not
    /// configured for this deployment.
    pub fn service_unit_for_alias(&self, alias: &str) -> Option<String> {
        match alias {
            "core" | "genie-core" => Some(self.services.core.systemd_unit.clone()),
            "llm" | "genie-llm" => Some(self.services.llm.systemd_unit.clone()),
            "api" | "genie-api" => Some(self.services.api.systemd_unit.clone()),
            "homeassistant" => self
                .services
                .homeassistant
                .as_ref()
                .map(|service| service.systemd_unit.clone()),
            "nextcloud" => self
                .services
                .nextcloud
                .as_ref()
                .map(|service| service.systemd_unit.clone()),
            "jellyfin" => self
                .services
                .jellyfin
                .as_ref()
                .map(|service| service.systemd_unit.clone()),
            _ if self.manages_service_alias(alias) => Some(alias.to_string()),
            _ => None,
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
        if self.optional_ai_provider.enabled {
            risk_flags.push("optional_ai_provider_enabled");
        }
        if self.optional_ai_provider.enabled
            && !self
                .optional_ai_provider
                .limited_context_compatible(&self.agent)
        {
            risk_flags.push("optional_ai_provider_context_exceeds_agent_budget");
        }
        if self.optional_ai_provider.enabled
            && is_remote_url(&self.optional_ai_provider.base_url)
            && !self.optional_ai_provider.allow_remote_base_url
        {
            risk_flags.push("optional_ai_provider_remote_url_blocked");
        }
        if !self.core.skill_policy.require_manifest {
            risk_flags.push("skill_manifest_not_required");
        }
        if !self.core.skill_policy.require_signature {
            risk_flags.push("skill_signature_not_required");
        } else if !has_trusted_skill_keys(&self.core.skill_policy.signature_key_dir) {
            // require_signature is on but no trusted keys are installed: the
            // loader fails closed (no skill can load), so flag the misconfig
            // rather than silently rejecting every skill.
            risk_flags.push("skill_signature_required_but_no_trusted_keys");
        }

        serde_json::json!({
            "trust_model": "single_household_operator_boundary",
            "raw_config_exposed": false,
            "raw_config_policy": "local_operator_file_only",
            "agent": {
                "runtime_profile": format!("{:?}", self.agent.runtime_profile),
                "context_window_tokens": self.agent.context_window_tokens,
                "ai_runtime_boundary": format!("{:?}", self.agent.ai_runtime_boundary),
                "voice_runtime_boundary": format!("{:?}", self.agent.voice_runtime_boundary),
                "home_runtime_boundary": format!("{:?}", self.agent.home_runtime_boundary),
                "agent_layer_only": true
            },
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
                "connectivity_coprocessor_enabled": self.connectivity_enabled(),
                "optional_ai_provider_enabled": self.optional_ai_provider.enabled
            },
            "optional_ai_provider": {
                "enabled": self.optional_ai_provider.enabled,
                "provider": format!("{:?}", self.optional_ai_provider.provider),
                "context_window_tokens": self.optional_ai_provider.context_window_tokens,
                "limited_context_compatible": self.optional_ai_provider.limited_context_compatible(&self.agent),
                "allow_remote_base_url": self.optional_ai_provider.allow_remote_base_url,
                "api_key_env_configured": !self.optional_ai_provider.api_key_env.trim().is_empty(),
                "base_url_configured": !self.optional_ai_provider.base_url.trim().is_empty(),
                "api_key_value_exposed": false
            },
            "policy": {
                "tool_policy_enabled": self.core.tool_policy.enabled,
                "actuation_safety_enabled": self.core.actuation_safety.enabled,
                "sensitive_multi_target_denied": self.core.actuation_safety.deny_multi_target_sensitive,
                "available_state_required": self.core.actuation_safety.require_available_state,
                "skill_manifest_required": self.core.skill_policy.require_manifest,
                "skill_signature_required": self.core.skill_policy.require_signature,
                "skill_signature_scheme": "ed25519_detached_over_so_bytes",
                "skill_signature_key_dir": self.core.skill_policy.signature_key_dir.display().to_string(),
                "skill_signature_trusted_keys_present": has_trusted_skill_keys(&self.core.skill_policy.signature_key_dir)
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
                backend: LlmBackendKind::default(),
            },
            llm: ServiceEndpoint {
                url: "http://127.0.0.1:8080/health".into(),
                systemd_unit: "genie-ai-runtime.service".into(),
                backend: LlmBackendKind::GenieAiRuntime,
            },
            api: defaults::api_service(),
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
            voice: TelegramVoiceConfig::default(),
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
            agent: AgentConfig::default(),
            optional_ai_provider: OptionalAiProviderConfig::default(),
            governor: GovernorConfig::default(),
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
            telegram: TelegramConfig::default(),
            web_search: WebSearchConfig::default(),
            connectivity: ConnectivityConfig::default(),
            http: HttpServerConfig::default(),
        }
    }

    #[test]
    fn homeassistant_is_optional_by_default() {
        let config = test_config();
        assert!(config.homeassistant_service().is_none());
        assert!(!config.manages_service_alias("homeassistant"));
    }

    #[test]
    fn agent_profile_defaults_to_jetson_limited_context() {
        let config = test_config();
        assert_eq!(config.agent.runtime_profile, AgentRuntimeProfile::Jetson);
        assert_eq!(config.agent.context_window_tokens, 4096);
        assert_eq!(
            config.agent.ai_runtime_boundary,
            RuntimeBoundaryMode::ExternalRuntime
        );
        assert_eq!(
            config.agent.voice_runtime_boundary,
            RuntimeBoundaryMode::TransitionalAdapter
        );
        assert_eq!(
            config.agent.home_runtime_boundary,
            RuntimeBoundaryMode::TransitionalAdapter
        );
    }

    #[test]
    fn portable_agent_profile_parses() {
        let config: AgentConfig = toml::from_str(
            r#"
runtime_profile = "portable_sbc"
context_window_tokens = 4096
ai_runtime_boundary = "external_runtime"
voice_runtime_boundary = "disabled"
home_runtime_boundary = "external_runtime"
"#,
        )
        .unwrap();

        assert_eq!(config.runtime_profile, AgentRuntimeProfile::PortableSbc);
        assert_eq!(config.context_window_tokens, 4096);
        assert_eq!(config.voice_runtime_boundary, RuntimeBoundaryMode::Disabled);
    }

    #[test]
    fn optional_ai_provider_is_disabled_and_limited_context_by_default() {
        let config = test_config();
        assert!(!config.optional_ai_provider.enabled);
        assert_eq!(
            config.optional_ai_provider.provider,
            OptionalAiProviderKind::OpenAiCompatible
        );
        assert!(
            config
                .optional_ai_provider
                .limited_context_compatible(&config.agent)
        );
        assert!(
            !config
                .optional_ai_provider
                .production_candidate(&config.agent)
        );
    }

    #[test]
    fn optional_ai_provider_must_fit_limited_context() {
        let agent = AgentConfig::default();
        let provider: OptionalAiProviderConfig = toml::from_str(
            r#"
enabled = true
provider = "open_ai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
context_window_tokens = 8192
allow_remote_base_url = true
"#,
        )
        .unwrap();

        assert!(!provider.limited_context_compatible(&agent));
        assert!(!provider.production_candidate(&agent));
    }

    #[test]
    fn optional_ai_provider_remote_requires_explicit_allow() {
        let agent = AgentConfig::default();
        let provider: OptionalAiProviderConfig = toml::from_str(
            r#"
enabled = true
provider = "custom"
base_url = "https://provider.example/v1"
api_key_env = "GENIE_PROVIDER_KEY"
context_window_tokens = 4096
allow_remote_base_url = false
"#,
        )
        .unwrap();

        assert!(provider.limited_context_compatible(&agent));
        assert!(!provider.production_candidate(&agent));
    }

    #[test]
    fn api_service_defaults_to_documented_endpoint() {
        let config = test_config();
        assert_eq!(config.services.api.url, "http://127.0.0.1:3080/api/status");
        assert_eq!(config.services.api.systemd_unit, "genie-api.service");
        assert!(config.manages_service_alias("api"));
        assert!(config.manages_service_alias("genie-api"));
        assert_eq!(
            config.service_unit_for_alias("api").as_deref(),
            Some("genie-api.service")
        );
    }

    #[test]
    fn services_api_can_be_overridden_in_toml() {
        let services: ServicesConfig = toml::from_str(
            r#"
[core]
url = "http://127.0.0.1:3000/api/health"
systemd_unit = "genie-core.service"

[llm]
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-ai-runtime.service"

[api]
url = "http://10.0.0.5:4080/api/status"
systemd_unit = "genie-api.service"
"#,
        )
        .unwrap();

        assert_eq!(services.api.url, "http://10.0.0.5:4080/api/status");
    }

    #[test]
    fn services_api_falls_back_when_toml_omits_section() {
        // Existing deployments may have [services.core] and [services.llm] but
        // no [services.api] yet — they must keep parsing.
        let services: ServicesConfig = toml::from_str(
            r#"
[core]
url = "http://127.0.0.1:3000/api/health"
systemd_unit = "genie-core.service"

[llm]
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-ai-runtime.service"
"#,
        )
        .unwrap();

        assert_eq!(services.api.url, "http://127.0.0.1:3080/api/status");
    }

    fn http_target(url: &str) -> (String, String) {
        match parse_service_probe_target(url) {
            ServiceProbeTarget::Http { addr, path } => (addr, path),
            other => panic!("expected Http target for {url}, got {other:?}"),
        }
    }

    #[test]
    fn parse_service_probe_target_splits_http_url() {
        let (addr, path) = http_target("http://127.0.0.1:3080/api/status");
        assert_eq!(addr, "127.0.0.1:3080");
        assert_eq!(path, "/api/status");
    }

    #[test]
    fn parse_service_probe_target_keeps_trailing_slash() {
        let (addr, path) = http_target("http://192.168.1.50:8123/");
        assert_eq!(addr, "192.168.1.50:8123");
        assert_eq!(path, "/");
    }

    #[test]
    fn parse_service_probe_target_defaults_http_port_when_missing() {
        let (addr, path) = http_target("http://homeassistant.local/api/");
        assert_eq!(addr, "homeassistant.local:80");
        assert_eq!(path, "/api/");
    }

    #[test]
    fn parse_service_probe_target_defaults_path_when_missing() {
        let (addr, path) = http_target("http://127.0.0.1:8123");
        assert_eq!(addr, "127.0.0.1:8123");
        assert_eq!(path, "/");
    }

    #[test]
    fn parse_service_probe_target_treats_bare_url_as_http() {
        // Some legacy configs wrote the host:port without a scheme; keep
        // them working as http targets.
        let (addr, path) = http_target("127.0.0.1:3080/api/status");
        assert_eq!(addr, "127.0.0.1:3080");
        assert_eq!(path, "/api/status");
    }

    #[test]
    fn parse_service_probe_target_parses_https_with_default_port() {
        match parse_service_probe_target("https://ha.example/api/") {
            ServiceProbeTarget::Https { addr, path } => {
                assert_eq!(addr, "ha.example:443");
                assert_eq!(path, "/api/");
            }
            other => panic!("expected Https target, got {other:?}"),
        }
    }

    #[test]
    fn parse_service_probe_target_handles_ipv6_with_explicit_port() {
        let (addr, path) = http_target("http://[::1]:3080/api/status");
        assert_eq!(addr, "[::1]:3080");
        assert_eq!(path, "/api/status");
    }

    #[test]
    fn parse_service_probe_target_adds_default_port_to_bracketed_ipv6() {
        // Regression for PR #127 review: a naive `authority.contains(':')`
        // check sees the colons inside [::1] and skips the default port,
        // producing `[::1]` which TcpStream::connect cannot parse. Make
        // sure we emit `[::1]:80` instead.
        let (addr, path) = http_target("http://[::1]/api/status");
        assert_eq!(addr, "[::1]:80");
        assert_eq!(path, "/api/status");
    }

    #[test]
    fn parse_service_probe_target_handles_bracketed_ipv6_without_path() {
        let (addr, path) = http_target("http://[fe80::1%25eth0]");
        assert_eq!(addr, "[fe80::1%25eth0]:80");
        assert_eq!(path, "/");
    }

    #[test]
    fn core_bind_host_defaults_to_localhost() {
        let config = test_config();
        assert_eq!(config.core.bind_host, "127.0.0.1");
    }

    #[test]
    fn core_http_addr_uses_bind_host_and_port() {
        let mut config = test_config();
        config.core.port = 3001;
        config.core.bind_host = "127.0.0.1".into();
        assert_eq!(config.core_http_addr(), "127.0.0.1:3001");
    }

    #[test]
    fn core_http_addr_maps_listen_all_to_loopback() {
        let mut config = test_config();
        config.core.port = 3000;
        config.core.bind_host = "0.0.0.0".into();
        assert_eq!(config.core_http_addr(), "127.0.0.1:3000");
    }

    #[test]
    fn core_health_url_uses_default_port() {
        let config = test_config();
        assert_eq!(config.core_health_url(), "http://127.0.0.1:3000/api/health");
    }

    #[test]
    fn core_health_url_tracks_custom_core_port() {
        let mut config = test_config();
        config.core.port = 3001;
        assert_eq!(config.core_health_url(), "http://127.0.0.1:3001/api/health");
    }

    #[test]
    fn core_health_url_maps_listen_all_to_loopback() {
        let mut config = test_config();
        config.core.bind_host = "0.0.0.0".into();
        assert_eq!(config.core_health_url(), "http://127.0.0.1:3000/api/health");
    }

    #[test]
    fn core_health_url_honors_custom_bind_host() {
        let mut config = test_config();
        config.core.bind_host = "10.0.0.5".into();
        config.core.port = 4000;
        assert_eq!(config.core_health_url(), "http://10.0.0.5:4000/api/health");
    }

    #[test]
    fn core_health_url_ignores_stale_services_core_url() {
        let mut config = test_config();
        config.core.port = 3001;
        config.services.core.url = "http://127.0.0.1:3000/api/health".into();
        assert_eq!(config.core_health_url(), "http://127.0.0.1:3001/api/health");
    }

    #[test]
    fn validate_service_url_drift_empty_on_aligned_defaults() {
        let config = test_config();
        assert!(config.service_url_drifts().is_empty());
    }

    #[test]
    fn validate_service_url_drift_detects_stale_core_port() {
        let mut config = test_config();
        config.core.port = 3001;
        config.services.core.url = "http://127.0.0.1:3000/api/health".into();

        let drifts = config.service_url_drifts();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].service, "core");
        assert_eq!(drifts[0].services_url_addr, "127.0.0.1:3000");
        assert_eq!(drifts[0].listen_addr, "127.0.0.1:3001");
    }

    #[test]
    fn validate_service_url_drift_detects_core_host_mismatch() {
        let mut config = test_config();
        config.core.bind_host = "10.0.0.5".into();
        config.core.port = 4000;
        config.services.core.url = "http://127.0.0.1:3000/api/health".into();

        let drifts = config.service_url_drifts();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].service, "core");
        assert_eq!(drifts[0].services_url_addr, "127.0.0.1:3000");
        assert_eq!(drifts[0].listen_addr, "10.0.0.5:4000");
    }

    #[test]
    fn validate_service_url_drift_skips_unsupported_api_scheme() {
        let mut config = test_config();
        config.services.api.url = "https://api.example/api/status".into();
        assert!(config.service_url_drifts().is_empty());
    }

    #[test]
    fn validate_service_url_drift_no_panic_on_defaults() {
        test_config().validate_service_url_drift();
    }

    #[test]
    fn api_http_addr_defaults_to_documented_port() {
        let config = test_config();
        assert_eq!(config.api_http_addr().unwrap(), "127.0.0.1:3080");
    }

    #[test]
    fn api_http_addr_follows_services_api_url() {
        let mut config = test_config();
        config.services.api.url = "http://127.0.0.1:4080/api/status".into();
        assert_eq!(config.api_http_addr().unwrap(), "127.0.0.1:4080");
    }

    #[test]
    fn api_http_addr_rejects_https_url() {
        let mut config = test_config();
        config.services.api.url = "https://api.example/api/status".into();
        let err = config.api_http_addr().unwrap_err();
        assert!(
            err.to_string().contains("unsupported scheme"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn api_status_url_defaults_to_documented_endpoint() {
        let config = test_config();
        assert_eq!(
            config.api_status_url().unwrap(),
            "http://127.0.0.1:3080/api/status"
        );
    }

    #[test]
    fn api_status_url_normalizes_bare_authority() {
        let mut config = test_config();
        config.services.api.url = "127.0.0.1:4080/api/status".into();
        assert_eq!(
            config.api_status_url().unwrap(),
            "http://127.0.0.1:4080/api/status"
        );
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
            backend: LlmBackendKind::default(),
        });

        assert!(config.manages_service_alias("genie-core"));
        assert!(config.manages_service_alias("llm"));
        assert!(!config.manages_service_alias("homeassistant"));
        assert!(config.manages_service_alias("nextcloud"));
        assert!(!config.manages_service_alias("jellyfin"));
    }

    #[test]
    fn service_unit_aliases_use_configured_units() {
        let mut config = test_config();
        config.services.llm.systemd_unit = "genie-ai-runtime.service".into();
        config.services.nextcloud = Some(ServiceEndpoint {
            url: "http://127.0.0.1:8180/status.php".into(),
            systemd_unit: "nextcloud.service".into(),
            backend: LlmBackendKind::default(),
        });

        assert_eq!(
            config.service_unit_for_alias("core").as_deref(),
            Some("genie-core.service")
        );
        assert_eq!(
            config.service_unit_for_alias("llm").as_deref(),
            Some("genie-ai-runtime.service")
        );
        assert_eq!(
            config.service_unit_for_alias("genie-llm").as_deref(),
            Some("genie-ai-runtime.service")
        );
        assert_eq!(
            config.service_unit_for_alias("nextcloud").as_deref(),
            Some("nextcloud.service")
        );
        assert_eq!(config.service_unit_for_alias("jellyfin"), None);
    }

    #[test]
    fn llm_service_backend_defaults_to_genie_ai_runtime() {
        let service: ServiceEndpoint = toml::from_str(
            r#"
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-ai-runtime.service"
"#,
        )
        .unwrap();

        assert_eq!(service.backend, LlmBackendKind::GenieAiRuntime);
    }

    #[test]
    fn llm_service_backend_accepts_genie_ai_runtime() {
        let service: ServiceEndpoint = toml::from_str(
            r#"
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-ai-runtime.service"
backend = "genie_ai_runtime"
"#,
        )
        .unwrap();

        assert_eq!(service.backend, LlmBackendKind::GenieAiRuntime);
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
        // Defaults to the distribution key dir when not overridden.
        assert_eq!(
            config.signature_key_dir,
            PathBuf::from("/etc/geniepod/skill-keys")
        );
    }

    #[test]
    fn skill_policy_signature_key_dir_overridable() {
        let config: SkillPolicyConfig = toml::from_str(
            r#"
require_signature = true
signature_key_dir = "/custom/keys"
"#,
        )
        .unwrap();
        assert_eq!(config.signature_key_dir, PathBuf::from("/custom/keys"));
    }

    #[test]
    fn security_posture_flags_require_signature_without_keys() {
        let mut config = test_config();
        config.core.skill_policy.require_signature = true;
        // Point at a directory with no trusted keys → loader fails closed.
        config.core.skill_policy.signature_key_dir =
            PathBuf::from("/nonexistent/geniepod-skill-keys");

        let posture = config.household_security_summary();
        let flags = posture["risk_flags"].as_array().unwrap();
        let has = |name: &str| flags.iter().any(|f| f == name);
        assert!(has("skill_signature_required_but_no_trusted_keys"));
        assert!(!has("skill_signature_not_required"));
        assert_eq!(
            posture["policy"]["skill_signature_required"],
            serde_json::json!(true)
        );
        assert_eq!(
            posture["policy"]["skill_signature_trusted_keys_present"],
            serde_json::json!(false)
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
    fn http_server_config_defaults_are_bounded() {
        let config = test_config();
        assert_eq!(config.http.max_request_line_bytes, 8 * 1024);
        assert_eq!(config.http.max_header_line_bytes, 8 * 1024);
        assert_eq!(config.http.max_header_count, 100);
        assert_eq!(config.http.max_header_bytes, 64 * 1024);
        assert_eq!(config.http.read_timeout_secs, 15);
        assert_eq!(config.http.max_connections, 256);
    }

    #[test]
    fn http_server_config_falls_back_when_section_absent() {
        // Existing deployments have no [http] section yet — the whole config
        // must still parse and use the hardened defaults.
        let config: Config = toml::from_str(
            r#"
[services.core]
url = "http://127.0.0.1:3000/api/health"
systemd_unit = "genie-core.service"

[services.llm]
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-ai-runtime.service"
"#,
        )
        .unwrap();

        assert_eq!(config.http.max_header_bytes, 64 * 1024);
        assert_eq!(config.http.max_connections, 256);
    }

    #[test]
    fn http_server_config_parses_overrides() {
        let config: HttpServerConfig = toml::from_str(
            r#"
max_request_line_bytes = 2048
max_header_line_bytes = 2048
max_header_count = 32
max_header_bytes = 16384
read_timeout_secs = 5
max_connections = 16
"#,
        )
        .unwrap();

        assert_eq!(config.max_request_line_bytes, 2048);
        assert_eq!(config.max_header_line_bytes, 2048);
        assert_eq!(config.max_header_count, 32);
        assert_eq!(config.max_header_bytes, 16384);
        assert_eq!(config.read_timeout_secs, 5);
        assert_eq!(config.max_connections, 16);
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
    use super::{LlmBackendKind, RuntimeBoundaryMode, ServiceEndpoint};
    use std::path::PathBuf;

    pub fn api_service() -> ServiceEndpoint {
        ServiceEndpoint {
            url: "http://127.0.0.1:3080/api/status".into(),
            systemd_unit: "genie-api.service".into(),
            backend: LlmBackendKind::default(),
        }
    }

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
    pub fn agent_context_window_tokens() -> u32 {
        4096
    }
    pub fn agent_ai_boundary() -> RuntimeBoundaryMode {
        RuntimeBoundaryMode::ExternalRuntime
    }
    pub fn agent_voice_boundary() -> RuntimeBoundaryMode {
        RuntimeBoundaryMode::TransitionalAdapter
    }
    pub fn agent_home_boundary() -> RuntimeBoundaryMode {
        RuntimeBoundaryMode::TransitionalAdapter
    }
    pub fn optional_ai_provider_api_key_env() -> String {
        "GENIEPOD_AI_PROVIDER_API_KEY".into()
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
    pub fn audio_output_device() -> String {
        "auto".to_string()
    }
    pub fn audio_device() -> String {
        "auto".into()
    }
    pub fn audio_denoiser() -> String {
        // alpha.7 default: try the neural denoiser first. Runtime falls back to
        // sox then none if the binary is absent, so this is safe even on hosts
        // that have not run the alpha.7 setup-jetson.sh yet.
        "deepfilternet".into()
    }
    pub fn deep_filter_path() -> PathBuf {
        PathBuf::from("/opt/geniepod/bin/deep-filter")
    }
    pub fn post_tts_silence_ms() -> u64 {
        // 1500 ms: empirical default that lets ALSA's hardware playback buffer
        // drain on Tegra HDA and the speaker/room decay fall below the
        // whisper-server no-speech threshold. Set lower on headphone-only
        // installs, higher on rooms with long reverberation.
        1500
    }
    pub fn deep_filter_atten_lim_db() -> f32 {
        100.0
    }
    pub fn audio_sample_rate() -> u32 {
        48000
    }
    pub fn voice_record_secs() -> u32 {
        3
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
    pub fn skill_signature_key_dir() -> PathBuf {
        PathBuf::from("/etc/geniepod/skill-keys")
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
    pub fn telegram_voice_max_duration_secs() -> u32 {
        60
    }
    pub fn telegram_voice_delete_temp_audio() -> bool {
        true
    }
    pub fn telegram_voice_ffmpeg_path() -> PathBuf {
        PathBuf::from("ffmpeg")
    }
    pub fn telegram_voice_max_reply_chars() -> usize {
        // Roughly the upper bound that comfortably encodes under Telegram's
        // 1 MB sendVoice limit at Piper's typical OGG/Opus output rate.
        // Long-form replies fall back to text.
        800
    }
    pub fn telegram_voice_max_parallel_voice() -> usize {
        // Two concurrent voice pipelines is enough to satisfy issue #77's
        // AC #8 (two voice messages from different chats transcribe in
        // parallel) while leaving headroom for ffmpeg / whisper-server on a
        // Jetson Orin Nano-class device. Bump in deployment configs if the
        // host has more CPU / a dedicated whisper-server.
        2
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
    pub fn http_max_request_line_bytes() -> usize {
        8 * 1024
    }
    pub fn http_max_header_line_bytes() -> usize {
        8 * 1024
    }
    pub fn http_max_header_count() -> usize {
        100
    }
    pub fn http_max_header_bytes() -> usize {
        // Mirror the genie-core 64 KiB body cap upward into the header phase.
        64 * 1024
    }
    pub fn http_read_timeout_secs() -> u64 {
        15
    }
    pub fn http_max_connections() -> usize {
        // Generous headroom over the handful of real clients (dashboard polls
        // every 5 s, plus voice/Telegram/local apps) while still bounding fan-out
        // so a connection flood cannot exhaust fds or wedge the single-threaded
        // genie-core runtime.
        256
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
