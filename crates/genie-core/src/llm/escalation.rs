//! Local-first escalation policy for gated cloud providers (#570).
//!
//! PrivacyProxy is the only cloud escalation surface today. These helpers keep
//! routing on the local model unless the configured trigger and gate allow an
//! anonymized outbound call, and they summarize (never log raw) outbound payload
//! size for the operator audit trail.

use genie_common::config::{EscalationTrigger, PrivacyProxyConfig};

use super::Message;

/// Why a turn is being considered for cloud escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationReason {
    ContextOverflow,
    LocalDecline,
}

impl EscalationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EscalationReason::ContextOverflow => "context_overflow",
            EscalationReason::LocalDecline => "local_decline",
        }
    }
}

/// Local-first gate: return the proxy config only when escalation is allowed.
///
/// Requires an enabled PrivacyProxy with a localhost-valid endpoint and a trigger
/// that matches `reason`. Otherwise returns `None` and the caller stays local.
pub fn may_escalate(
    proxy: Option<&PrivacyProxyConfig>,
    reason: EscalationReason,
) -> Option<&PrivacyProxyConfig> {
    let proxy = proxy.filter(|p| p.enabled && p.endpoint_is_valid())?;
    let allowed = match reason {
        EscalationReason::ContextOverflow => matches!(
            proxy.trigger,
            EscalationTrigger::ContextOverflow | EscalationTrigger::LocalDeclineOrContextOverflow
        ),
        EscalationReason::LocalDecline => matches!(
            proxy.trigger,
            EscalationTrigger::LocalDecline | EscalationTrigger::LocalDeclineOrContextOverflow
        ),
    };
    allowed.then_some(proxy)
}

/// Non-content summary of what would leave the device on escalation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationPayloadSummary {
    pub message_count: usize,
    pub payload_chars: usize,
}

pub fn summarize_messages(messages: &[Message]) -> EscalationPayloadSummary {
    EscalationPayloadSummary {
        message_count: messages.len(),
        payload_chars: messages.iter().map(|m| m.content.len()).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::PrivacyProxyConfig;

    fn sample_proxy(trigger: EscalationTrigger) -> PrivacyProxyConfig {
        PrivacyProxyConfig {
            enabled: true,
            trigger,
            ..PrivacyProxyConfig::default()
        }
    }

    #[test]
    fn may_escalate_requires_enabled_local_proxy() {
        let mut proxy = sample_proxy(EscalationTrigger::LocalDeclineOrContextOverflow);
        proxy.enabled = false;
        assert!(may_escalate(Some(&proxy), EscalationReason::LocalDecline).is_none());

        proxy.enabled = true;
        proxy.base_url = "https://api.openai.com/v1".into();
        assert!(may_escalate(Some(&proxy), EscalationReason::LocalDecline).is_none());
    }

    #[test]
    fn context_overflow_trigger_matrix() {
        let both = sample_proxy(EscalationTrigger::LocalDeclineOrContextOverflow);
        assert!(may_escalate(Some(&both), EscalationReason::ContextOverflow).is_some());
        assert!(may_escalate(Some(&both), EscalationReason::LocalDecline).is_some());

        let overflow_only = sample_proxy(EscalationTrigger::ContextOverflow);
        assert!(may_escalate(Some(&overflow_only), EscalationReason::ContextOverflow).is_some());
        assert!(may_escalate(Some(&overflow_only), EscalationReason::LocalDecline).is_none());

        let decline_only = sample_proxy(EscalationTrigger::LocalDecline);
        assert!(may_escalate(Some(&decline_only), EscalationReason::ContextOverflow).is_none());
        assert!(may_escalate(Some(&decline_only), EscalationReason::LocalDecline).is_some());
    }

    #[test]
    fn summarize_messages_counts_chars_not_content() {
        let messages = vec![
            Message {
                role: "system".into(),
                content: "abcd".into(),
            },
            Message {
                role: "user".into(),
                content: "ef".into(),
            },
        ];
        let summary = summarize_messages(&messages);
        assert_eq!(summary.message_count, 2);
        assert_eq!(summary.payload_chars, 6);
    }
}
