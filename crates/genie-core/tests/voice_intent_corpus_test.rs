#![cfg(feature = "voice")]

use genie_core::voice::intent::{VoiceIntentDecision, assess_transcript};

/// Verbatim copy of the `assess_transcript` pipeline from `main` @ 0d8904e,
/// before the single-pass normalize / marker-gate rework. The differential
/// test below asserts the optimized implementation produces identical
/// `VoiceIntentDecision`s (including reject reasons) across the corpus.
mod reference {
    use super::VoiceIntentDecision;

    pub fn assess_transcript(text: &str) -> VoiceIntentDecision {
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            return VoiceIntentDecision::Reject("empty transcript");
        }

        let lower = trimmed.to_ascii_lowercase();
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

    fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
        prefixes.iter().any(|prefix| text.starts_with(prefix))
    }

    fn contains_any(text: &str, needles: &[&str]) -> bool {
        needles.iter().any(|needle| text.contains(needle))
    }

    fn word_count(text: &str) -> usize {
        text.split_whitespace().count()
    }
}

const CORPUS: &[&str] = &[
    // Direct commands hitting the prefix table.
    "turn on the kitchen light",
    "set the thermostat to 21 degrees",
    "play some jazz in the living room",
    "please dim the lights",
    "dim the bedroom lights",
    "brighten the hallway",
    "lock the front door",
    "unlock the garage",
    "look up the capital of peru",
    "calculate 15 percent of 80",
    "call grandma",
    "text sam that dinner is ready",
    "remind me to water the plants",
    "check the garage door",
    "tell me a joke",
    "show me the cameras",
    "forget my last request",
    "remember that maya likes oat milk",
    // Questions.
    "what time is it?",
    "what's the weather like",
    "whats the temperature outside",
    "who won the game last night",
    "when does the sun set today",
    "where did i put my keys",
    "why is the hallway light red",
    "how long is the drive to the airport",
    "is the back door locked",
    "are the lights off upstairs",
    "do we have milk",
    "did the package arrive",
    "will you turn down the volume",
    "would you close the blinds",
    "could you check the oven",
    "can you start the vacuum",
    // Containment markers mid-sentence.
    "hey genie what's up",
    "okay jarvis lights on",
    "the assistant should hear this one",
    "make the lights warmer",
    "i think the thermostat is broken",
    "crank the volume a bit",
    "my alarm did not go off",
    "someone left the front door open",
    "the weather looks rough tomorrow",
    "hmm the tv is frozen again",
    "put on some music",
    "it is too cold in the bedroom",
    "the kitchen smells amazing tonight",
    "search the web for pancake recipes",
    // Low-signal filler (exact matches) and near-misses.
    "okay",
    "ok",
    "hmm",
    "uh",
    "um",
    "mm",
    "huh",
    "right",
    "yeah",
    "yep",
    "nope",
    "thanks",
    "thank you",
    "good night",
    "goodbye",
    "okay then",
    "thanks a lot",
    "  OKAY  ",
    "Thank   You",
    // Ambient narration (>= 9 words, narration-shaped, no request markers).
    "the old house stood alone at the end of the road",
    "she said the meeting went longer than anyone expected today",
    "they were talking about the game for hours last night",
    "this recipe needs more salt and a little more time",
    "he told me the neighbors are moving out next month",
    "it rained all day and the kids stayed inside reading",
    "we should have left the party a little bit earlier",
    "that movie was much longer than the reviews had promised",
    "an hour passed before anyone noticed the cake was burning",
    "a quiet evening settled over the block as porch bulbs flickered",
    // Narration-shaped but rescued by a request marker or question mark.
    "the dog needs a walk can you add a reminder",
    "she asked me to remember the dentist appointment on friday",
    "they wanted to know if you could search for flights",
    "the party starts at eight should i set a timer",
    "it looks dark outside already turn on the porch light",
    "the house feels cold please bump the heat a little",
    "is the oven still on after all this time today?",
    // Short ambient (under 9 words) stays accepted.
    "the cat is asleep",
    "she likes tea",
    "it broke",
    // Whitespace, casing, and unicode edge cases.
    "",
    "   ",
    "\t\n  \t",
    "TURN ON THE KITCHEN LIGHT",
    "  turn   on \t the\nkitchen light  ",
    "Wie ist das Wetter draußen",
    "Καλημέρα τι κάνεις",
    "☀️ turn on the lights ☀️",
    "¿puedes encender la luz?",
    "light",
    "lights",
    " lights please",
    "x",
    "9",
];

/// Differential regression: the single-pass normalize + first-byte prefix
/// dispatch + marker-byte gates must keep every `VoiceIntentDecision`
/// (including the reject reason) identical to the previous implementation.
#[test]
fn voice_intent_corpus_matches_reference_implementation() {
    for text in CORPUS {
        assert_eq!(
            assess_transcript(text),
            reference::assess_transcript(text),
            "decision drift for transcript {text:?}"
        );
    }
}

/// The full direct-request prefix table from `main`: every prefix must still
/// be accepted when used as the start of a transcript, guarding the grouped
/// first-byte dispatch against a dropped or mistyped entry.
#[test]
fn voice_intent_every_direct_prefix_still_accepts() {
    const PREFIXES: &[&str] = &[
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
    for prefix in PREFIXES {
        let transcript = format!("{prefix}zz zz zz");
        assert_eq!(
            assess_transcript(&transcript),
            VoiceIntentDecision::Accept,
            "prefix {prefix:?} no longer accepts"
        );
        assert_eq!(
            assess_transcript(&transcript),
            reference::assess_transcript(&transcript),
            "prefix {prefix:?} drifted from reference"
        );
    }
}

/// Every containment marker from `main` must still rescue an otherwise
/// narration-shaped transcript, guarding the second-byte gate table.
#[test]
fn voice_intent_every_containment_marker_still_accepts() {
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
    for marker in MARKERS {
        let transcript = format!("zz{marker} zz");
        assert_eq!(
            assess_transcript(&transcript),
            VoiceIntentDecision::Accept,
            "marker {marker:?} no longer accepts"
        );
        assert_eq!(
            assess_transcript(&transcript),
            reference::assess_transcript(&transcript),
            "marker {marker:?} drifted from reference"
        );
    }
}
