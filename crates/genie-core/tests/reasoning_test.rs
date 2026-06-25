use genie_core::llm::Message;
use genie_core::prompt::ModelFamily;
use genie_core::reasoning::{InteractionKind, ReasoningMode, apply_reasoning_mode};

fn single_user_message(text: &str) -> Vec<Message> {
    vec![Message {
        role: "user".into(),
        content: text.into(),
    }]
}

#[test]
fn qwen_defaults_to_no_think() {
    let (messages, decision) = apply_reasoning_mode(
        ModelFamily::Qwen,
        &single_user_message("hi there"),
        "hi there",
        InteractionKind::Chat,
    );

    assert!(decision.applied);
    assert_eq!(decision.mode, ReasoningMode::Normal);
    assert!(messages[0].content.ends_with("/no_think"));
}

#[test]
fn explicit_think_overrides_default() {
    let (messages, decision) = apply_reasoning_mode(
        ModelFamily::Qwen,
        &single_user_message("debug this crash /think"),
        "debug this crash /think",
        InteractionKind::Chat,
    );

    assert!(decision.explicit);
    assert_eq!(decision.mode, ReasoningMode::Deep);
    assert!(messages[0].content.ends_with("/think"));
    assert!(!messages[0].content.contains("/no_think"));
}

#[test]
fn complex_prompt_escalates_to_think() {
    let text = "Compare these two Rust designs, explain the tradeoffs, and recommend the safer refactor step by step.";
    let (messages, decision) = apply_reasoning_mode(
        ModelFamily::Qwen,
        &single_user_message(text),
        text,
        InteractionKind::Chat,
    );

    assert_eq!(decision.mode, ReasoningMode::Deep);
    assert!(messages[0].content.ends_with("/think"));
}

#[test]
fn phi_family_is_unchanged() {
    let original = single_user_message("hello");
    let (messages, decision) =
        apply_reasoning_mode(ModelFamily::Phi, &original, "hello", InteractionKind::Chat);

    assert_eq!(messages[0].content, "hello");
    assert!(!decision.applied);
}

#[test]
fn gemma_family_is_unchanged() {
    let original = single_user_message("what time is it");
    let (messages, decision) = apply_reasoning_mode(
        ModelFamily::Gemma,
        &original,
        "what time is it",
        InteractionKind::Chat,
    );

    assert_eq!(messages[0].content, "what time is it");
    assert!(!decision.applied);
}

fn decision_key(
    messages: &[Message],
    mode: ReasoningMode,
    explicit: bool,
    applied: bool,
) -> String {
    format!(
        "mode={mode:?};explicit={explicit};applied={applied};content={}",
        messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("")
    )
}

/// Fixed corpus captured from `main` @ 1e01954.
#[test]
fn reasoning_mode_corpus_regression() {
    const CORPUS: &[(&str, InteractionKind, &str)] = &[
        (
            "hi there",
            InteractionKind::Chat,
            "mode=Normal;explicit=false;applied=true;content=hi there\n/no_think",
        ),
        (
            "what time is it",
            InteractionKind::Voice,
            "mode=Normal;explicit=false;applied=true;content=what time is it\n/no_think",
        ),
        (
            "debug this crash /think",
            InteractionKind::Chat,
            "mode=Deep;explicit=true;applied=true;content=debug this crash\n/think",
        ),
        (
            "Compare these two Rust designs, explain the tradeoffs, and recommend the safer refactor step by step.",
            InteractionKind::Chat,
            "mode=Deep;explicit=true;applied=true;content=Compare these two Rust designs, explain the tradeoffs, and recommend the safer refactor step by step.\n/think",
        ),
        (
            "turn on the living room lights",
            InteractionKind::Voice,
            "mode=Normal;explicit=false;applied=true;content=turn on the living room lights\n/no_think",
        ),
        // Explicit deep-reasoning directives must each escalate to /think — guards
        // the four keywords behind explicit_reasoning_mode (matedev01 review on #502).
        (
            "tell me about the moon, think deeply",
            InteractionKind::Chat,
            "mode=Deep;explicit=true;applied=true;content=tell me about the moon, think deeply\n/think",
        ),
        (
            "is this safe, reason carefully",
            InteractionKind::Voice,
            "mode=Deep;explicit=true;applied=true;content=is this safe, reason carefully\n/think",
        ),
    ];

    for (text, interaction, expected) in CORPUS {
        let (messages, decision) = apply_reasoning_mode(
            ModelFamily::Qwen,
            &single_user_message(text),
            text,
            *interaction,
        );
        assert_eq!(
            decision_key(
                &messages,
                decision.mode,
                decision.explicit,
                decision.applied
            ),
            *expected,
            "corpus mismatch for {text:?}"
        );
    }
}
