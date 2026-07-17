//! Conservative shared-room intent gating for voice transcripts.
//!
//! The goal is not to classify every utterance perfectly. It is to reject
//! obvious ambient chatter and low-signal transcripts before they consume
//! LLM/tool budget in wake-word and follow-up flows.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceIntentDecision {
    Accept,
    Reject(&'static str),
}

pub fn assess_transcript(text: &str) -> VoiceIntentDecision {
    let lower = normalize_transcript(text);
    if lower.is_empty() {
        return VoiceIntentDecision::Reject("empty transcript");
    }

    let words = word_count(&lower);

    if is_low_signal_filler(&lower) {
        return VoiceIntentDecision::Reject("low-signal filler");
    }

    if looks_like_direct_request(&lower) {
        return VoiceIntentDecision::Accept;
    }

    if looks_like_ambient_narration(&lower, words) {
        return VoiceIntentDecision::Reject("ambient narration");
    }

    VoiceIntentDecision::Accept
}

fn looks_like_direct_request(text: &str) -> bool {
    text.ends_with('?')
        || starts_with_any(
            text,
            &[
                "what ",
                "what's ",
                "whats ",
                "who ",
                "when ",
                "where ",
                "why ",
                "how ",
                "can you ",
                "could you ",
                "would you ",
                "will you ",
                "please ",
                "turn ",
                "set ",
                "play ",
                "search ",
                "look up ",
                "remember ",
                "forget ",
                "open ",
                "close ",
                "lock ",
                "unlock ",
                "dim ",
                "brighten ",
                "check ",
                "tell me ",
                "show me ",
                "is ",
                "are ",
                "do ",
                "did ",
                "weather ",
                "timer ",
                "remind ",
                "calculate ",
                "call ",
                "text ",
            ],
        )
        || contains_any(
            text,
            &[
                " genie",
                " jarvis",
                " assistant",
                " lights",
                " light ",
                " thermostat",
                " temperature",
                " home assistant",
                " music",
                " tv",
                " volume",
                " alarm",
                " reminder",
                " kitchen",
                " bedroom",
                " living room",
                " garage",
                " front door",
                " weather",
                " time is it",
                " status",
                " search the web",
            ],
        )
}

fn looks_like_ambient_narration(text: &str, words: usize) -> bool {
    words >= 9
        && starts_with_any(
            text,
            &[
                "the ", "a ", "an ", "he ", "she ", "they ", "it ", "we ", "this ", "that ",
            ],
        )
        && !text.ends_with('?')
        && !contains_any(
            text,
            &[
                "please",
                "can you",
                "could you",
                "would you",
                "turn",
                "set",
                "play",
                "search",
                "remember",
                "forget",
                "weather",
                "timer",
                "remind",
                "assistant",
                "genie",
                "jarvis",
            ],
        )
}

fn is_low_signal_filler(text: &str) -> bool {
    matches!(
        text,
        "okay"
            | "ok"
            | "hmm"
            | "uh"
            | "um"
            | "mm"
            | "huh"
            | "right"
            | "yeah"
            | "yep"
            | "nope"
            | "thanks"
            | "thank you"
            | "good night"
            | "goodbye"
    )
}

