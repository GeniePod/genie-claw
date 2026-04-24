use crate::memory::policy::{IdentityConfidence, MemoryReadContext};
use genie_common::config::{
    SpeakerIdentityConfig, SpeakerIdentityProvider as SpeakerIdentityProviderKind,
};

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

#[derive(Debug, Clone)]
pub struct SpeakerIdentityRequest<'a> {
    pub wav_path: Option<&'a str>,
    pub transcript: &'a str,
    pub detected_language: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub enum SpeakerIdentityProvider {
    #[default]
    None,
    Fixed(SpeakerIdentity),
}

impl SpeakerIdentityProvider {
    pub fn from_config(config: &SpeakerIdentityConfig) -> Self {
        if !config.enabled {
            return Self::None;
        }

        match config.provider {
            SpeakerIdentityProviderKind::None => Self::None,
            SpeakerIdentityProviderKind::Fixed => {
                let name = config.fixed_name.trim();
                if name.is_empty() {
                    Self::None
                } else {
                    Self::Fixed(SpeakerIdentity {
                        name: Some(name.to_string()),
                        confidence: identity_confidence_from_str(&config.fixed_confidence),
                    })
                }
            }
        }
    }

    pub fn identify(&self, _request: &SpeakerIdentityRequest<'_>) -> SpeakerIdentity {
        match self {
            Self::None => SpeakerIdentity::default(),
            Self::Fixed(identity) => identity.clone(),
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

fn identity_confidence_from_str(value: &str) -> IdentityConfidence {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" => IdentityConfidence::High,
        "medium" => IdentityConfidence::Medium,
        "low" => IdentityConfidence::Low,
        _ => IdentityConfidence::Unknown,
    }
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

    #[test]
    fn fixed_provider_returns_configured_identity() {
        let provider = SpeakerIdentityProvider::from_config(&SpeakerIdentityConfig {
            enabled: true,
            provider: SpeakerIdentityProviderKind::Fixed,
            fixed_name: "Jared".into(),
            fixed_confidence: "high".into(),
        });
        let identity = provider.identify(&SpeakerIdentityRequest {
            wav_path: None,
            transcript: "what do you remember about me",
            detected_language: Some("en"),
        });
        assert_eq!(identity.name.as_deref(), Some("Jared"));
        assert_eq!(identity.confidence, IdentityConfidence::High);
    }
}
