#![cfg(feature = "voice")]
use genie_core::voice::intent::{VoiceIntentDecision, assess_transcript};

const CORPUS: &[(&str, VoiceIntentDecision)] = &[
    ("turn on the kitchen light", VoiceIntentDecision::Accept),
    ("what time is it?", VoiceIntentDecision::Accept),
    (
        "thank you",
        VoiceIntentDecision::Reject("low-signal filler"),
    ),
    (
        "the old house stood alone at the end of the road",
        VoiceIntentDecision::Reject("ambient narration"),
    ),
    ("weather in Tokyo", VoiceIntentDecision::Accept),
    ("  turn   on   the   light  ", VoiceIntentDecision::Accept),
    ("okay", VoiceIntentDecision::Reject("low-signal filler")),
    (
        "please turn on the bedroom lights when you get a chance tonight",
        VoiceIntentDecision::Accept,
    ),
];

#[test]
fn assess_transcript_corpus_regression() {
    for (text, want) in CORPUS {
        assert_eq!(
            assess_transcript(text),
            *want,
            "unexpected decision for {text:?}"
        );
    }
}
