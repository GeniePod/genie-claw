//! Per-query memory injection into LLM system prompt.
//!
//! Instead of static "recent 5 memories" at startup, this module
//! searches for query-relevant memories and identity facts per turn.

use super::{Memory, policy};

/// Keep aligned with `agent_harness::MEMORY_HYDRATION_BUDGET_TOKENS`.
const MEMORY_HYDRATION_BUDGET_TOKENS: usize = 700;

fn estimate_hydration_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn format_memory_lines(entries: &[String]) -> String {
    entries
        .iter()
        .map(|entry| format!("- {entry}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_entry_to_budget(entry: &str, budget_tokens: usize) -> String {
    if estimate_hydration_tokens(&format!("- {entry}")) <= budget_tokens {
        return entry.to_string();
    }

    let chars: Vec<char> = entry.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let truncated: String = chars.iter().take(mid).copied().collect();
        let candidate = if mid >= chars.len() {
            truncated
        } else {
            format!("{truncated}…")
        };
        if estimate_hydration_tokens(&format!("- {candidate}")) <= budget_tokens {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }

    if lo == 0 {
        return String::new();
    }

    if lo >= chars.len() {
        entry.to_string()
    } else {
        format!("{}…", chars.iter().take(lo).copied().collect::<String>())
    }
}

fn apply_hydration_budget(entries: Vec<String>) -> String {
    if entries.is_empty() {
        return "(no household context yet)".to_string();
    }

    let total_candidates = entries.len();
    let mut selected = Vec::new();
    let first_entry = entries.first().cloned();

    for entry in entries {
        let mut trial = selected.clone();
        trial.push(entry);
        if estimate_hydration_tokens(&format_memory_lines(&trial)) <= MEMORY_HYDRATION_BUDGET_TOKENS
        {
            selected = trial;
        } else {
            break;
        }
    }

    let mut truncated_content = false;
    if selected.is_empty()
        && let Some(first) = first_entry
    {
        let truncated = truncate_entry_to_budget(&first, MEMORY_HYDRATION_BUDGET_TOKENS);
        if !truncated.is_empty() {
            selected.push(truncated);
            truncated_content = true;
        }
    }

    let dropped_entries = total_candidates.saturating_sub(selected.len());
    let output = format_memory_lines(&selected);

    if dropped_entries > 0 || truncated_content {
        tracing::warn!(
            estimated_tokens = estimate_hydration_tokens(&output),
            budget_tokens = MEMORY_HYDRATION_BUDGET_TOKENS,
            dropped_entries,
            kept_entries = selected.len(),
            "memory hydration truncated to fit Jetson token budget"
        );
    }

    output
}

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
    build_memory_context_with_read_context(
        memory,
        user_query,
        policy::MemoryReadContext::shared_room_voice(),
    )
}

/// Build memory context using explicit session/identity information.
///
/// This is the internal contract the voice/app layers should use once they can
/// resolve room, speaker identity, or explicit person/private intent. The
/// default `build_memory_context` remains conservative for shared-room voice.
pub fn build_memory_context_with_read_context(
    memory: &Memory,
    user_query: &str,
    read_context: policy::MemoryReadContext,
) -> String {
    let mut entries = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // Always inject identity memories.
    if let Ok(identities) = memory.get_by_kind("identity", 5) {
        for entry in identities {
            if seen_ids.insert(entry.id) && may_inject_entry(&entry, read_context) {
                entries.push(format!("[{}] {}", entry.kind, entry.content));
            }
        }
    }

    // Always inject relationship memories.
    if let Ok(relationships) = memory.get_by_kind("relationship", 3) {
        for entry in relationships {
            if seen_ids.insert(entry.id) && may_inject_entry(&entry, read_context) {
                entries.push(format!("[{}] {}", entry.kind, entry.content));
            }
        }
    }

    // Search for query-relevant memories.
    if !user_query.trim().is_empty()
        && let Ok(relevant) = memory.search(user_query, 5)
    {
        for entry in relevant {
            if seen_ids.insert(entry.id) && may_inject_entry(&entry, read_context) {
                entries.push(format!("[{}] {}", entry.kind, entry.content));
            }
        }
    }

    // Also include recent preferences if we have room.
    if entries.len() < 8
        && let Ok(prefs) = memory.get_by_kind("preference", 3)
    {
        for entry in prefs {
            if entries.len() >= 8 {
                break;
            }
            if seen_ids.insert(entry.id) && may_inject_entry(&entry, read_context) {
                entries.push(format!("[{}] {}", entry.kind, entry.content));
            }
        }
    }

    apply_hydration_budget(entries)
}

fn may_inject_entry(entry: &super::MemoryEntry, read_context: policy::MemoryReadContext) -> bool {
    policy::assess_memory_read(entry.metadata, read_context).allowed
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

    #[test]
    fn person_memory_needs_identity_context() {
        let mem = temp_memory();
        mem.store("person_preference", "Maya likes oat milk")
            .unwrap();

        let shared_room = build_memory_context(&mem, "oat milk");
        assert_eq!(shared_room, "(no household context yet)");

        let identified = build_memory_context_with_read_context(
            &mem,
            "oat milk",
            policy::MemoryReadContext {
                identity_confidence: policy::IdentityConfidence::Medium,
                explicit_named_person: false,
                explicit_private_intent: false,
                shared_space_voice: true,
            },
        );
        assert!(identified.contains("Maya likes oat milk"));
    }

    #[test]
    fn hydration_respects_700_token_budget() {
        let mem = temp_memory();
        let long = "household detail sentence. ".repeat(22);
        for i in 0..5 {
            mem.store("identity", &format!("{long} #{i}")).unwrap();
        }

        let ctx = build_memory_context(&mem, "turn on the kitchen lights");
        let tokens = estimate_hydration_tokens(&ctx);

        assert!(
            tokens <= MEMORY_HYDRATION_BUDGET_TOKENS,
            "tokens={tokens}: {ctx}"
        );
        assert!(
            ctx.contains("[identity]"),
            "at least one identity entry should be kept: {ctx}"
        );
    }

    #[test]
    fn hydration_drops_lower_priority_entries_before_preferences() {
        let mem = temp_memory();
        let long = "household detail sentence. ".repeat(18);
        for i in 0..5 {
            mem.store("identity", &format!("{long} identity {i}"))
                .unwrap();
        }
        for i in 0..3 {
            mem.store("relationship", &format!("{long} relationship {i}"))
                .unwrap();
        }
        mem.store("preference", &format!("{long} jazz preference"))
            .unwrap();

        let ctx = build_memory_context(&mem, "hello");
        assert!(estimate_hydration_tokens(&ctx) <= MEMORY_HYDRATION_BUDGET_TOKENS);
        assert!(
            ctx.contains("[identity]"),
            "identity entries should win over preferences: {ctx}"
        );
    }

    #[test]
    fn injection_uses_persisted_policy_metadata() {
        let mem = temp_memory();
        mem.store_with_metadata(
            "fact",
            "Maya likes oat milk",
            policy::MemoryPolicyMetadata {
                scope: policy::MemoryScope::Person,
                sensitivity: policy::MemorySensitivity::Normal,
                spoken_policy: policy::SpokenMemoryPolicy::Allow,
            },
            false,
        )
        .unwrap();

        let shared_room = build_memory_context(&mem, "oat milk");
        assert_eq!(shared_room, "(no household context yet)");

        let identified = build_memory_context_with_read_context(
            &mem,
            "oat milk",
            policy::MemoryReadContext {
                identity_confidence: policy::IdentityConfidence::High,
                explicit_named_person: false,
                explicit_private_intent: false,
                shared_space_voice: true,
            },
        );
        assert!(identified.contains("Maya likes oat milk"));
    }
}
