use genie_core::llm::Message;
use genie_core::prompt::ModelFamily;
use genie_core::reasoning::{InteractionKind, ReasoningDecision, apply_reasoning_mode};

/// Verbatim copy of the `apply_reasoning_mode` pipeline from `main` @ 0d8904e,
/// before the shared-lowercase / early-out rework. The differential test below
/// asserts the optimized implementation produces byte-identical adjusted
/// messages and identical `ReasoningDecision`s across the corpus.
mod reference {
    use super::*;
    use genie_core::reasoning::ReasoningMode;

    pub fn apply_reasoning_mode(
        model_family: ModelFamily,
        messages: &[Message],
        user_text: &str,
        interaction: InteractionKind,
    ) -> (Vec<Message>, ReasoningDecision) {
        if !supports_reasoning_toggle(model_family) {
            return (
                messages.to_vec(),
                ReasoningDecision {
                    mode: ReasoningMode::Normal,
                    explicit: false,
                    applied: false,
                },
            );
        }

        let explicit_mode = explicit_reasoning_mode(user_text);
        let mode = explicit_mode.unwrap_or_else(|| auto_reasoning_mode(user_text, interaction));
        let explicit = explicit_mode.is_some();
        let cleaned_user_text = strip_reasoning_directives(user_text);

        let Some(last_user_idx) = messages.iter().rposition(|m| m.role == "user") else {
            return (
                messages.to_vec(),
                ReasoningDecision {
                    mode,
                    explicit,
                    applied: false,
                },
            );
        };

        let mut adjusted = messages.to_vec();
        let base = if cleaned_user_text.trim().is_empty() {
            adjusted[last_user_idx].content.trim().to_string()
        } else {
            cleaned_user_text.trim().to_string()
        };

        adjusted[last_user_idx].content = match mode {
            ReasoningMode::Normal => {
                if base.is_empty() {
                    "/no_think".into()
                } else {
                    format!("{base}\n/no_think")
                }
            }
            ReasoningMode::Deep => {
                if base.is_empty() {
                    "/think".into()
                } else {
                    format!("{base}\n/think")
                }
            }
        };

        (
            adjusted,
            ReasoningDecision {
                mode,
                explicit,
                applied: true,
            },
        )
    }

    fn supports_reasoning_toggle(model_family: ModelFamily) -> bool {
        matches!(model_family, ModelFamily::Qwen)
    }

    fn explicit_reasoning_mode(user_text: &str) -> Option<ReasoningMode> {
        let lower = user_text.to_lowercase();
        if lower.contains("/no_think") {
            Some(ReasoningMode::Normal)
        } else if lower.contains("/think")
            || lower.contains("think deeply")
            || lower.contains("reason carefully")
            || lower.contains("step by step")
        {
            Some(ReasoningMode::Deep)
        } else {
            None
        }
    }

    fn auto_reasoning_mode(user_text: &str, interaction: InteractionKind) -> ReasoningMode {
        if matches!(interaction, InteractionKind::ToolSummary) {
            return ReasoningMode::Normal;
        }

        if is_simple_request(user_text) {
            return ReasoningMode::Normal;
        }

        if looks_like_deep_reasoning_request(user_text) {
            return ReasoningMode::Deep;
        }

        let _ = interaction;
        ReasoningMode::Normal
    }

    fn is_simple_request(user_text: &str) -> bool {
        let lower = user_text.to_lowercase();
        let words = lower.split_whitespace().count();

        words <= 10
            && (lower.contains("what time")
                || lower.contains("weather")
                || lower.starts_with("hi")
                || lower.starts_with("hello")
                || lower.starts_with("hey")
                || lower.contains("turn on")
                || lower.contains("turn off")
                || lower.starts_with("set ")
                || lower.contains("remember")
                || lower.contains("my name")
                || lower.contains("what's up")
                || lower.contains("whats up"))
    }

    fn looks_like_deep_reasoning_request(user_text: &str) -> bool {
        let lower = user_text.to_lowercase();
        let complex_markers = [
            "analy",
            "compare",
            "tradeoff",
            "trade-off",
            "architecture",
            "design",
            "plan",
            "debug",
            "review",
            "refactor",
            "prove",
            "derive",
            "why does",
            "what is wrong",
            "what's wrong",
            "optimiz",
            "algorithm",
            "complexity",
            "step by step",
            "pros and cons",
            "should we",
            "write code",
            "rust",
            "explain in detail",
        ];

        lower.len() > 140
            || lower.contains('\n')
            || lower.contains("1.")
            || lower.contains("2.")
            || lower.contains("```")
            || complex_markers.iter().any(|marker| lower.contains(marker))
    }

    fn strip_reasoning_directives(user_text: &str) -> String {
        user_text.replace("/no_think", "").replace("/think", "")
    }
}

