//! Shared-space memory policy for the NVIDIA Jetson Orin 8GB-native home agent.
//!
//! This is the code-level version of the product memory policy:
//! household memory is useful by default, private memory is opt-in, and
//! high-risk secrets and sensitive household locations should not be
//! captured through room voice.

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

impl IdentityConfidence {
    /// Stable lowercase representation for logs/audit trails — independent of
    /// the derived `Debug` repr, which is free to change with variant names.
    pub fn as_str(&self) -> &'static str {
        match self {
            IdentityConfidence::Unknown => "unknown",
            IdentityConfidence::Low => "low",
            IdentityConfidence::Medium => "medium",
            IdentityConfidence::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryDisclosure {
    Speak,
    Confirm,
    AppOnly,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryDisclosureClass {
    Public,
    Household,
    Person,
    Sensitive,
    Private,
    Restricted,
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
    pub class: MemoryDisclosureClass,
    pub reason: &'static str,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Household => "household",
            Self::Person => "person",
            Self::Private => "private",
        }
    }

    pub fn from_storage(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "session" => Self::Session,
            "person" => Self::Person,
            "private" => Self::Private,
            _ => Self::Household,
        }
    }
}

impl MemorySensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Cautious => "cautious",
            Self::Restricted => "restricted",
        }
    }

    pub fn from_storage(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "cautious" => Self::Cautious,
            "restricted" => Self::Restricted,
            _ => Self::Normal,
        }
    }
}

impl SpokenMemoryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Confirm => "confirm",
            Self::AppOnly => "app_only",
            Self::Deny => "deny",
        }
    }

    pub fn from_storage(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "confirm" => Self::Confirm,
            "app_only" => Self::AppOnly,
            "deny" => Self::Deny,
            _ => Self::Allow,
        }
    }
}

impl MemoryDisclosureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Household => "household",
            Self::Person => "person",
            Self::Sensitive => "sensitive",
            Self::Private => "private",
            Self::Restricted => "restricted",
        }
    }
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

pub fn classify_memory(metadata: MemoryPolicyMetadata) -> MemoryDisclosureClass {
    if metadata.sensitivity == MemorySensitivity::Restricted
        || metadata.spoken_policy == SpokenMemoryPolicy::Deny
    {
        return MemoryDisclosureClass::Restricted;
    }

    if metadata.scope == MemoryScope::Private {
        return MemoryDisclosureClass::Private;
    }

    if metadata.sensitivity == MemorySensitivity::Cautious
        || metadata.spoken_policy == SpokenMemoryPolicy::Confirm
    {
        return MemoryDisclosureClass::Sensitive;
    }

    match metadata.scope {
        MemoryScope::Session | MemoryScope::Household => MemoryDisclosureClass::Household,
        MemoryScope::Person => MemoryDisclosureClass::Person,
        MemoryScope::Private => MemoryDisclosureClass::Private,
    }
}

fn decision_for_metadata(
    metadata: MemoryPolicyMetadata,
    allowed: bool,
    disclosure: MemoryDisclosure,
    reason: &'static str,
) -> MemoryPolicyDecision {
    MemoryPolicyDecision {
        allowed,
        disclosure,
        class: classify_memory(metadata),
        reason,
    }
}

fn restricted_decision(reason: &'static str) -> MemoryPolicyDecision {
    MemoryPolicyDecision {
        allowed: false,
        disclosure: MemoryDisclosure::Deny,
        class: MemoryDisclosureClass::Restricted,
        reason,
    }
}

/// Infer V1 policy metadata from the memory kind and content.
///
/// This is used both when new memories are stored and when older databases are
/// backfilled into the persisted scope/sensitivity/spoken-policy columns.
pub fn infer_metadata(kind: &str, content: &str) -> MemoryPolicyMetadata {
    let kind_lower = kind.trim().to_lowercase();
    let content_lower = content.to_lowercase();
    infer_metadata_lower(kind, &kind_lower, &content_lower)
}