/// Collapse runs of Unicode whitespace to a single ASCII space, trim leading
/// and trailing whitespace, and ASCII-lowercase — in one pass, one
/// allocation. Replaces the old `split_whitespace().collect::<Vec<_>>().join(" ")`
/// plus a separate `.to_ascii_lowercase()`, which allocated twice (#545):
/// once for the collected `Vec<&str>` joined into a `String`, again for the
/// lowercase copy. Mirrors the `normalize_raw` idiom in `security/injection.rs`.
fn normalize_transcript(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !pending_space {
                out.push(' ');
                pending_space = true;
            }
        } else {
            pending_space = false;
            out.push(ch.to_ascii_lowercase());
        }
    }
    if pending_space {
        out.pop();
    }
    out
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| text.starts_with(prefix))
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_direct_home_command() {
        assert_eq!(
            assess_transcript("turn on the kitchen light"),
            VoiceIntentDecision::Accept
        );
    }

    #[test]
    fn accepts_question() {
        assert_eq!(
            assess_transcript("what time is it?"),
            VoiceIntentDecision::Accept
        );
    }

    #[test]
    fn rejects_low_signal_filler() {
        assert_eq!(
            assess_transcript("thank you"),
            VoiceIntentDecision::Reject("low-signal filler")
        );
    }

    #[test]
    fn rejects_ambient_narration() {
        assert_eq!(
            assess_transcript("the old house stood alone at the end of the road"),
            VoiceIntentDecision::Reject("ambient narration")
        );
    }

    #[test]
    fn does_not_reject_short_status_style_request() {
        assert_eq!(
            assess_transcript("weather in Tokyo"),
            VoiceIntentDecision::Accept
        );
    }

    /// Verbatim copy of the pre-#545 two-allocation normalize path, kept
    /// only as a diff oracle for `normalize_transcript`.
    fn normalize_transcript_oracle(text: &str) -> String {
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        normalized.trim().to_ascii_lowercase()
    }

    /// #545: the single-pass `normalize_transcript` must be byte-identical
    /// to the old split_whitespace+join+to_ascii_lowercase path for every
    /// whitespace/casing shape a real transcript can take.
    #[test]
    fn normalize_transcript_matches_oracle_across_corpus() {
        let corpus = [
            "",
            "   ",
            "\t\n  \t",
            "hello",
            "Hello World",
            "  Hello   World  ",
            "hello\tworld\nagain",
            "turn on the kitchen light",
            "TURN ON THE KITCHEN LIGHT",
            "what time is it?",
            "  what   time    is  it?  ",
            "the old house stood alone at the end of the road",
            "single",
            " single ",
            "MiXeD CaSe TeXt",
            "multiple   internal     spaces    here",
            "\u{a0}non-breaking\u{a0}space\u{a0}padded\u{a0}",
            "trailing punctuation!!  ",
            "\r\nwindows\r\nline\r\nendings\r\n",
        ];

        for text in corpus {
            assert_eq!(
                normalize_transcript(text),
                normalize_transcript_oracle(text),
                "mismatch for input {text:?}"
            );
        }
    }

    /// #545 acceptance: `assess_transcript` decisions must be unchanged
    /// across a representative accept/reject/filler/ambient corpus.
    #[test]
    fn assess_transcript_decisions_unchanged_across_corpus() {
        let cases: &[(&str, VoiceIntentDecision)] = &[
            ("", VoiceIntentDecision::Reject("empty transcript")),
            ("   ", VoiceIntentDecision::Reject("empty transcript")),
            ("thanks", VoiceIntentDecision::Reject("low-signal filler")),
            ("yep", VoiceIntentDecision::Reject("low-signal filler")),
            ("turn off the bedroom light", VoiceIntentDecision::Accept),
            ("TURN OFF THE BEDROOM LIGHT", VoiceIntentDecision::Accept),
            ("  what   time    is  it?  ", VoiceIntentDecision::Accept),
            ("set a timer for ten minutes", VoiceIntentDecision::Accept),
            (
                "play some music in the kitchen",
                VoiceIntentDecision::Accept,
            ),
            (
                "hey genie what's the temperature",
                VoiceIntentDecision::Accept,
            ),
            (
                "he walked into the room and sat down slowly by the window",
                VoiceIntentDecision::Reject("ambient narration"),
            ),
            (
                "she said that the meeting would start soon after lunch",
                VoiceIntentDecision::Reject("ambient narration"),
            ),
            (
                "the weather outside looked calm before the storm arrived",
                VoiceIntentDecision::Accept,
            ),
        ];

        for (text, expected) in cases {
            assert_eq!(assess_transcript(text), *expected, "mismatch for {text:?}");
        }
    }
}
