use crate::llm::Message;
use crate::prompt::ModelFamily;

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

    let lower = user_text.to_lowercase();
    let explicit_mode = explicit_reasoning_mode(&lower);
    let mode = explicit_mode.unwrap_or_else(|| auto_reasoning_mode(&lower, user_text, interaction));
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

fn explicit_reasoning_mode(lower: &str) -> Option<ReasoningMode> {
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

fn auto_reasoning_mode(
    lower: &str,
    user_text: &str,
    interaction: InteractionKind,
) -> ReasoningMode {
    if matches!(interaction, InteractionKind::ToolSummary) {
        return ReasoningMode::Normal;
    }

    if is_simple_request(lower, user_text) {
        return ReasoningMode::Normal;
    }

    if looks_like_deep_reasoning_request(lower, user_text) {
        return ReasoningMode::Deep;
    }

    let _ = interaction;
    ReasoningMode::Normal
}

fn is_simple_request(lower: &str, user_text: &str) -> bool {
    let words = lower.split_whitespace().count();
    if words > 10 {
        return false;
    }
    if !needs_simple_request_scan(user_text) {
        return false;
    }

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

const DEEP_REASONING_MARKERS: &[&str] = &[
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

fn looks_like_deep_reasoning_request(lower: &str, user_text: &str) -> bool {
    if lower.len() > 140
        || lower.contains('\n')
        || lower.contains("1.")
        || lower.contains("2.")
        || lower.contains("```")
    {
        return true;
    }
    if !needs_deep_reasoning_scan(user_text) {
        return false;
    }

    DEEP_REASONING_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Conservative gate before the simple-request marker checks on the lowered view.
fn needs_simple_request_scan(text: &str) -> bool {
    const CONTAINS: &[&str] = &[
        "what time",
        "weather",
        "turn on",
        "turn off",
        "remember",
        "my name",
        "what's up",
        "whats up",
    ];
    if CONTAINS
        .iter()
        .any(|marker| contains_ascii_ci(text, marker))
    {
        return true;
    }

    let trimmed = text.trim_start();
    const PREFIXES: &[&str] = &["hi", "hello", "hey", "set "];
    PREFIXES.iter().any(|prefix| {
        trimmed.len() >= prefix.len()
            && trimmed
                .as_bytes()
                .get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    })
}

/// Conservative gate before the deep-reasoning marker loop on the lowered view.
fn needs_deep_reasoning_scan(text: &str) -> bool {
    DEEP_REASONING_MARKERS
        .iter()
        .any(|marker| contains_ascii_ci(text, marker))
}

fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.as_bytes().windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle.bytes())
            .all(|(left, right)| left.eq_ignore_ascii_case(&right))
    })
}

fn strip_reasoning_directives(user_text: &str) -> String {
    user_text.replace("/no_think", "").replace("/think", "")
}
