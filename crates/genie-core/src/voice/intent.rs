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
    let (lower, words) = normalize_transcript(text);
    if lower.is_empty() {
        return VoiceIntentDecision::Reject("empty transcript");
    }

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

/// Collapse whitespace runs to single spaces, ASCII-lowercase, and count
/// words in one pass over the transcript. Replaces the previous
/// `split_whitespace().collect::<Vec<_>>().join(" ")` (Vec + join
/// allocations) followed by a second full-string `to_ascii_lowercase`
/// allocation; `char::to_ascii_lowercase` applied per char produces the
/// same bytes as `str::to_ascii_lowercase` on the joined string, and
/// `split_whitespace` yields no empty items, so the word count and the
/// normalized text are identical to `main`.
fn normalize_transcript(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut words = 0usize;
    for word in text.split_whitespace() {
        if words > 0 {
            out.push(' ');
        }
        words += 1;
        for ch in word.chars() {
            out.push(ch.to_ascii_lowercase());
        }
    }
    (out, words)
}

fn looks_like_direct_request(text: &str) -> bool {
    text.ends_with('?') || starts_with_direct_prefix(text) || contains_direct_marker(text)
}

/// The direct-request prefix table from `main`, grouped by first byte so a
/// transcript probes only the prefixes sharing its first letter instead of
/// all 39. Every prefix starts with a lowercase ASCII letter, and the text
/// is already lowercased, so dispatching on the first byte cannot drop a
/// match.
fn starts_with_direct_prefix(text: &str) -> bool {
    let candidates: &[&str] = match text.as_bytes().first() {
        Some(b'a') => &["are "],
        Some(b'b') => &["brighten "],
        Some(b'c') => &[
            "can you ",
            "could you ",
            "check ",
            "close ",
            "calculate ",
            "call ",
        ],
        Some(b'd') => &["do ", "did ", "dim "],
        Some(b'f') => &["forget "],
        Some(b'h') => &["how "],
        Some(b'i') => &["is "],
        Some(b'l') => &["look up ", "lock "],
        Some(b'o') => &["open "],
        Some(b'p') => &["please ", "play "],
        Some(b'r') => &["remember ", "remind "],
        Some(b's') => &["set ", "search ", "show me "],
        Some(b't') => &["turn ", "tell me ", "timer ", "text "],
        Some(b'u') => &["unlock "],
        Some(b'w') => &[
            "what ",
            "what's ",
            "whats ",
            "who ",
            "when ",
            "where ",
            "why ",
            "would you ",
            "will you ",
            "weather ",
        ],
        _ => return false,
    };
    starts_with_any(text, candidates)
}

/// Containment markers from `main`. Every marker is `' '` followed by a
/// lowercase ASCII letter, so one byte pass recording which letters actually
/// follow a space lets the scan probe only the markers that can possibly
/// match, instead of running all 22 substring searches per transcript.
fn contains_direct_marker(text: &str) -> bool {
    const MARKERS: &[&str] = &[
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

    let mut after_space = [false; 26];
    let mut any_after_space = false;
    for pair in text.as_bytes().windows(2) {
        if pair[0] == b' ' && pair[1].is_ascii_lowercase() {
            after_space[(pair[1] - b'a') as usize] = true;
            any_after_space = true;
        }
    }
    if !any_after_space {
        return false;
    }

    MARKERS.iter().any(|marker| {
        let second = marker.as_bytes()[1];
        after_space[(second - b'a') as usize] && text.contains(marker)
    })
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

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| text.starts_with(prefix))
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
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
}
