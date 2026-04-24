use crate::memory::policy::{IdentityConfidence, MemoryReadContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerIdentity {
    pub name: Option<String>,
    pub confidence: IdentityConfidence,
}

impl Default for SpeakerIdentity {
    fn default() -> Self {
        Self {
            name: None,
            confidence: IdentityConfidence::Unknown,
        }
    }
}

pub fn build_memory_read_context(text: &str, speaker: &SpeakerIdentity) -> MemoryReadContext {
    let lower = text.trim().to_ascii_lowercase();
    MemoryReadContext {
        identity_confidence: speaker.confidence,
        explicit_named_person: mentions_named_person(&lower),
        explicit_private_intent: contains_any(
            &lower,
            &[
                "private",
                "privately",
                "for me only",
                "don't say this aloud",
                "do not say this aloud",
            ],
        ),
        shared_space_voice: true,
    }
}

fn mentions_named_person(lower: &str) -> bool {
    starts_with_any(
        lower,
        &[
            "what does ",
            "what did ",
            "tell me about ",
            "who is ",
            "does ",
            "is ",
            "ask ",
            "call ",
            "text ",
            "message ",
            "remind ",
        ],
    ) || contains_any(
        lower,
        &[
            " my wife",
            " my husband",
            " my son",
            " my daughter",
            " my mom",
            " my mother",
            " my dad",
            " my father",
            " my friend",
            " my partner",
        ],
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
    fn build_memory_read_context_uses_speaker_confidence() {
        let ctx = build_memory_read_context(
            "what do you remember about me",
            &SpeakerIdentity {
                name: Some("Jared".into()),
                confidence: IdentityConfidence::High,
            },
        );
        assert_eq!(ctx.identity_confidence, IdentityConfidence::High);
        assert!(!ctx.explicit_named_person);
        assert!(ctx.shared_space_voice);
    }

    #[test]
    fn build_memory_read_context_detects_named_person_request() {
        let ctx =
            build_memory_read_context("what does Maya like to drink", &SpeakerIdentity::default());
        assert!(ctx.explicit_named_person);
    }

    #[test]
    fn build_memory_read_context_detects_private_intent() {
        let ctx = build_memory_read_context(
            "remember this privately and do not say this aloud",
            &SpeakerIdentity::default(),
        );
        assert!(ctx.explicit_private_intent);
    }
}
