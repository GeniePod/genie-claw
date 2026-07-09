use crate::llm::Message;
use crate::prompt::ModelFamily;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionKind {
    Chat,
    Voice,
    Repl,
    OpenAiBridge,
    ToolSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningMode {
    Normal,
    Deep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningDecision {
    pub mode: ReasoningMode,
    pub explicit: bool,
    pub applied: bool,
}

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

    // One shared lowercase view for every marker scan below.
    // `explicit_reasoning_mode`, `is_simple_request`, and
    // `looks_like_deep_reasoning_request` previously each built their own
    // `to_lowercase()` copy of the same user text (up to three full-string
    // allocations per utterance). ASCII text with no uppercase letters — the
    // common voice-transcript case — borrows `user_text` without allocating.
    let lower: Cow<'_, str> = if is_already_ascii_lowercase(user_text) {
        Cow::Borrowed(user_text)
    } else {
        Cow::Owned(user_text.to_lowercase())
    };

    let explicit_mode = explicit_reasoning_mode(&lower);
    let mode = explicit_mode.unwrap_or_else(|| auto_reasoning_mode(&lower, interaction));
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

/// True when Unicode lowercasing is the identity for `text`, so the marker
/// scans can borrow it instead of allocating. Conservative: any non-ASCII
/// byte falls back to the allocating `to_lowercase` path.
fn is_already_ascii_lowercase(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii() && !b.is_ascii_uppercase())
}

/// `lower` must be the lowercased user text.
fn explicit_reasoning_mode(lower: &str) -> Option<ReasoningMode> {
    // Both slash directives contain '/': one byte scan gates both substring
    // searches for the common directive-free utterance.
    if lower.contains('/') {
        if lower.contains("/no_think") {
            return Some(ReasoningMode::Normal);
        }
        if lower.contains("/think") {
            return Some(ReasoningMode::Deep);
        }
    }
    if lower.contains("think deeply")
        || lower.contains("reason carefully")
        || lower.contains("step by step")
    {
        return Some(ReasoningMode::Deep);
    }
    None
}

/// `lower` must be the lowercased user text.
fn auto_reasoning_mode(lower: &str, interaction: InteractionKind) -> ReasoningMode {
    if matches!(interaction, InteractionKind::ToolSummary) {
        return ReasoningMode::Normal;
    }

    if is_simple_request(lower) {
        return ReasoningMode::Normal;
    }

    if looks_like_deep_reasoning_request(lower) {
        return ReasoningMode::Deep;
    }

    let _ = interaction;
    ReasoningMode::Normal
}

/// `lower` must be the lowercased user text.
fn is_simple_request(lower: &str) -> bool {
    // The decision only needs "10 words or fewer"; stop counting at 11 so
    // long prompts don't pay for a full-string whitespace walk.
    let words = lower.split_whitespace().take(11).count();

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

/// `lower` must be the lowercased user text.
fn looks_like_deep_reasoning_request(lower: &str) -> bool {
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

fn strip_reasoning_directives(user_text: &str) -> Cow<'_, str> {
    // Both directives contain '/'; without one neither `replace` can match,
    // so the directive-free common case borrows instead of copying twice.
    if !user_text.contains('/') {
        return Cow::Borrowed(user_text);
    }
    Cow::Owned(user_text.replace("/no_think", "").replace("/think", ""))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
