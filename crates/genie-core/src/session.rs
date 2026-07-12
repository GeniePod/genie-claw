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
        self.track(session_key, now_ms);
        // `track` inserts `conversation_id == session_key` when absent and never
        // evicts the just-inserted entry (the cap is enforced before insert), so
        // the lookup succeeds; the fallback only guards a degenerate cap of 0.
        self.conversation_id(session_key)
            .map(str::to_string)
            .unwrap_or_else(|| session_key.to_string())
    }

    /// Register (or refresh) a conversation id in the bounded, idle-expiring
    /// registry without minting a new session key.
    ///
    /// Used when a client supplies its own `conversation_id`: those turns must
    /// still count against the Jetson session budget (idle expiry + LRU cap)
    /// instead of bypassing the registry, otherwise the real clients that always
    /// send an explicit id (web UI, Telegram) would never be bounded at all.
    pub fn track(&mut self, conversation_id: &str, now_ms: i64) {
        self.evict_idle(now_ms);
        if self.sessions.contains_key(conversation_id) {
            self.touch(conversation_id, now_ms);
            return;
        }
        self.enforce_cap();
        self.sessions.insert(
            conversation_id.to_string(),
            SessionEntry {
                conversation_id: conversation_id.to_string(),
                last_active_ms: now_ms,
            },
        );
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
    fn track_registers_client_supplied_conversation_under_the_cap() {
        // A client-supplied conversation id must count against the cap just like
        // a resolved session, otherwise clients that always send an explicit id
        // would bypass the Jetson session budget entirely.
        let mut registry = SessionRegistry::new(2, 60_000);
        registry.track("telegram-1", 1_000);
        registry.track("telegram-2", 2_000);
        registry.track("telegram-3", 3_000);
        assert_eq!(registry.len(), 2);
        // Oldest (telegram-1) was evicted to make room.
        assert!(registry.conversation_id("telegram-1").is_none());
        assert_eq!(registry.conversation_id("telegram-3"), Some("telegram-3"));
    }

    #[test]
    fn track_refreshes_activity_without_growing_the_registry() {
        let mut registry = SessionRegistry::new(4, 60_000);
        registry.track("http:dana", 1_000);
        registry.track("http:dana", 5_000);
        assert_eq!(registry.len(), 1);
        // The refreshed entry survives an eviction sweep past the original stamp.
        assert_eq!(registry.evict_idle(2_000 + 60_000), 0);
        assert_eq!(registry.conversation_id("http:dana"), Some("http:dana"));
    }

    #[test]
    fn tracked_session_idle_expires() {
        let mut registry = SessionRegistry::new(8, 1_000);
        registry.track("telegram-7", 1_000);
        assert_eq!(registry.len(), 1);
        // A later turn past the TTL sweeps the idle client session.
        registry.track("http:maya", 3_000);
        assert!(registry.conversation_id("telegram-7").is_none());
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
