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

    // Lowercase the user text once and share it across every classifier below.
    // On `main`, explicit_reasoning_mode / is_simple_request /
    // looks_like_deep_reasoning_request each rebuilt `user_text.to_lowercase()`
    // independently — up to three full-string allocations per Qwen utterance on
    // the pre-LLM hot path. All three only *read* the lowered form
    // (contains / starts_with / split_whitespace / len), so one shared buffer is
    // byte-identical. `strip_reasoning_directives` still runs on the original
    // `user_text`: its replace is case-sensitive, and preserving that exact
    // (detect-lowercased, strip-cased) behavior matters.
    let lower = user_text.to_lowercase();

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

fn is_simple_request(lower: &str) -> bool {
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

fn strip_reasoning_directives(user_text: &str) -> String {
    user_text.replace("/no_think", "").replace("/think", "")
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

    /// A representative spread of utterances: simple/greeting, explicit
    /// /think + /no_think, auto-deep (markers, long, multiline, numbered,
    /// code-fence), and mixed / non-ASCII casing that exercises the lowercase
    /// fold. Used by the equivalence and bench tests below.
    fn corpus() -> Vec<&'static str> {
        vec![
            "hi there",
            "Hello",
            "what time is it",
            "weather today",
            "turn off the kitchen lights",
            "Set a timer for five minutes",
            "what's up",
            "do you remember my name",
            "debug this crash /think",
            "/no_think just tell me the time",
            "Think Deeply about this before answering",
            "reason carefully and Step By Step",
            "What is the best ARCHITECTURE for a home controller",
            "please Analyze the tradeoffs between these two designs",
            "compare the PROS AND CONS and recommend a refactor",
            "café owner wants to OPTIMIZE the espresso queue",
            "first do this\nthen do that",
            "here is a list 1. wake up 2. sleep",
            "look at this ```rust fn main(){} ```",
            "i went to the store earlier and picked up milk and bread and eggs and \
             also some cheese and then i drove home and put everything away in the \
             fridge before dinner",
            "",
        ]
    }

    // Verbatim pre-refactor classifiers (each rebuilds `to_lowercase()` per call).
    // They are the oracle the shared-lowercase path must match bit-for-bit,
    // mirroring the `word_overlap_reference` pattern in `memory/mod.rs`.
    fn explicit_reasoning_mode_ref(user_text: &str) -> Option<ReasoningMode> {
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

    fn is_simple_request_ref(user_text: &str) -> bool {
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

    fn looks_like_deep_reasoning_request_ref(user_text: &str) -> bool {
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

    #[test]
    fn shared_lowercase_matches_per_call_oracle() {
        // The optimization only changes *where* the lowercase happens (once,
        // shared) vs the pre-refactor per-call lowercasing. Each classifier must
        // stay bit-for-bit identical across the corpus.
        for text in corpus() {
            let lower = text.to_lowercase();
            assert_eq!(
                explicit_reasoning_mode(&lower),
                explicit_reasoning_mode_ref(text),
                "explicit mismatch for {text:?}"
            );
            assert_eq!(
                is_simple_request(&lower),
                is_simple_request_ref(text),
                "is_simple mismatch for {text:?}"
            );
            assert_eq!(
                looks_like_deep_reasoning_request(&lower),
                looks_like_deep_reasoning_request_ref(text),
                "deep mismatch for {text:?}"
            );
        }
    }

    #[test]
    fn shared_lowercase_preserves_end_to_end_decision() {
        // End-to-end: the full apply_reasoning_mode decision + adjusted content
        // must match a decision recomputed through the per-call oracle for every
        // corpus utterance (Qwen, Chat).
        for text in corpus() {
            let (messages, decision) = apply_reasoning_mode(
                ModelFamily::Qwen,
                &single_user_message(text),
                text,
                InteractionKind::Chat,
            );

            let explicit_mode = explicit_reasoning_mode_ref(text);
            let expected_mode = explicit_mode.unwrap_or_else(|| {
                if is_simple_request_ref(text) {
                    ReasoningMode::Normal
                } else if looks_like_deep_reasoning_request_ref(text) {
                    ReasoningMode::Deep
                } else {
                    ReasoningMode::Normal
                }
            });

            assert_eq!(decision.mode, expected_mode, "mode mismatch for {text:?}");
            assert_eq!(
                decision.explicit,
                explicit_mode.is_some(),
                "explicit flag mismatch for {text:?}"
            );
            assert!(decision.applied, "should apply for Qwen: {text:?}");
            let suffix = match expected_mode {
                ReasoningMode::Normal => "/no_think",
                ReasoningMode::Deep => "/think",
            };
            assert!(
                messages[0].content.ends_with(suffix),
                "content {:?} should end with {suffix} for {text:?}",
                messages[0].content
            );
        }
    }

    // Reproducible before→after microbench. Ignored by default (timing, not a
    // correctness gate); run with:
    //   cargo test -p genie-core --release reasoning::tests::bench_lowercase_dedup -- --ignored --nocapture
    #[test]
    #[ignore = "microbench; run with --release --ignored --nocapture"]
    fn bench_lowercase_dedup() {
        use std::time::Instant;
        let corpus = corpus();
        let iters = 200_000u32;

        // Before: three independent to_lowercase() per utterance (main).
        let start = Instant::now();
        let mut acc = 0u64;
        for _ in 0..iters {
            for text in &corpus {
                acc += u64::from(explicit_reasoning_mode_ref(text).is_some());
                acc += u64::from(is_simple_request_ref(text));
                acc += u64::from(looks_like_deep_reasoning_request_ref(text));
            }
        }
        let before = start.elapsed();

        // After: one shared to_lowercase() per utterance, reused by all three.
        let start = Instant::now();
        for _ in 0..iters {
            for text in &corpus {
                let lower = text.to_lowercase();
                acc += u64::from(explicit_reasoning_mode(&lower).is_some());
                acc += u64::from(is_simple_request(&lower));
                acc += u64::from(looks_like_deep_reasoning_request(&lower));
            }
        }
        let after = start.elapsed();

        eprintln!(
            "reasoning classifiers over {} utterances x {iters} iters:\n  \
             before (3x lowercase): {before:?}\n  after  (1x lowercase): {after:?}\n  \
             speedup: {:.2}x  (acc={acc})",
            corpus.len(),
            before.as_secs_f64() / after.as_secs_f64(),
        );
        assert!(
            after <= before.mul_f64(1.5),
            "shared path unexpectedly slower"
        );
    }
}