fn infer_metadata_lower(kind: &str, kind_lower: &str, content_lower: &str) -> MemoryPolicyMetadata {
    let private_intent = kind_lower == "private"
        || kind_lower.starts_with("private_")
        || (needs_private_intent_scan(content_lower) && has_private_intent(content_lower));
    let person_linked = kind_lower == "person"
        || kind_lower.starts_with("person_")
        || kind_lower == "person-linked"
        || kind_lower == "person_linked";
    let restricted = needs_restricted_secret_scan(content_lower)
        && restricted_secret_reason(content_lower).is_some();
    let cautious = is_cautious_memory(kind, content_lower);

    let scope = if private_intent {
        MemoryScope::Private
    } else if person_linked {
        MemoryScope::Person
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
    let content_lower = content.to_lowercase();
    if needs_restricted_secret_scan(&content_lower)
        && let Some(reason) = restricted_secret_reason(&content_lower)
    {
        return restricted_decision(reason);
    }

    let kind_lower = kind.trim().to_lowercase();
    let metadata = infer_metadata_lower(kind, &kind_lower, &content_lower);
    if metadata.scope == MemoryScope::Private {
        return decision_for_metadata(
            metadata,
            false,
            MemoryDisclosure::AppOnly,
            "Private personal memory requires an explicit app-backed flow in V1.",
        );
    }

    decision_for_metadata(
        metadata,
        true,
        MemoryDisclosure::Speak,
        "Memory is safe for household-shared storage.",
    )
}

/// Decide whether a memory is safe to use in the current response context.
pub fn assess_memory_read(
    metadata: MemoryPolicyMetadata,
    context: MemoryReadContext,
) -> MemoryPolicyDecision {
    if metadata.spoken_policy == SpokenMemoryPolicy::Deny
        || metadata.sensitivity == MemorySensitivity::Restricted
    {
        return decision_for_metadata(
            metadata,
            false,
            MemoryDisclosure::Deny,
            "Memory is restricted and must not be spoken.",
        );
    }

    match metadata.scope {
        MemoryScope::Session | MemoryScope::Household => match metadata.spoken_policy {
            SpokenMemoryPolicy::Allow => decision_for_metadata(
                metadata,
                true,
                MemoryDisclosure::Speak,
                "Household memory is safe for shared-space use.",
            ),
            SpokenMemoryPolicy::Confirm => decision_for_metadata(
                metadata,
                false,
                MemoryDisclosure::Confirm,
                "Cautious household memory requires confirmation before speaking.",
            ),
            SpokenMemoryPolicy::AppOnly => decision_for_metadata(
                metadata,
                false,
                MemoryDisclosure::AppOnly,
                "Memory should be shown in the app instead of spoken.",
            ),
            SpokenMemoryPolicy::Deny => decision_for_metadata(
                metadata,
                false,
                MemoryDisclosure::Deny,
                "Memory policy denies spoken disclosure.",
            ),
        },
        MemoryScope::Person => {
            if context.explicit_named_person
                || context.identity_confidence >= IdentityConfidence::Medium
            {
                decision_for_metadata(
                    metadata,
                    true,
                    MemoryDisclosure::Speak,
                    "Person-linked household memory is eligible in this context.",
                )
            } else {
                decision_for_metadata(
                    metadata,
                    false,
                    MemoryDisclosure::Confirm,
                    "Person-linked memory needs explicit naming or stronger identity confidence.",
                )
            }
        }
        MemoryScope::Private => {
            if context.explicit_private_intent && !context.shared_space_voice {
                decision_for_metadata(
                    metadata,
                    false,
                    MemoryDisclosure::AppOnly,
                    "Private memory should be presented through a personal interface.",
                )
            } else {
                decision_for_metadata(
                    metadata,
                    false,
                    MemoryDisclosure::Deny,
                    "Private memory is not spoken in shared-room voice.",
                )
            }
        }
    }
}

/// Escalation policy for cloud routing via PrivacyProxy (issue #418).
///
/// Determines whether a memory fact may be included in a request forwarded
/// through the on-device anonymizing proxy to a cloud model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationPolicy {
    /// Fact must remain on-device. Never send it even through an anonymizing proxy.
    LocalOnly,
    /// Fact is eligible for cloud escalation via PrivacyProxy.
    /// PrivacyProxy applies deterministic identifier masking before forwarding.
    Anonymized,
}

/// Determine the escalation policy for a memory fact based on its policy metadata.
///
/// `Private` scope, `Restricted` sensitivity, and `Cautious` sensitivity facts are
/// always `LocalOnly`: they must not travel through any proxy, even an anonymizing
/// one, because the proxy sees the raw content before masking. Health data, identity
/// entries, and other cautious content fall into this bucket.
pub fn escalation_policy(metadata: MemoryPolicyMetadata) -> EscalationPolicy {
    match (metadata.scope, metadata.sensitivity) {
        (MemoryScope::Private, _)
        | (_, MemorySensitivity::Restricted)
        | (_, MemorySensitivity::Cautious) => EscalationPolicy::LocalOnly,
        _ => EscalationPolicy::Anonymized,
    }
}

