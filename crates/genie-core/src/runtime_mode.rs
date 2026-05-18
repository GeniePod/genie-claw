//! Startup-mode selection for genie-core.

/// High-level interface selected at process startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    /// Run the voice loop.
    Voice,
    /// Run the HTTP API / daemon path.
    HttpOnly,
}

/// Why startup selected a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupReason {
    VoiceNotRequested,
    InteractivePushToTalk,
    WakewordDaemon,
    PushToTalkNeedsTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupDecision {
    pub mode: StartupMode,
    pub reason: StartupReason,
}

impl StartupDecision {
    pub fn enters_voice(self) -> bool {
        self.mode == StartupMode::Voice
    }

    pub fn blocked_push_to_talk(self) -> bool {
        self.reason == StartupReason::PushToTalkNeedsTerminal
    }
}

/// Decide whether startup may enter voice mode.
///
/// Push-to-talk consumes stdin, so it is only valid with an interactive
/// terminal. Wake-word mode owns its own listener process and can run as a
/// daemon under systemd.
pub fn decide_startup_mode(
    voice_requested: bool,
    stdin_interactive: bool,
    wakeword_available: bool,
) -> StartupDecision {
    if !voice_requested {
        return StartupDecision {
            mode: StartupMode::HttpOnly,
            reason: StartupReason::VoiceNotRequested,
        };
    }

    if wakeword_available {
        return StartupDecision {
            mode: StartupMode::Voice,
            reason: StartupReason::WakewordDaemon,
        };
    }

    if stdin_interactive {
        return StartupDecision {
            mode: StartupMode::Voice,
            reason: StartupReason::InteractivePushToTalk,
        };
    }

    StartupDecision {
        mode: StartupMode::HttpOnly,
        reason: StartupReason::PushToTalkNeedsTerminal,
    }
}

#[cfg(test)]
mod tests {
    use super::{StartupMode, StartupReason, decide_startup_mode};

    #[test]
    fn systemd_push_to_talk_falls_back_to_http() {
        let decision = decide_startup_mode(true, false, false);
        assert_eq!(decision.mode, StartupMode::HttpOnly);
        assert_eq!(decision.reason, StartupReason::PushToTalkNeedsTerminal);
        assert!(decision.blocked_push_to_talk());
    }

    #[test]
    fn terminal_push_to_talk_still_runs() {
        let decision = decide_startup_mode(true, true, false);
        assert_eq!(decision.mode, StartupMode::Voice);
        assert_eq!(decision.reason, StartupReason::InteractivePushToTalk);
        assert!(decision.enters_voice());
    }

    #[test]
    fn wakeword_voice_can_run_without_terminal() {
        let decision = decide_startup_mode(true, false, true);
        assert_eq!(decision.mode, StartupMode::Voice);
        assert_eq!(decision.reason, StartupReason::WakewordDaemon);
        assert!(decision.enters_voice());
    }

    #[test]
    fn voice_disabled_stays_http_only() {
        let decision = decide_startup_mode(false, true, true);
        assert_eq!(decision.mode, StartupMode::HttpOnly);
        assert_eq!(decision.reason, StartupReason::VoiceNotRequested);
    }
}
