use genie_common::config::ActuationSafetyConfig;
use serde::{Deserialize, Serialize};

use super::{HomeAction, HomeActionKind, HomeState, HomeTarget, HomeTargetKind, IntegrationHealth};
use crate::tools::actuation::RequestOrigin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPolicyDecision {
    pub risk: ActionRisk,
    pub allowed: bool,
    pub requires_confirmation: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSafetyDecision {
    pub allowed: bool,
    pub reason: String,
}

impl RuntimeSafetyDecision {
    pub fn allow(reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            reason: reason.into(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason: reason.into(),
        }
    }
}

impl ActionPolicyDecision {
    pub fn allow(risk: ActionRisk, reason: impl Into<String>) -> Self {
        Self {
            risk,
            allowed: true,
            requires_confirmation: false,
            reason: reason.into(),
        }
    }

    pub fn require_confirmation(risk: ActionRisk, reason: impl Into<String>) -> Self {
        Self {
            risk,
            allowed: false,
            requires_confirmation: true,
            reason: reason.into(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            risk: ActionRisk::High,
            allowed: false,
            requires_confirmation: false,
            reason: reason.into(),
        }
    }
}

/// Assess whether a home action is safe to execute immediately.
///
/// This is intentionally conservative. GenieClaw is a shared-room appliance,
/// so risky physical actions need a real confirmation flow instead of trusting
/// the LLM to self-confirm a JSON argument.
pub fn assess_home_action(action: &HomeAction) -> ActionPolicyDecision {
    let target = &action.target;
    let domain = target.domain.as_deref().unwrap_or("");

    // Activating a script that is not marked voice-safe is a hard deny, never a
    // confirmable prompt. This has to be checked before the generic voice_safe
    // guard below: that guard returns `require_confirmation` for every
    // non-voice-safe target, so it used to swallow this case and leave the deny
    // branch unreachable — the caller then offered a confirmation for a script
    // the policy was written to refuse outright, and on confirm fell through to
    // execution (only the provider's own re-check stopped it).
    if matches!(action.kind, HomeActionKind::Activate)
        && matches!(target.kind, HomeTargetKind::Script)
        && !target.voice_safe
    {
        return ActionPolicyDecision::deny(format!(
            "{} is not a voice-safe script",
            target.display_name
        ));
    }

    if !target.voice_safe {
        return ActionPolicyDecision::require_confirmation(
            ActionRisk::High,
            format!("{} is not marked voice-safe", target.display_name),
        );
    }

    if matches!(domain, "lock" | "alarm_control_panel" | "camera") {
        return ActionPolicyDecision::require_confirmation(
            ActionRisk::High,
            format!(
                "{} controls a sensitive {} device",
                target.display_name, domain
            ),
        );
    }

    if matches!(action.kind, HomeActionKind::Unlock) {
        return ActionPolicyDecision::require_confirmation(
            ActionRisk::High,
            format!("unlocking {} requires confirmation", target.display_name),
        );
    }

    // Only `Open` actions consult the descriptor, so build it lazily here
    // instead of allocating a lowercased string for every turn_on/off/toggle/
    // brightness action. The `cover` domain short-circuits before the string
    // is built at all.
    if matches!(action.kind, HomeActionKind::Open)
        && (domain == "cover" || opens_physical_barrier(target))
    {
        return ActionPolicyDecision::require_confirmation(
            ActionRisk::High,
            format!("opening {} requires confirmation", target.display_name),
        );
    }

    let risk = match (domain, action.kind) {
        ("climate", HomeActionKind::SetTemperature) => ActionRisk::Medium,
        ("cover", HomeActionKind::Close) => ActionRisk::Medium,
        // `cover.toggle` opens-or-closes the same physical barrier as
        // `open`/`close`, so it must be at least as sensitive as a `close`
        // (Medium); otherwise a toggle silently skips the confirmation both
        // `open` (High, above) and `close` (Medium) require.
        ("cover", HomeActionKind::Toggle) => ActionRisk::Medium,
        ("script", HomeActionKind::Activate) => ActionRisk::Medium,
        _ => ActionRisk::Low,
    };
    ActionPolicyDecision::allow(risk, "allowed by local household policy")
}

/// Whether the target's name/query/entities suggest a physical barrier
/// (garage, door, gate) whose opening warrants confirmation. Builds the
/// lowercased descriptor on demand so non-`Open` actions never pay for it.
fn opens_physical_barrier(target: &HomeTarget) -> bool {
    let descriptor = format!(
        "{} {} {}",
        target.display_name,
        target.query,
        target.entity_ids.join(" ")
    )
    .to_lowercase();
    descriptor.contains("garage") || descriptor.contains("door") || descriptor.contains("gate")
}

pub fn assess_runtime_home_action(
    action: &HomeAction,
    policy: &ActionPolicyDecision,
    health: &IntegrationHealth,
    current_state: Option<&HomeState>,
    config: &ActuationSafetyConfig,
    origin: RequestOrigin,
    confirmed: bool,
) -> RuntimeSafetyDecision {
    if !config.enabled {
        return RuntimeSafetyDecision::allow("runtime safety gate disabled");
    }

    if !health.connected {
        return RuntimeSafetyDecision::deny(format!(
            "Home Assistant is not healthy enough for actuation: {}",
            health.message
        ));
    }

    if action.target.entity_ids.is_empty() {
        return RuntimeSafetyDecision::deny("target resolution produced no concrete entities");
    }

    let mut required_confidence = if matches!(policy.risk, ActionRisk::Medium | ActionRisk::High) {
        config.min_sensitive_confidence
    } else {
        config.min_target_confidence
    };

    if !confirmed {
        required_confidence = match origin {
            RequestOrigin::Voice => required_confidence.max(0.88),
            RequestOrigin::Telegram => required_confidence.max(0.90),
            RequestOrigin::Api => required_confidence.max(0.84),
            _ => required_confidence,
        };
    }

    if action.target.confidence < required_confidence {
        return RuntimeSafetyDecision::deny(format!(
            "target match confidence {:.2} is below required {:.2}",
            action.target.confidence, required_confidence
        ));
    }

    if config.deny_multi_target_sensitive
        && matches!(policy.risk, ActionRisk::Medium | ActionRisk::High)
        && action.target.entity_ids.len() > 1
    {
        return RuntimeSafetyDecision::deny(format!(
            "sensitive action targets {} entities, which is too broad",
            action.target.entity_ids.len()
        ));
    }

    if config.require_available_state
        && !matches!(
            action.target.kind,
            HomeTargetKind::Scene | HomeTargetKind::Script
        )
    {
        match current_state {
            Some(state) if state.available => {}
            Some(_) => {
                return RuntimeSafetyDecision::deny(format!(
                    "{} is currently unavailable",
                    action.target.display_name
                ));
            }
            None => {
                return RuntimeSafetyDecision::deny(format!(
                    "runtime safety check could not verify current state for {}",
                    action.target.display_name
                ));
            }
        }
    }

    RuntimeSafetyDecision::allow("passed runtime actuation safety checks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::{HomeTarget, HomeTargetKind};

    fn action(domain: &str, kind: HomeActionKind, name: &str, voice_safe: bool) -> HomeAction {
        HomeAction {
            kind,
            target: HomeTarget {
                kind: HomeTargetKind::Entity,
                query: name.into(),
                display_name: name.into(),
                entity_ids: vec![format!("{domain}.test")],
                domain: Some(domain.into()),
                area: Some("Living Room".into()),
                confidence: 0.9,
                voice_safe,
            },
            value: None,
        }
    }

    #[test]
    fn allows_basic_light_control() {
        let decision = assess_home_action(&action(
            "light",
            HomeActionKind::TurnOn,
            "Living room lamp",
            true,
        ));
        assert!(decision.allowed);
        assert_eq!(decision.risk, ActionRisk::Low);
    }

    #[test]
    fn requires_confirmation_for_locks() {
        let decision =
            assess_home_action(&action("lock", HomeActionKind::Unlock, "Front door", false));
        assert!(!decision.allowed);
        assert!(decision.requires_confirmation);
        assert_eq!(decision.risk, ActionRisk::High);
    }

    #[test]
    fn requires_confirmation_for_opening_garage_cover() {
        let decision =
            assess_home_action(&action("cover", HomeActionKind::Open, "Garage door", true));
        assert!(!decision.allowed);
        assert!(decision.requires_confirmation);
    }

    #[test]
    fn cover_toggle_is_as_sensitive_as_cover_close() {
        // A cover toggle opens-or-closes the same physical barrier, so it must
        // require confirmation like open/close (Medium, matching close).
        let toggle = action("cover", HomeActionKind::Toggle, "Garage door", true);
        assert_eq!(assess_home_action(&toggle).risk, ActionRisk::Medium);
    }

    #[test]
    fn denies_non_voice_safe_script_activation() {
        // A non-voice-safe script is a hard deny, not a confirmable prompt: the
        // generic voice_safe guard used to return first, so the caller offered a
        // confirmation the policy was written to refuse outright.
        let mut action = action("script", HomeActionKind::Activate, "Disarm alarm", false);
        action.target.kind = HomeTargetKind::Script;

        let decision = assess_home_action(&action);

        assert!(!decision.allowed);
        assert!(
            !decision.requires_confirmation,
            "a non-voice-safe script must be refused outright, got a confirmable prompt: {}",
            decision.reason
        );
        assert_eq!(decision.risk, ActionRisk::High);
        assert!(
            decision.reason.contains("not a voice-safe script"),
            "got: {}",
            decision.reason
        );
    }

    fn health() -> IntegrationHealth {
        IntegrationHealth {
            connected: true,
            cached_graph: false,
            message: "ok".into(),
        }
    }

    #[test]
    fn runtime_gate_blocks_low_confidence_target() {
        let mut action = action("light", HomeActionKind::TurnOn, "Living room lamp", true);
        action.target.confidence = 0.60;
        let policy = assess_home_action(&action);

        let decision = assess_runtime_home_action(
            &action,
            &policy,
            &health(),
            Some(&HomeState {
                target_name: "Living room lamp".into(),
                domain: Some("light".into()),
                area: Some("Living Room".into()),
                entities: Vec::new(),
                available: true,
                spoken_summary: "ok".into(),
            }),
            &ActuationSafetyConfig::default(),
            RequestOrigin::Dashboard,
            false,
        );

        assert!(!decision.allowed);
        assert!(decision.reason.contains("confidence"));
    }

    #[test]
    fn runtime_gate_blocks_sensitive_multi_target_actions() {
        let mut action = action("cover", HomeActionKind::Close, "All covers", true);
        action.target.entity_ids = vec!["cover.a".into(), "cover.b".into()];
        action.target.confidence = 0.95;
        let policy = assess_home_action(&action);

        let decision = assess_runtime_home_action(
            &action,
            &policy,
            &health(),
            Some(&HomeState {
                target_name: "All covers".into(),
                domain: Some("cover".into()),
                area: None,
                entities: Vec::new(),
                available: true,
                spoken_summary: "ok".into(),
            }),
            &ActuationSafetyConfig::default(),
            RequestOrigin::Dashboard,
            false,
        );

        assert!(!decision.allowed);
        assert!(decision.reason.contains("too broad"));
    }

    #[test]
    fn runtime_gate_allows_safe_available_low_risk_action() {
        let action = action("light", HomeActionKind::TurnOn, "Living room lamp", true);
        let policy = assess_home_action(&action);

        let decision = assess_runtime_home_action(
            &action,
            &policy,
            &health(),
            Some(&HomeState {
                target_name: "Living room lamp".into(),
                domain: Some("light".into()),
                area: Some("Living Room".into()),
                entities: Vec::new(),
                available: true,
                spoken_summary: "ok".into(),
            }),
            &ActuationSafetyConfig::default(),
            RequestOrigin::Dashboard,
            false,
        );

        assert!(decision.allowed);
    }
}
