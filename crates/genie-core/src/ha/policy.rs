use serde::{Deserialize, Serialize};

use super::{HomeAction, HomeActionKind, HomeTargetKind};

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
    let descriptor = format!(
        "{} {} {}",
        target.display_name,
        target.query,
        target.entity_ids.join(" ")
    )
    .to_lowercase();

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

    if matches!(action.kind, HomeActionKind::Open)
        && (domain == "cover"
            || descriptor.contains("garage")
            || descriptor.contains("door")
            || descriptor.contains("gate"))
    {
        return ActionPolicyDecision::require_confirmation(
            ActionRisk::High,
            format!("opening {} requires confirmation", target.display_name),
        );
    }

    if matches!(action.kind, HomeActionKind::Activate)
        && matches!(target.kind, HomeTargetKind::Script)
        && !target.voice_safe
    {
        return ActionPolicyDecision::deny(format!(
            "{} is not a voice-safe script",
            target.display_name
        ));
    }

    let risk = match (domain, action.kind) {
        ("climate", HomeActionKind::SetTemperature) => ActionRisk::Medium,
        ("cover", HomeActionKind::Close) => ActionRisk::Medium,
        ("script", HomeActionKind::Activate) => ActionRisk::Medium,
        _ => ActionRisk::Low,
    };
    ActionPolicyDecision::allow(risk, "allowed by local household policy")
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
}
