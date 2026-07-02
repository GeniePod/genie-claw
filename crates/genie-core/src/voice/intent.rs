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

/// Collapse ASCII whitespace and lowercase in one pass (no `Vec` + `join`).
fn normalize_transcript(text: &str) -> String {
    let trimmed = text.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut pending_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !pending_space {
                pending_space = true;
            }
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

const DIRECT_REQUEST_PREFIXES: &[&str] = &[
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
];

const DIRECT_REQUEST_CONTAINS: &[&str] = &[
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
];

fn looks_like_direct_request(text: &str) -> bool {
    if !might_be_direct_request(text) {
        return false;
    }

    text.ends_with('?')
        || starts_with_any(text, DIRECT_REQUEST_PREFIXES)
        || contains_any(text, DIRECT_REQUEST_CONTAINS)
}

/// Conservative gate before the prefix / `contains_any` checks in
/// [`looks_like_direct_request`].
///
/// Derived from [`DIRECT_REQUEST_PREFIXES`] and [`DIRECT_REQUEST_CONTAINS`] so
/// the gate cannot drift from the guarded lists. Each prefix/needle core is
/// scanned with `contains`, which is a superset of `starts_with` / exact
/// `contains` on the padded forms below.
fn might_be_direct_request(text: &str) -> bool {
    if text.ends_with('?') {
        return true;
    }

    DIRECT_REQUEST_PREFIXES
        .iter()
        .any(|prefix| text.contains(prefix.trim_end()))
        || DIRECT_REQUEST_CONTAINS
            .iter()
            .any(|needle| text.contains(needle.trim()))
}

fn looks_like_ambient_narration(text: &str, words: usize) -> bool {
    if words < 9 {
        return false;
    }

    if !starts_with_any(
        text,
        &[
            "the ", "a ", "an ", "he ", "she ", "they ", "it ", "we ", "this ", "that ",
        ],
    ) {
        return false;
    }

    if text.ends_with('?') {
        return false;
    }

    !might_be_ambient_command(text)
}

/// Command markers that prevent an ambient-narration rejection. Shared by
/// [`looks_like_ambient_narration`] and [`might_be_ambient_command`] so the
/// early-out gate cannot drift from the guarded `contains_any` list.
const AMBIENT_COMMAND_MARKERS: &[&str] = &[
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
];

fn might_be_ambient_command(text: &str) -> bool {
    contains_any(text, AMBIENT_COMMAND_MARKERS)
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

    #[test]
    fn normalize_transcript_collapses_whitespace() {
        assert_eq!(
            normalize_transcript("  turn   on   the   light  "),
            "turn on the light"
        );
    }

    /// Every guarded prefix/needle must pass the early-out gate; otherwise a new
    /// marker added to [`looks_like_direct_request`] could be silently skipped.
    #[test]
    fn direct_request_gate_covers_guarded_markers() {
        for prefix in DIRECT_REQUEST_PREFIXES {
            let sample = format!("{prefix}the kitchen light");
            assert!(
                might_be_direct_request(&sample),
                "gate must not block prefix {prefix:?}"
            );
            assert!(
                looks_like_direct_request(&sample),
                "prefix check failed for {prefix:?}"
            );
        }

        for needle in DIRECT_REQUEST_CONTAINS {
            let sample = format!("x{needle}y");
            assert!(
                might_be_direct_request(&sample),
                "gate must not block needle {needle:?}"
            );
            assert!(
                looks_like_direct_request(&sample),
                "contains check failed for {needle:?}"
            );
        }

        assert!(might_be_direct_request("anything?"));
        assert!(looks_like_direct_request("anything?"));
    }

    /// Ambient narration must not reject transcripts that carry a command marker.
    #[test]
    fn ambient_command_gate_covers_guarded_markers() {
        for marker in AMBIENT_COMMAND_MARKERS {
            let sample = format!("the old house {marker} stood alone at the end of the road");
            assert!(
                might_be_ambient_command(&sample),
                "gate must not miss ambient command marker {marker:?}"
            );
            assert!(
                !looks_like_ambient_narration(&sample, word_count(&sample)),
                "ambient narration must not reject command marker {marker:?}"
            );
        }
    }
}
