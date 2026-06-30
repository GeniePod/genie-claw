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

fn looks_like_direct_request(text: &str) -> bool {
    if !might_be_direct_request(text) {
        return false;
    }

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

/// Conservative superset of markers checked by [`looks_like_direct_request`].
fn might_be_direct_request(text: &str) -> bool {
    if text.ends_with('?') {
        return true;
    }
    const MARKERS: &[&str] = &[
        "what",
        "who",
        "when",
        "where",
        "why",
        "how",
        "can you",
        "could you",
        "would you",
        "will you",
        "please",
        "turn ",
        "set ",
        "play ",
        "search",
        "look up",
        "remember",
        "forget",
        "open ",
        "close ",
        "lock ",
        "unlock",
        "dim ",
        "brighten",
        "check ",
        "tell me",
        "show me",
        "is ",
        "are ",
        "do ",
        "did ",
        "weather",
        "timer",
        "remind",
        "calculate",
        "call ",
        "text ",
        "genie",
        "jarvis",
        "assistant",
        " light",
        "lights",
        "thermostat",
        "temperature",
        "home assistant",
        "music",
        " tv",
        "volume",
        "alarm",
        "reminder",
        "kitchen",
        "bedroom",
        "living room",
        "garage",
        "front door",
        "time is it",
        "status",
        "search the web",
    ];
    MARKERS.iter().any(|marker| text.contains(marker))
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

/// Superset of command markers that prevent an ambient-narration rejection.
fn might_be_ambient_command(text: &str) -> bool {
    const MARKERS: &[&str] = &[
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
    MARKERS.iter().any(|marker| text.contains(marker))
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
}
