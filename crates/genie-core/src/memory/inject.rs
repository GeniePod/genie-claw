//! Per-query memory injection into LLM system prompt.
//!
//! Instead of static "recent 5 memories" at startup, this module
//! searches for query-relevant memories and identity facts per turn.

use super::{Memory, policy};

/// Build the memory section to append to the system prompt for a given query.
///
/// Strategy:
/// 1. Always include identity memories
/// 2. Search for query-relevant memories
/// 3. Deduplicate and format
///
/// Returns a string like:
/// ```text
/// Relevant household context:
/// - [identity] Household member name is Jared
/// - [preference] Jared likes spicy food
/// ```
pub fn build_memory_context(memory: &Memory, user_query: &str) -> String {
    let mut entries = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // Always inject identity memories.
    if let Ok(identities) = memory.get_by_kind("identity", 5) {
        for entry in identities {
            if seen_ids.insert(entry.id)
                && policy::may_inject_into_shared_prompt(&entry.kind, &entry.content)
            {
                entries.push(format!("[{}] {}", entry.kind, entry.content));
            }
        }
    }

    // Always inject relationship memories.
    if let Ok(relationships) = memory.get_by_kind("relationship", 3) {
        for entry in relationships {
            if seen_ids.insert(entry.id)
                && policy::may_inject_into_shared_prompt(&entry.kind, &entry.content)
            {
                entries.push(format!("[{}] {}", entry.kind, entry.content));
            }
        }
    }

    // Search for query-relevant memories.
    if !user_query.trim().is_empty() {
        if let Ok(relevant) = memory.search(user_query, 5) {
            for entry in relevant {
                if seen_ids.insert(entry.id)
                    && policy::may_inject_into_shared_prompt(&entry.kind, &entry.content)
                {
                    entries.push(format!("[{}] {}", entry.kind, entry.content));
                }
            }
        }
    }

    // Also include recent preferences if we have room.
    if entries.len() < 8 {
        if let Ok(prefs) = memory.get_by_kind("preference", 3) {
            for entry in prefs {
                if entries.len() >= 8 {
                    break;
                }
                if seen_ids.insert(entry.id)
                    && policy::may_inject_into_shared_prompt(&entry.kind, &entry.content)
                {
                    entries.push(format!("[{}] {}", entry.kind, entry.content));
                }
            }
        }
    }

    if entries.is_empty() {
        return "(no household context yet)".to_string();
    }

    entries
        .iter()
        .map(|e| format!("- {}", e))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_memory() -> Memory {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-inject-test-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        Memory::open(&path).unwrap()
    }

    #[test]
    fn inject_empty_db() {
        let mem = temp_memory();
        let ctx = build_memory_context(&mem, "hello");
        assert_eq!(ctx, "(no household context yet)");
    }

    #[test]
    fn inject_identity_always_present() {
        let mem = temp_memory();
        mem.store("identity", "User's name is Jared").unwrap();
        mem.store("fact", "The sky is blue").unwrap();

        // Query about weather — identity should still be injected.
        let ctx = build_memory_context(&mem, "weather");
        assert!(ctx.contains("Jared"), "identity should always be injected");
    }

    #[test]
    fn inject_query_relevant() {
        let mem = temp_memory();
        mem.store("preference", "User likes jazz music").unwrap();
        mem.store("preference", "User dislikes cold weather")
            .unwrap();

        let ctx = build_memory_context(&mem, "play some music");
        assert!(
            ctx.contains("jazz"),
            "jazz should be relevant to 'play some music'"
        );
    }

    #[test]
    fn inject_deduplicates() {
        let mem = temp_memory();
        mem.store("identity", "User's name is Jared").unwrap();

        // "Jared" query would match the identity entry — should not appear twice.
        let ctx = build_memory_context(&mem, "Jared");
        let count = ctx.matches("Jared").count();
        assert_eq!(count, 1, "should not duplicate: {}", ctx);
    }

    #[test]
    fn inject_skips_restricted_memory() {
        let mem = temp_memory();
        mem.store("fact", "User's password is swordfish").unwrap();

        let ctx = build_memory_context(&mem, "password");

        assert_eq!(ctx, "(no household context yet)");
    }
}