/// Return true when a memory fact (by kind + content) is eligible for cloud escalation.
///
/// Identity entries are always `LocalOnly` regardless of sensitivity: the person's
/// name is the primary identifier the proxy is trying to mask, so it must not appear
/// in any forwarded payload — not even through an anonymising gateway.
pub fn eligible_for_escalation(kind: &str, content: &str) -> bool {
    if kind.eq_ignore_ascii_case("identity") {
        return false;
    }
    escalation_policy(infer_metadata(kind, content)) == EscalationPolicy::Anonymized
}

/// Extract entity-level vocabulary terms from a memory entry for PrivacyProxy seeding.
///
/// PrivacyProxy builds its substitution map ("Alex" → "__PERSON_1__") from individual
/// entity names, not from full sentences. Seeding whole content strings prevents the
/// proxy from constructing a usable substitution table.
///
/// Returns deduplicated runs of consecutive title-cased words, skipping common
/// sentence-initial words that are capitalised by convention rather than by being
/// proper nouns. Multi-word names ("Alex Morgan") are emitted in full and the first
/// token alone ("Alex") is also included so that first-name references in chat are
/// masked correctly.
///
/// Only call this on entries whose [`eligible_for_escalation`] returned `true`;
/// `LocalOnly` entries must never be seeded.
pub fn extract_vocab_terms(_kind: &str, content: &str) -> Vec<String> {
    // Common sentence-initial words that appear Title-Cased but are not proper nouns.
    const STOP_WORDS: &[&str] = &[
        "User", "Users", "The", "A", "An", "My", "Our", "Your", "His", "Her", "Their", "Its",
        "This", "That", "These", "Those", "There", "Here", "We", "You", "He", "She", "They", "It",
        "I",
    ];

    let mut terms: Vec<String> = Vec::new();
    let words: Vec<&str> = content.split_whitespace().collect();
    let mut i = 0;

    while i < words.len() {
        let alpha: String = words[i].chars().filter(|c| c.is_alphabetic()).collect();
        let is_proper = alpha.len() > 1
            && alpha
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
            && !STOP_WORDS.contains(&alpha.as_str());

        if is_proper {
            let run_start = i;
            i += 1;
            while i < words.len() {
                let next_alpha: String = words[i].chars().filter(|c| c.is_alphabetic()).collect();
                if next_alpha.len() > 1
                    && next_alpha
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false)
                    && !STOP_WORDS.contains(&next_alpha.as_str())
                {
                    i += 1;
                } else {
                    break;
                }
            }
            let run: Vec<String> = words[run_start..i]
                .iter()
                .map(|w| w.chars().filter(|c| c.is_alphabetic()).collect())
                .collect();
            let full = run.join(" ");
            if !terms.contains(&full) {
                terms.push(full);
            }
            // For multi-word names also include the first token alone so that
            // first-name-only references in chat are masked ("Alex" as well as
            // "Alex Morgan").
            if run.len() > 1 && !terms.contains(&run[0]) {
                terms.push(run[0].clone());
            }
        } else {
            i += 1;
        }
    }

    terms
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
        || (needs_cautious_scan(lower)
            && contains_any(
                lower,
                &[
                    "medical diagnosis",
                    "mental health",
                    "therapy session",
                    "legal problem",
                    "personal secret",
                    // Health data must not reach a cloud proxy even through an anonymizing
                    // gateway; the proxy sees raw content before masking.
                    "medication",
                    "prescription",
                    "diagnosed with",
                    "for diabetes",
                    "for cancer",
                    "for depression",
                    "for anxiety",
                    "for hypertension",
                    "for epilepsy",
                    "for asthma",
                    "insulin",
                    "chemotherapy",
                    "dialysis",
                ],
            ))
}

fn needs_restricted_secret_scan(lower: &str) -> bool {
    lower.contains("password")
        || lower.contains("passcode")
        || lower.contains("pass:")
        || lower.contains(" pass:")
        || lower.contains("one-time")
        || lower.contains("one time")
        || lower.contains("otp")
        || lower.contains("2fa")
        || lower.contains("recovery")
        || lower.contains("seed phrase")
        || lower.contains("private key")
        || lower.contains("secret key")
        || lower.contains("api key")
        || lower.contains("access token")
        || lower.contains("gate code")
        || lower.contains("door code")
        || lower.contains("garage code")
        || lower.contains("alarm code")
        || lower.contains("security code")
        || lower.contains("safe code")
        || lower.contains("combination")
        || lower.contains(" lock combo")
        || lower.contains("credit card")
        || lower.contains("card number")
        || lower.contains("cvv")
        || lower.contains("account number")
        || lower.contains("confirmation number")
        || lower.contains("bank account")
        || lower.contains("routing number")
        || lower.contains("social security")
        || lower.contains("ssn")
        || lower.contains("passport")
        || lower.contains("driver license")
        || lower.contains("government id")
        || lower.contains(" is in ")
        || lower.contains(" are in ")
        || lower.contains(" is inside ")
        || lower.contains(" are inside ")
        || lower.contains(" is kept ")
        || lower.contains(" are kept ")
        || lower.contains(" is stored ")
        || lower.contains(" are stored ")
        || lower.contains(" is hidden ")
        || lower.contains(" are hidden ")
        || lower.contains(" under ")
        || lower.contains(" behind ")
        || lower.contains("birth certificate")
        || lower.contains("house key")
        || lower.contains("spare key")
        || lower.contains("safe key")
        || lower.contains("car title")
        || lower.contains("property deed")
        || lower.contains("documents are")
        || lower.contains("valuables are")
        || lower.contains("important documents")
}

