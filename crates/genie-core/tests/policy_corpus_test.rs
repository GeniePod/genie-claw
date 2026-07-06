use genie_core::memory::policy::{
    MemoryDisclosure, MemoryDisclosureClass, MemoryScope, MemorySensitivity, SpokenMemoryPolicy,
    assess_memory_write, infer_metadata,
};

fn write_key(kind: &str, content: &str) -> String {
    let decision = assess_memory_write(kind, content);
    let metadata = infer_metadata(kind, content);
    format!(
        "allowed={};disclosure={:?};class={:?};scope={:?};sensitivity={:?};spoken={:?};reason={}",
        decision.allowed,
        decision.disclosure,
        decision.class,
        metadata.scope,
        metadata.sensitivity,
        metadata.spoken_policy,
        decision.reason,
    )
}

/// Fixed corpus captured from `main` @ ecd7592 — guards byte-identical policy
/// decisions after assess_memory_write early-outs (#497) and deferred content
/// `to_lowercase` on benign auto-capture writes.
#[test]
fn memory_policy_corpus_regression() {
    const CORPUS: &[(&str, &str, &str)] = &[
        (
            "preference",
            "User likes jazz music",
            "allowed=true;disclosure=Speak;class=Household;scope=Household;sensitivity=Normal;spoken=Allow;reason=Memory is safe for household-shared storage.",
        ),
        (
            "fact",
            "my password is swordfish",
            "allowed=false;disclosure=Deny;class=Restricted;scope=Household;sensitivity=Restricted;spoken=Deny;reason=I should not store passwords, tokens, keys, or one-time codes as voice memory.",
        ),
        (
            "fact",
            "the gate code is 5829",
            "allowed=false;disclosure=Deny;class=Restricted;scope=Household;sensitivity=Restricted;spoken=Deny;reason=I should not store household access codes or lock combinations as voice memory.",
        ),
        (
            "fact",
            "the passports are in the safe",
            "allowed=false;disclosure=Deny;class=Restricted;scope=Household;sensitivity=Restricted;spoken=Deny;reason=I should not store sensitive document, key, or safe locations as voice memory.",
        ),
        (
            "fact",
            "User has a recent medical diagnosis of mild asthma",
            "allowed=true;disclosure=Speak;class=Sensitive;scope=Household;sensitivity=Cautious;spoken=Confirm;reason=Memory is safe for household-shared storage.",
        ),
        (
            "person_preference",
            "Maya likes oat milk",
            "allowed=true;disclosure=Speak;class=Person;scope=Person;sensitivity=Normal;spoken=Allow;reason=Memory is safe for household-shared storage.",
        ),
        (
            "private_note",
            "remember this privately that I owe Sam twenty dollars",
            "allowed=false;disclosure=AppOnly;class=Private;scope=Private;sensitivity=Cautious;spoken=AppOnly;reason=Private personal memory requires an explicit app-backed flow in V1.",
        ),
        (
            "fact",
            "kitchen light is the ceiling lamp",
            "allowed=true;disclosure=Speak;class=Household;scope=Household;sensitivity=Normal;spoken=Allow;reason=Memory is safe for household-shared storage.",
        ),
        (
            "preference",
            "User LIKES hiking in the mountains",
            "allowed=true;disclosure=Speak;class=Household;scope=Household;sensitivity=Normal;spoken=Allow;reason=Memory is safe for household-shared storage.",
        ),
        (
            "identity",
            "User's name is Jared",
            "allowed=true;disclosure=Speak;class=Household;scope=Household;sensitivity=Normal;spoken=Allow;reason=Memory is safe for household-shared storage.",
        ),
    ];

    for (kind, content, expected) in CORPUS {
        assert_eq!(
            write_key(kind, content),
            *expected,
            "corpus mismatch for kind={kind:?} content={content:?}"
        );
    }
}

#[test]
fn metadata_round_trip_matches_decision_class() {
    let metadata = infer_metadata("preference", "User likes jazz music");
    assert_eq!(metadata.scope, MemoryScope::Household);
    assert_eq!(metadata.sensitivity, MemorySensitivity::Normal);
    assert_eq!(metadata.spoken_policy, SpokenMemoryPolicy::Allow);

    let decision = assess_memory_write("preference", "User likes jazz music");
    assert!(decision.allowed);
    assert_eq!(decision.disclosure, MemoryDisclosure::Speak);
    assert_eq!(decision.class, MemoryDisclosureClass::Household);
}
