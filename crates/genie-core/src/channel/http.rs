//! HTTP JSON → `IncomingTurn` adapter (#564 reference).
//!
//! Parses the `/api/chat` request body into a normalized turn so HTTP routes
//! can plug into the shared `Channel` boundary without duplicating speaker
//! and session field handling.

use super::{ChannelKind, IncomingTurn, SpeakerInfo};
use crate::memory::policy::IdentityConfidence;

/// Build an HTTP `IncomingTurn` from a parsed chat JSON body.
pub fn incoming_turn_from_chat_json(
    parsed: &serde_json::Value,
    fallback_session: &str,
) -> IncomingTurn {
    let text = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let mut turn = IncomingTurn::new(text, fallback_session, ChannelKind::Http);
    if let Some(speaker) = parsed.get("speaker").map(parse_speaker_field)
        && speaker.is_resolved()
    {
        turn = turn.with_speaker(speaker);
    }
    turn
}

fn parse_speaker_field(value: &serde_json::Value) -> SpeakerInfo {
    match value {
        serde_json::Value::String(name) if !name.trim().is_empty() => SpeakerInfo {
            name: Some(name.trim().to_string()),
            confidence: IdentityConfidence::High,
        },
        serde_json::Value::Object(_) => {
            let map = value.as_object().expect("object branch");
            let name = map
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string);
            let confidence = map
                .get("confidence")
                .and_then(|v| v.as_str())
                .map(identity_confidence_from_str)
                .unwrap_or(IdentityConfidence::High);
            SpeakerInfo { name, confidence }
        }
        _ => SpeakerInfo::default(),
    }
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
    fn parses_message_and_default_speaker() {
        let parsed = serde_json::json!({"message": "hello"});
        let turn = incoming_turn_from_chat_json(&parsed, "default");
        assert_eq!(turn.text, "hello");
        assert_eq!(turn.session_id, "default");
        assert_eq!(turn.channel, ChannelKind::Http);
        assert!(!turn.speaker.is_resolved());
    }

    #[test]
    fn parses_string_speaker_name() {
        let parsed = serde_json::json!({"message": "hi", "speaker": " Dana "});
        let turn = incoming_turn_from_chat_json(&parsed, "default");
        assert_eq!(turn.speaker.name.as_deref(), Some("Dana"));
        assert_eq!(turn.speaker.confidence, IdentityConfidence::High);
        assert_eq!(turn.conversation_id("default"), "http:dana");
    }

    #[test]
    fn parses_object_speaker_with_confidence() {
        let parsed = serde_json::json!({
            "message": "hi",
            "speaker": {"name": "maya", "confidence": "medium"}
        });
        let turn = incoming_turn_from_chat_json(&parsed, "default");
        assert_eq!(turn.speaker.name.as_deref(), Some("maya"));
        assert_eq!(turn.speaker.confidence, IdentityConfidence::Medium);
    }

    #[test]
    fn empty_speaker_object_defaults_confidence_without_name() {
        let parsed = serde_json::json!({"message": "hi", "speaker": {}});
        let turn = incoming_turn_from_chat_json(&parsed, "sess-1");
        assert!(turn.speaker.is_resolved());
        assert_eq!(turn.speaker.confidence, IdentityConfidence::High);
        assert_eq!(turn.conversation_id("sess-1"), "sess-1");
    }
}