fn needs_private_intent_scan(lower: &str) -> bool {
    lower.contains("private")
        || lower.contains("for me only")
        || lower.contains("aloud")
        || lower.contains("remember this")
}

fn needs_cautious_scan(lower: &str) -> bool {
    lower.contains("medical")
        || lower.contains("mental health")
        || lower.contains("therapy")
        || lower.contains("legal")
        || lower.contains("secret")
        || lower.contains("medication")
        || lower.contains("prescription")
        || lower.contains("diagnosed")
        || lower.contains("diabetes")
        || lower.contains("cancer")
        || lower.contains("depression")
        || lower.contains("anxiety")
        || lower.contains("hypertension")
        || lower.contains("epilepsy")
        || lower.contains("asthma")
        || lower.contains("insulin")
        || lower.contains("chemotherapy")
        || lower.contains("dialysis")
}

fn restricted_secret_reason(lower: &str) -> Option<&'static str> {
    if contains_any(
        lower,
        &[
            "password",
            "pass:",
            " pass:",
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
            "gate code",
            "door code",
            "garage code",
            "alarm code",
            "security code",
            "safe code",
            "safe combination",
            "lock combination",
            "lock combo",
            "bike lock combo",
            "bicycle lock combo",
            "bike lock combination",
            "bicycle lock combination",
        ],
    ) {
        return Some(
            "I should not store household access codes or lock combinations as voice memory.",
        );
    }

    if contains_any(
        lower,
        &[
            "credit card",
            "card number",
            "cvv",
            "account number",
            "confirmation number",
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

    if describes_sensitive_location(lower) {
        return Some(
            "I should not store sensitive document, key, or safe locations as voice memory.",
        );
    }

    None
}

fn describes_sensitive_location(lower: &str) -> bool {
    let sensitive_object = contains_any(
        lower,
        &[
            "passport",
            "passports",
            "birth certificate",
            "birth certificates",
            "social security card",
            "ssn card",
            "government id",
            "house key",
            "spare key",
            "safe key",
            "car title",
            "property deed",
        ],
    );
    if sensitive_object && contains_location_phrase(lower) {
        return true;
    }

    contains_any(
        lower,
        &[
            "documents are in the safe",
            "documents are inside the safe",
            "important documents are in",
            "important documents are kept",
            "valuables are in the safe",
            "valuables are inside the safe",
        ],
    )
}

fn contains_location_phrase(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            " is in ",
            " are in ",
            " is inside ",
            " are inside ",
            " is kept in ",
            " are kept in ",
            " is kept at ",
            " are kept at ",
            " is stored in ",
            " are stored in ",
            " is stored at ",
            " are stored at ",
            " is hidden in ",
            " are hidden in ",
            " is hidden under ",
            " are hidden under ",
            " under ",
            " behind ",
        ],
    )
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
        assert_eq!(decision.class, MemoryDisclosureClass::Household);
        assert_eq!(decision.class.as_str(), "household");
    }

    #[test]
    fn password_memory_is_rejected() {
        let decision = assess_memory_write("fact", "my password is swordfish");

        assert!(!decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Deny);
        assert_eq!(decision.class, MemoryDisclosureClass::Restricted);
        assert!(decision.reason.contains("passwords"));
    }

    #[test]
    fn household_access_code_memory_is_rejected() {
        let decision = assess_memory_write("fact", "the gate code is 5829");

        assert!(!decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Deny);
        assert_eq!(decision.class, MemoryDisclosureClass::Restricted);
        assert!(decision.reason.contains("access codes"));
    }

    #[test]
    fn sensitive_location_memory_is_rejected() {
        let decision = assess_memory_write("fact", "the passports are in the safe");

        assert!(!decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Deny);
        assert_eq!(decision.class, MemoryDisclosureClass::Restricted);
        assert!(decision.reason.contains("sensitive document"));
    }

    #[test]
    fn cautious_memory_is_classified_sensitive() {
        let metadata = infer_metadata("fact", "User has a recent medical diagnosis of mild asthma");
        let decision = assess_memory_read(metadata, MemoryReadContext::shared_room_voice());

        assert!(!decision.allowed);
        assert_eq!(decision.disclosure, MemoryDisclosure::Confirm);
        assert_eq!(decision.class, MemoryDisclosureClass::Sensitive);
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
        assert_eq!(decision.class, MemoryDisclosureClass::Private);
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
        assert_eq!(low.class, MemoryDisclosureClass::Person);

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
        assert_eq!(medium.class, MemoryDisclosureClass::Person);
    }

    #[test]
    fn household_normal_memory_is_anonymized_eligible() {
        let metadata = infer_metadata("preference", "User likes jazz music");
        assert_eq!(escalation_policy(metadata), EscalationPolicy::Anonymized);
        assert!(eligible_for_escalation(
            "preference",
            "User likes jazz music"
        ));
    }

    #[test]
    fn private_scope_memory_is_local_only() {
        let metadata = MemoryPolicyMetadata {
            scope: MemoryScope::Private,
            sensitivity: MemorySensitivity::Normal,
            spoken_policy: SpokenMemoryPolicy::AppOnly,
        };
        assert_eq!(escalation_policy(metadata), EscalationPolicy::LocalOnly);
    }

    #[test]
    fn restricted_sensitivity_is_local_only_regardless_of_scope() {
        for scope in [
            MemoryScope::Session,
            MemoryScope::Household,
            MemoryScope::Person,
            MemoryScope::Private,
        ] {
            let metadata = MemoryPolicyMetadata {
                scope,
                sensitivity: MemorySensitivity::Restricted,
                spoken_policy: SpokenMemoryPolicy::Deny,
            };
            assert_eq!(
                escalation_policy(metadata),
                EscalationPolicy::LocalOnly,
                "scope {scope:?} with Restricted sensitivity must be LocalOnly"
            );
        }
    }

    #[test]
    fn password_content_is_not_eligible_for_escalation() {
        assert!(!eligible_for_escalation("fact", "my password is swordfish"));
    }

    #[test]
    fn person_linked_normal_memory_is_anonymized_eligible() {
        assert!(eligible_for_escalation(
            "person_preference",
            "Maya likes oat milk"
        ));
    }

    #[test]
    fn health_content_is_not_eligible_for_escalation() {
        assert!(!eligible_for_escalation(
            "person_preference",
            "Grandma takes metformin at 8am for diabetes"
        ));
    }

    #[test]
    fn identity_content_is_not_eligible_for_escalation() {
        assert!(!eligible_for_escalation(
            "identity",
            "my name is Alex Morgan"
        ));
    }

    #[test]
    fn cautious_sensitivity_is_local_only() {
        let metadata = MemoryPolicyMetadata {
            scope: MemoryScope::Household,
            sensitivity: MemorySensitivity::Cautious,
            spoken_policy: SpokenMemoryPolicy::Confirm,
        };
        assert_eq!(escalation_policy(metadata), EscalationPolicy::LocalOnly);
    }

    #[test]
    fn extract_vocab_terms_single_name() {
        let terms = extract_vocab_terms("person_preference", "Maya likes oat milk");
        assert_eq!(terms, vec!["Maya"]);
    }

    #[test]
    fn extract_vocab_terms_multi_word_name() {
        let terms = extract_vocab_terms("identity", "my name is Alex Morgan");
        assert!(
            terms.contains(&"Alex Morgan".to_string()),
            "full name missing"
        );
        assert!(terms.contains(&"Alex".to_string()), "first name missing");
        assert!(
            !terms.iter().any(|t| t == "Morgan"),
            "surname alone should not be added"
        );
    }

    #[test]
    fn extract_vocab_terms_skips_stop_words() {
        let terms = extract_vocab_terms("preference", "User likes jazz music");
        assert!(terms.is_empty(), "stop-word 'User' must not be extracted");
    }

    #[test]
    fn extract_vocab_terms_no_proper_nouns() {
        let terms = extract_vocab_terms("fact", "kitchen light is the ceiling lamp");
        assert!(terms.is_empty(), "all-lowercase content yields no terms");
    }

    #[test]
    fn infers_person_scope_from_kind() {
        let metadata = infer_metadata("person_preference", "Maya likes oat milk");

        assert_eq!(metadata.scope, MemoryScope::Person);
        assert_eq!(metadata.spoken_policy, SpokenMemoryPolicy::Allow);
    }
}
