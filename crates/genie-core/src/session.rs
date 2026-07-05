//! Per-(channel, speaker) session lifecycle (#565 / #629).
//!
//! Maps a stable session key to a conversation id with idle expiry and a bounded
//! active-session cap so the Jetson agent does not track unbounded open chats.

use std::collections::BTreeMap;

/// Default cap on concurrently tracked sessions (HTTP + voice + Telegram).
pub const DEFAULT_MAX_ACTIVE_SESSIONS: usize = 32;

/// Default idle TTL before a session is evicted from the registry (30 minutes).
pub const DEFAULT_SESSION_IDLE_TTL_MS: i64 = 30 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionEntry {
    conversation_id: String,
    last_active_ms: i64,
}

/// Tracks active session keys → conversation ids with idle expiry and LRU cap.
#[derive(Debug, Clone)]
pub struct SessionRegistry {
    sessions: BTreeMap<String, SessionEntry>,
    max_sessions: usize,
    idle_ttl_ms: i64,
}

impl SessionRegistry {
    pub fn new(max_sessions: usize, idle_ttl_ms: i64) -> Self {
        Self {
            sessions: BTreeMap::new(),
            max_sessions: max_sessions.max(1),
            idle_ttl_ms: idle_ttl_ms.max(1),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_ACTIVE_SESSIONS, DEFAULT_SESSION_IDLE_TTL_MS)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn conversation_id(&self, session_key: &str) -> Option<&str> {
        self.sessions
            .get(session_key)
            .map(|entry| entry.conversation_id.as_str())
    }

    /// Resolve `session_key` to a conversation id, creating one when absent.
    pub fn resolve(&mut self, session_key: &str, now_ms: i64) -> String {
        self.evict_idle(now_ms);
        if let Some(entry) = self.sessions.get(session_key) {
            let conv_id = entry.conversation_id.clone();
            self.touch(session_key, now_ms);
            return conv_id;
        }
        self.enforce_cap();
        let conversation_id = session_key.to_string();
        self.sessions.insert(
            session_key.to_string(),
            SessionEntry {
                conversation_id: conversation_id.clone(),
                last_active_ms: now_ms,
            },
        );
        conversation_id
    }

    pub fn touch(&mut self, session_key: &str, now_ms: i64) {
        if let Some(entry) = self.sessions.get_mut(session_key) {
            entry.last_active_ms = now_ms;
        }
    }

    pub fn evict_idle(&mut self, now_ms: i64) -> usize {
        let cutoff = now_ms.saturating_sub(self.idle_ttl_ms);
        let stale: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, entry)| entry.last_active_ms < cutoff)
            .map(|(key, _)| key.clone())
            .collect();
        let count = stale.len();
        for key in stale {
            self.sessions.remove(&key);
        }
        count
    }

    fn enforce_cap(&mut self) {
        while self.sessions.len() >= self.max_sessions {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, entry)| entry.last_active_ms)
                .map(|(key, _)| key.clone());
            let Some(oldest_key) = oldest else {
                break;
            };
            self.sessions.remove(&oldest_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_creates_and_reuses_conversation_id() {
        let mut registry = SessionRegistry::with_defaults();
        let first = registry.resolve("http:dana", 1_000);
        let second = registry.resolve("http:dana", 2_000);
        assert_eq!(first, "http:dana");
        assert_eq!(second, first);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn idle_sessions_are_evicted_on_resolve() {
        let mut registry = SessionRegistry::new(8, 1_000);
        registry.resolve("http:dana", 1_000);
        assert_eq!(registry.len(), 1);
        registry.resolve("http:maya", 3_000);
        assert_eq!(registry.len(), 1);
        assert!(registry.conversation_id("http:dana").is_none());
        assert_eq!(registry.conversation_id("http:maya"), Some("http:maya"));
    }

    #[test]
    fn cap_evicts_least_recently_active_session() {
        let mut registry = SessionRegistry::new(2, 60_000);
        registry.resolve("http:alice", 1_000);
        registry.resolve("http:bob", 2_000);
        registry.touch("http:alice", 3_000);
        registry.resolve("http:carol", 4_000);
        assert!(registry.conversation_id("http:bob").is_none());
        assert_eq!(registry.conversation_id("http:alice"), Some("http:alice"));
        assert_eq!(registry.conversation_id("http:carol"), Some("http:carol"));
    }
}
