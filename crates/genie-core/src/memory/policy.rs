//! Shared-space memory policy for GeniePod Home.
//!
//! This is the code-level version of the product memory policy:
//! household memory is useful by default, private memory is opt-in, and
//! high-risk secrets should not be captured through room voice.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    Session,
    Household,
    Person,
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySensitivity {
    Normal,
    Cautious,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenMemoryPolicy {
    Allow,
    Confirm,
    AppOnly,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdentityConfidence {
    Unknown,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryDisclosure {
    Speak,
    Confirm,
    AppOnly,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPolicyMetadata {
    pub scope: MemoryScope,
    pub sensitivity: MemorySensitivity,
    pub spoken_policy: SpokenMemoryPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryReadContext {
    pub identity_confidence: IdentityConfidence,
    pub explicit_named_person: bool,
    pub explicit_private_intent: bool,
    pub shared_space_voice: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPolicyDecision {
    pub allowed: bool,
    pub disclosure: MemoryDisclosure,
    pub reason: &'static str,
}

impl MemoryReadContext {
    pub fn shared_room_voice() -> Self {
        Self {
            identity_confidence: IdentityConfidence::Unknown,
            explicit_named_person: false,
            explicit_private_intent: false,
            shared_space_voice: true,
        }
    }
}

/// Infer V1 policy metadata for memories stored by the current compact schema.
///
/// The current database does not yet persist scope/sensitivity fields, so this
/// function provides conservative defaults and high-risk detection until the
/// expanded memory schema lands.
pub fn infer_metadata(kind: &str, content: &str) -> MemoryPolicyMetadata {
    let lower = content.to_lowercase();
    let private_intent = has_private_intent(&lower);
    let restricted = restricted_secret_reason(&lower).is_some();
    let cautious = is_cautious_memory(kind, &lower);

    let scope = if private_intent {
        MemoryScope::Private
    } else {
        MemoryScope::Household
    };

    let sensitivity = if restricted {
        MemorySensitivity::Restricted
    } else if cautious || private_intent {
        MemorySensitivity::Cautious
    } else {
        MemorySensitivity::Normal
    };

    let spoken_policy = match (scope, sensitivity) {
        (_, MemorySensitivity::Restricted) => SpokenMemoryPolicy::Deny,
        (MemoryScope::Private, _) => SpokenMemoryPolicy::AppOnly,
        (_, MemorySensitivity::Cautious) => SpokenMemoryPolicy::Confirm,
        _ => SpokenMemoryPolicy::Allow,
    };

    MemoryPolicyMetadata {
        scope,
        sensitivity,
        spoken_policy,
    }
}

/// Decide whether a proposed memory may be written by voice/tool flow.
pub fn assess_memory_write(kind: &str, content: &str) -> MemoryPolicyDecision {
    let lower = content.to_lowercase();
    if let Some(reason) = restricted_secret_reason(&lower) {
        return MemoryPolicyDecision {
            allowed: false,
            disclosure: MemoryDisclosure::Deny,
            reason,
        };
    }

    let metadata = infer_metadata(kind, content);
    if metadata.scope == MemoryScope::Private {
        return MemoryPolicyDecision {
            allowed: false,
            disclosure: MemoryDisclosure::AppOnly,
            reason: "Private personal memory requires an explicit app-backed flow in V1.",
        };
    }

    MemoryPolicyDecision {
        allowed: true,
        disclosure: MemoryDisclosure::Speak,
        reason: "Memory is safe for household-shared storage.",
    }
}

/// Decide whether a memory is safe to use in the current response context.
pub fn assess_memory_read(
    metadata: MemoryPolicyMetadata,
    context: MemoryReadContext,
) -> MemoryPolicyDecision {
    if metadata.spoken_policy == SpokenMemoryPolicy::Deny
        || metadata.sensitivity == MemorySensitivity::Restricted
    {
        return MemoryPolicyDecision {
            allowed: false,
            disclosure: MemoryDisclosure::Deny,
            reason: "Memory is restricted and must not be spoken.",
        };
    }

    match metadata.scope {
        MemoryScope::Session | MemoryScope::Household => match metadata.spoken_policy {
            SpokenMemoryPolicy::Allow => MemoryPolicyDecision {
                allowed: true,
                disclosure: MemoryDisclosure::Speak,
                reason: "Household memory is safe for shared-space use.",
            },
            SpokenMemoryPolicy::Confirm => MemoryPolicyDecision {
                allowed: false,
                disclosure: MemoryDisclosure::Confirm,
                reason: "Cautious household memory requires confirmation before speaking.",
            },
            SpokenMemoryPolicy::AppOnly => MemoryPolicyDecision {
                allowed: false,
                disclosure: MemoryDisclosure::AppOnly,
                reason: "Memory should be shown in the app instead of spoken.",
            },
            SpokenMemoryPolicy::Deny => MemoryPolicyDecision {
                allowed: false,
                disclosure: MemoryDisclosure::Deny,
                reason: "Memory policy denies spoken disclosure.",
            },
        },
        MemoryScope::Person => {
            if context.explicit_named_person
                || context.identity_confidence >= IdentityConfidence::Medium
            {
                MemoryPolicyDecision {
                    allowed: true,
                    disclosure: MemoryDisclosure::Speak,
                    reason: "Person-linked household memory is eligible in this context.",
                }
            } else {
                MemoryPolicyDecision {
                    allowed: false,
                    disclosure: MemoryDisclosure::Confirm,
                    reason: "Person-linked memory needs explicit naming or stronger identity confidence.",
                }
            }
        }
        MemoryScope::Private => {
            if context.explicit_private_intent && !context.shared_space_voice {
                MemoryPolicyDecision {
                    allowed: false,
                    disclosure: MemoryDisclosure::AppOnly,
                    reason: "Private memory should be presented through a personal interface.",
                }
            } else {
                MemoryPolicyDecision {
                    allowed: false,
                    disclosure: MemoryDisclosure::Deny,
                    reason: "Private memory is not spoken in shared-room voice.",
                }
            }
        }
    }
}

pub fn may_inject_into_shared_prompt(kind: &str, content: &str) -> bool {
    let metadata = infer_metadata(kind, content);
    assess_memory_read(metadata, MemoryReadContext::shared_room_voice()).allowed
}

fn has_private_intent(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "remember this privately",
            "private memory",
            "private note",
            "for me only",
            "do not say this aloud",
            "don't say this aloud",
        ],
    )
}

fn is_cautious_memory(kind: &str, lower: &str) -> bool {
    kind.eq_ignore_ascii_case("private")
        || contains_any(
            lower,
            &[
                "medical diagnosis",
                "mental health",
                "therapy session",
                "legal problem",
                "personal secret",
            ],
        )
}

fn restricted_secret_reason(lower: &str) -> Option<&'static str> {
    if contains_any(
        lower,
        &[
            "password",
            "passcode",
            "one-time code",
            "one time code",
            "otp",
            "2fa code",
            "recovery code",
            "seed phrase",
            "recovery phrase",
            "private key",
            "secret key",
            "api key",
            "access token",
        ],
    ) {
        return Some(
            "I should not store passwords, tokens, keys, or one-time codes as voice memory.",
        );
    }

    if contains_any(
        lower,
        &[
            "credit card",
            "card number",
            "cvv",
            "bank account",
            "routing number",
            "social security",
            "ssn",
            "passport number",
            "driver license number",
            "government id",
        ],
    ) {
        return Some(
            "I should not store payment, banking, or government ID details as voice memory.",
        );
    }

    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn household_memory_can_be_spoken_in_shared_room() {
        let metadata = infer_metadata("preference", "User likes jazz music");
        let decision = assess_memory_read(metadata, MemoryReadContext::shared_room_voice());

        assert!(decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Speak);
    }

    #[test]
    fn password_memory_is_rejected() {
        let decision = assess_memory_write("fact", "my password is swordfish");

        assert!(!decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Deny);
        assert!(decision.reason.contains("passwords"));
    }

    #[test]
    fn private_memory_is_not_spoken_in_shared_room() {
        let metadata = MemoryPolicyMetadata {
            scope: MemoryScope::Private,
            sensitivity: MemorySensitivity::Cautious,
            spoken_policy: SpokenMemoryPolicy::AppOnly,
        };

        let decision = assess_memory_read(metadata, MemoryReadContext::shared_room_voice());

        assert!(!decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Deny);
    }

    #[test]
    fn person_memory_needs_name_or_identity_confidence() {
        let metadata = MemoryPolicyMetadata {
            scope: MemoryScope::Person,
            sensitivity: MemorySensitivity::Normal,
            spoken_policy: SpokenMemoryPolicy::Allow,
        };

        let low = assess_memory_read(metadata, MemoryReadContext::shared_room_voice());
        assert!(!low.allowed);

        let medium = assess_memory_read(
            metadata,
            MemoryReadContext {
                identity_confidence: IdentityConfidence::Medium,
                explicit_named_person: false,
                explicit_private_intent: false,
                shared_space_voice: true,
            },
        );
        assert!(medium.allowed);
    }
}
