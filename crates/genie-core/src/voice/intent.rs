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
        // Whole words/phrases, not prefixes. These needles used to carry a
        // leading space and nothing on the right, so each one also matched the
        // head of a longer, unrelated word: " music" fired on "musical",
        // " alarm" on "alarming", " assistant" on "assistants", " lights" on
        // "lightsaber". Ordinary room chatter was then classified as a direct
        // request and forwarded to the LLM — the exact cost the ambient filter
        // exists to avoid. Plural forms that really are commands are listed
        // explicitly rather than falling out of prefix matching.
        || contains_any_word(
            text,
            &[
                "genie",
                "jarvis",
                "assistant",
                "light",
                "lights",
                "thermostat",
                "thermostats",
                "temperature",
                "home assistant",
                "music",
                "tv",
                "volume",
                "alarm",
                "alarms",
                "reminder",
                "reminders",
                "kitchen",
                "bedroom",
                "living room",
                "garage",
                "front door",
                "weather",
                "time is it",
                "status",
                "search the web",
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
        // Whole words, not prefixes. #854 gave "turn"/"set"/"play"/"search" a
        // leading space so "sunset"/"returned" stopped defeating the filter,
        // but left the right-hand side open, so the mirror-image words still
        // did: "the playground was empty…" matched " play", "the turnout was
        // low…" matched " turn", "the setback in negotiations…" matched " set",
        // "the crowd remembered…" matched "remember". Bounding both sides
        // closes the class instead of one half of it. Command-bearing
        // inflections that prefix matching used to cover are listed explicitly.
        && !contains_any_word(
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
                "timers",
                "remind",
                "reminder",
                "reminders",
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

/// True when any needle occurs in `text` as a whole word or whole phrase —
/// bounded on both sides by a non-alphanumeric character (or by the ends of the
/// string), rather than merely appearing as a substring.
///
/// A plain `contains` lets an unrelated longer word stand in for a command
/// ("playground" for "play"), and a leading space alone only closes the left
/// half of that. `text` has already been through `normalize_transcript`, which
/// collapses whitespace and ASCII-lowercases but deliberately keeps punctuation,
/// so the boundary test has to accept punctuation — a space-padded needle would
/// miss the very common "turn on the lights." and "hey, genie".
///
/// Boundaries are tested on bytes: every needle here is ASCII, so a match always
/// starts and ends on a char boundary, and any adjacent non-ASCII byte is
/// treated as a boundary (never as a letter continuing the word).
fn contains_any_word(text: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| contains_word_or_phrase(text, needle))
}

fn contains_word_or_phrase(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    text.match_indices(needle).any(|(start, matched)| {
        let end = start + matched.len();
        let left_open = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let right_open = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        left_open && right_open
    })
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
    fn rejects_ambient_narration_with_embedded_command_substrings() {
        // "sunset" embeds "set", "returned" embeds "turn"; the old bare-substring
        // needles let this narration escape the ambient filter to the LLM path.
        assert_eq!(
            assess_transcript("the sunset over the hills painted the sky in gold tonight"),
            VoiceIntentDecision::Reject("ambient narration")
        );
        assert_eq!(
            assess_transcript("the soldiers returned from the long campaign weary and changed"),
            VoiceIntentDecision::Reject("ambient narration")
        );
    }

    #[test]
    fn rejects_ambient_narration_whose_words_merely_start_with_a_command_word() {
        // #854 bounded "turn"/"set"/"play"/"search" on the LEFT only, so the
        // mirror-image words still slipped through: an ordinary word that
        // *starts with* a command word was read as a command. Every line here is
        // plain room chatter that reached the LLM before this fix.
        for text in [
            // ambient-negation needles, right-hand side
            "the playground was empty this morning after the storm passed through",
            "the turnout for the school concert was much smaller than last year",
            "the setback in the negotiations was severe for everyone involved today",
            "the crowd remembered the old song and sang along together all evening",
            "the photo reminded her of the summer they spent beside the lake",
            // direct-request needles
            "the musical was wonderful and everyone loved the songs performed tonight",
            "the alarming news about the storm kept everyone awake last night",
            "the assistants gathered in the hall before the ceremony began today",
        ] {
            assert_eq!(
                assess_transcript(text),
                VoiceIntentDecision::Reject("ambient narration"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn still_accepts_the_real_command_words_the_needles_are_for() {
        // The boundary rule must not cost a single genuine command. Punctuation
        // counts as a boundary — normalize_transcript keeps it, so a
        // space-padded needle would have missed the trailing-keyword forms.
        for text in [
            "hey genie, dim the lights.",
            "hey genie the lights are too bright",
            "any word on the weather",
            "my alarm did not go off",
            "the timer in the kitchen",
            "the reminder about the dentist",
            "the thermostat is set too high in the living room",
            "the tv is still on in the bedroom",
            "the music in the garage is too loud right now",
        ] {
            assert_eq!(
                assess_transcript(text),
                VoiceIntentDecision::Accept,
                "{text:?}"
            );
        }
    }

    #[test]
    fn word_boundary_matcher_rejects_prefixes_and_accepts_punctuation() {
        assert!(contains_word_or_phrase("turn on the lights.", "lights"));
        assert!(contains_word_or_phrase("hey, genie", "genie"));
        assert!(contains_word_or_phrase("genie turn on the fan", "genie"));
        assert!(contains_word_or_phrase(
            "the living room lamp",
            "living room"
        ));

        assert!(!contains_word_or_phrase("the lightsaber duel", "lights"));
        assert!(!contains_word_or_phrase("the playground was empty", "play"));
        assert!(!contains_word_or_phrase("the sunset was gold", "set"));
        assert!(!contains_word_or_phrase(
            "the soldiers returned home",
            "turn"
        ));
        assert!(!contains_word_or_phrase("the musical was long", "music"));
        assert!(!contains_word_or_phrase("", "play"));

        // A later, properly-bounded occurrence still wins over an earlier
        // embedded one — the scan must not stop at the first rejected match.
        assert!(contains_word_or_phrase(
            "the playground and play music",
            "play"
        ));
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
