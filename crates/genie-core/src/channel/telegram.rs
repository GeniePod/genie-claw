//! Telegram → `IncomingTurn` reference adapter (#564 / #783).
//!
//! Normalizes a Telegram chat id + message into the shared `Channel` boundary
//! and builds the `/api/chat` JSON body the in-process adapter posts to core.

use super::{ChannelKind, IncomingTurn};

/// Stable session / conversation id for a Telegram chat (backward compatible).
pub fn telegram_session_id(chat_id: i64) -> String {
    format!("telegram-{chat_id}")
}

/// Build a Telegram `IncomingTurn` from a chat id and user text.
pub fn incoming_turn_from_telegram(chat_id: i64, text: impl Into<String>) -> IncomingTurn {
    IncomingTurn::new(text, telegram_session_id(chat_id), ChannelKind::Telegram)
}

/// `/api/chat` JSON body for the in-process Telegram adapter.
pub fn telegram_chat_json(chat_id: i64, text: impl Into<String>) -> serde_json::Value {
    let turn = incoming_turn_from_telegram(chat_id, text);
    serde_json::json!({
        "message": turn.text,
        "conversation_id": turn.session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_preserves_legacy_format() {
        assert_eq!(telegram_session_id(42), "telegram-42");
    }

    #[test]
    fn incoming_turn_uses_telegram_channel_and_session() {
        let turn = incoming_turn_from_telegram(99, "hello");
        assert_eq!(turn.text, "hello");
        assert_eq!(turn.session_id, "telegram-99");
        assert_eq!(turn.channel, ChannelKind::Telegram);
        assert!(!turn.speaker.is_resolved());
        assert_eq!(turn.conversation_id("fallback"), "fallback");
    }

    #[test]
    fn chat_json_matches_core_adapter_shape() {
        let body = telegram_chat_json(7, "ping");
        assert_eq!(body["message"], "ping");
        assert_eq!(body["conversation_id"], "telegram-7");
    }
}