const CORPUS: &[&str] = &[
    // Simple / greeting / home-control (all-lowercase borrow fast path).
    "hi there",
    "hello genie",
    "hey",
    "what time is it",
    "turn on the kitchen lights",
    "turn off the porch light please",
    "set a timer for 10 minutes",
    "remember that maya likes oat milk",
    "what's up",
    "whats up with the thermostat",
    // Mixed case (must take the allocating lowercase path, same decisions).
    "What Time Is It",
    "TURN ON the Kitchen Lights",
    "What's the WEATHER like today?",
    "Remember my name is Jared",
    // Explicit directives, including case variants the original code only
    // strips when the directive is exactly lowercase.
    "/think",
    "/no_think",
    "debug this crash /think",
    "/no_think just answer quickly",
    "please /THINK about this one",
    "use /No_Think for this reply",
    "what is 1/2 plus 1/4",
    "think deeply about the garden layout",
    "reason carefully before answering",
    "walk me through it step by step",
    // Deep-reasoning auto-escalation markers.
    "compare the two thermostats and recommend one",
    "why does the living room sensor keep dropping offline",
    "review the ARCHITECTURE of the heating schedule",
    "help me refactor this rust function",
    "what's wrong with my automation",
    "1. check the sensor 2. restart the hub",
    "here is the log\nplease analyze it",
    "```fn main() {}```",
    // Long (> 140 chars) prompt.
    "I want a detailed comparison of running the media server on the jetson \
     versus the nas, including power draw, thermals, and which one is easier \
     to keep updated over time.",
    // Awkward inputs.
    "",
    "   ",
    "\t\n",
    "ok",
    "café au lait at noon",
    "wie ist das Wetter draußen",
    "Καλημέρα τι κάνεις σήμερα",
    "☀️ turn on the lights ☀️",
];

const INTERACTIONS: &[InteractionKind] = &[
    InteractionKind::Chat,
    InteractionKind::Voice,
    InteractionKind::Repl,
    InteractionKind::OpenAiBridge,
    InteractionKind::ToolSummary,
];

const FAMILIES: &[ModelFamily] = &[ModelFamily::Qwen, ModelFamily::Phi, ModelFamily::Gemma];

fn msg(role: &str, content: &str) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
    }
}

fn assert_equivalent(
    family: ModelFamily,
    messages: &[Message],
    user_text: &str,
    interaction: InteractionKind,
) {
    let (expected_msgs, expected_decision) =
        reference::apply_reasoning_mode(family, messages, user_text, interaction);
    let (actual_msgs, actual_decision) =
        apply_reasoning_mode(family, messages, user_text, interaction);

    assert_eq!(
        actual_decision, expected_decision,
        "decision drift for family={family:?} interaction={interaction:?} text={user_text:?}"
    );
    assert_eq!(
        actual_msgs.len(),
        expected_msgs.len(),
        "message count drift for text={user_text:?}"
    );
    for (actual, expected) in actual_msgs.iter().zip(expected_msgs.iter()) {
        assert_eq!(
            actual.role, expected.role,
            "role drift for text={user_text:?}"
        );
        assert_eq!(
            actual.content, expected.content,
            "content drift for family={family:?} interaction={interaction:?} text={user_text:?}"
        );
    }
}

/// Differential regression: the shared-lowercase + early-out rework must keep
/// `ReasoningDecision` and the adjusted message content byte-identical to the
/// previous implementation for every corpus entry, interaction kind, and
/// model family.
#[test]
fn reasoning_corpus_matches_reference_implementation() {
    for family in FAMILIES {
        for interaction in INTERACTIONS {
            for text in CORPUS {
                let messages = vec![msg("user", text)];
                assert_equivalent(*family, &messages, text, *interaction);
            }
        }
    }
}

/// Multi-turn shapes: only the last user message is adjusted, and histories
/// with no user message at all keep the `applied=false` path.
#[test]
fn reasoning_corpus_multi_turn_shapes_match_reference() {
    let histories: &[Vec<Message>] = &[
        vec![
            msg("system", "You are Genie."),
            msg("user", "turn on the kitchen lights"),
            msg("assistant", "Done."),
            msg("user", "now compare zigbee and zwave for the door sensors"),
        ],
        vec![msg("system", "You are Genie."), msg("assistant", "Hello!")],
        vec![msg("user", "")],
        vec![
            msg("user", "first question"),
            msg("assistant", "answer"),
            msg("user", "/think about the heating plan"),
        ],
    ];

    for history in histories {
        let user_text = history
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        for interaction in INTERACTIONS {
            assert_equivalent(ModelFamily::Qwen, history, &user_text, *interaction);
        }
    }
}
