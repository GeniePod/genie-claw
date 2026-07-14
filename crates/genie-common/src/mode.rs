use serde::{Deserialize, Serialize};
use std::fmt;

/// Operating modes for the GeniePod governor.
///
/// Each mode defines which services are running and at what capacity.
/// The governor transitions between modes based on time, user commands,
/// and memory pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Full voice pipeline + LLM 4B + HA + opt-in services.
    /// Default 06:00-23:00. Free RAM: ~2.2-3.2 GB.
    Day,

    /// Same as Day but with background tasks (summarize, analyze, morning briefing).
    /// Default 23:00-06:00.
    NightA,

    /// Unload 4B, load 9B for deeper reasoning. HA + opt-ins stopped.
    /// Free RAM: ~0.7-1.2 GB. Opt-in only.
    NightB,

    /// LLM unloaded, mpv launched for HDMI playback via NVDEC.
    /// Wake word + rules engine remain active for playback control.
    /// Free RAM: +2.8 GB from LLM unload.
    Media,

    /// Emergency mode: memory pressure critical.
    /// Opt-ins stopped, context capped, STT downgraded.
    Pressure,
}

impl Mode {
    /// Services that must be running in this mode.
    pub fn required_services(&self) -> &'static [&'static str] {
        match self {
            Mode::Day | Mode::NightA => &[
                "genie-wakeword",
                "genie-core",
                "llm",
                "genie-mqtt",
                "homeassistant",
            ],
            Mode::NightB => &["genie-wakeword", "genie-core", "llm", "genie-mqtt"],
            Mode::Media => &["genie-wakeword", "genie-core", "genie-mqtt"],
            Mode::Pressure => &["genie-wakeword", "genie-core", "llm", "genie-mqtt"],
        }
    }

    /// Services that must be stopped in this mode.
    pub fn stopped_services(&self) -> &'static [&'static str] {
        match self {
            Mode::Day | Mode::NightA => &[],
            Mode::NightB => &["homeassistant", "nextcloud", "jellyfin"],
            Mode::Media => &["llm", "nextcloud", "jellyfin"],
            Mode::Pressure => &["nextcloud", "jellyfin"],
        }
    }

    /// LLM model to use in this mode.
    pub fn llm_model(&self) -> Option<&'static str> {
        match self {
            Mode::Day | Mode::NightA | Mode::Pressure => Some("nemotron-4b-q4_k_m.gguf"),
            Mode::NightB => Some("nemotron-9b-q4.gguf"),
            Mode::Media => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Day => write!(f, "day"),
            Mode::NightA => write!(f, "night_a"),
            Mode::NightB => write!(f, "night_b"),
            Mode::Media => write!(f, "media"),
            Mode::Pressure => write!(f, "pressure"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Display persists the mode_transitions history and prints the transition
    // log, so it must match the snake_case identifier used everywhere else: the
    // serde rename_all form the governor control protocol accepts/emits, the
    // CLI (`genie-ctl mode night_a`), and the docs. Before this fix NightA and
    // NightB emitted hyphens. The expected values below are serde's snake_case
    // output for each variant.
    #[test]
    fn display_uses_snake_case_matching_serde() {
        assert_eq!(Mode::Day.to_string(), "day");
        assert_eq!(Mode::NightA.to_string(), "night_a");
        assert_eq!(Mode::NightB.to_string(), "night_b");
        assert_eq!(Mode::Media.to_string(), "media");
        assert_eq!(Mode::Pressure.to_string(), "pressure");
    }
}
