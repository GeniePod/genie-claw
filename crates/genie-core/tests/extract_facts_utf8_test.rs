//! Regression tests: `extract_facts` must never panic on non-ASCII input.
//!
//! The "remember" fast-path used to guard a `trimmed[..8]` byte slice with only a
//! `trimmed.len() >= 8` byte-length check, so any utterance whose 8th byte fell
//! inside a multi-byte UTF-8 char panicked with "byte index 8 is not a char
//! boundary". `extract_facts` runs on raw chat/voice/document text, so this took
//! down fact extraction for any non-Latin-script household.

use genie_core::memory::extract::extract_facts;

/// Non-ASCII utterances where byte 8 lands inside a multi-byte char must be
/// parsed without panicking (and simply not match the "remember" prefix).
#[test]
fn non_ascii_input_does_not_panic() {
    // Byte 8 is inside the 3rd CJK char (each is 3 bytes: 0..3, 3..6, 6..9).
    let _ = extract_facts("日本語hello");
    // 2-byte accented chars: byte 8 is inside the 5th 'é'.
    let _ = extract_facts("éééééhello");
    // Emoji (4 bytes) straddling the boundary.
    let _ = extract_facts("👍👍hello world");
    // Leading whitespace then non-ASCII (exercises the trimmed path).
    let _ = extract_facts("   café con leche por favor");
}

/// Inputs shorter than the "remember" prefix must not panic either.
#[test]
fn short_input_does_not_panic() {
    for s in ["", "r", "remem", "café"] {
        let facts = extract_facts(s);
        assert!(
            !facts.iter().any(|f| f.category == "fact"),
            "{s:?} should not produce a remember fact"
        );
    }
}

/// The ASCII "remember" path still works unchanged after the fix.
#[test]
fn remember_request_still_extracted() {
    let facts = extract_facts("remember to call mom");
    let fact = facts
        .iter()
        .find(|f| f.category == "fact")
        .expect("an explicit remember request yields a fact");
    assert!(
        fact.content.contains("call mom"),
        "unexpected content: {:?}",
        fact.content
    );

    // Case-insensitive prefix and the "that" connective still hold.
    let facts = extract_facts("Remember that the spare key is under the mat");
    let fact = facts
        .iter()
        .find(|f| f.category == "fact")
        .expect("capitalized remember request yields a fact");
    assert!(fact.content.contains("spare key is under the mat"));
}

/// A remember request whose payload is non-ASCII is extracted, not dropped or
/// panicked on (the ASCII prefix is a valid char boundary, so the tail is safe).
#[test]
fn remember_with_non_ascii_payload() {
    let facts = extract_facts("remember that Renée's café opens at nine");
    let fact = facts
        .iter()
        .find(|f| f.category == "fact")
        .expect("remember request with accented payload yields a fact");
    assert!(fact.content.contains("Renée's café"));
}
