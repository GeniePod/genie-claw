//! Per-query memory injection into LLM system prompt.
//!
//! Instead of static "recent 5 memories" at startup, this module
//! searches for query-relevant memories and identity facts per turn.

use super::{Memory, policy};

/// Keep aligned with `agent_harness::MEMORY_HYDRATION_BUDGET_TOKENS`.
const MEMORY_HYDRATION_BUDGET_TOKENS: usize = 700;

fn estimate_hydration_tokens(text: &str) -> usize {
    estimate_hydration_tokens_from_len(text.len())
}

/// Token estimate from a byte length. Shared by [`estimate_hydration_tokens`]
/// and the incremental fit in [`fit_entries_to_budget`] so both paths apply the
/// identical `div_ceil(4)` rule and can never disagree.
fn estimate_hydration_tokens_from_len(len: usize) -> usize {
    len.div_ceil(4)
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

/// Greedily keep the longest in-order prefix of `entries` whose formatted
/// `- {entry}` lines (joined by newlines) fit the hydration token budget.
///
/// The straightforward way to compute this is to grow a `selected` vector and,
/// on each step, re-render every kept entry to measure the budget — which clones
/// the vector and re-`join`s all kept entries once per candidate (O(n^2) string
/// work, n allocations). But `format_memory_lines` renders each entry as a fixed
/// 2-byte `- ` prefix plus the entry bytes, joined by single newlines, so the joined
/// byte length is exactly additive:
///
/// ```text
/// len(join(k entries)) = sum(2 + entry.len())  +  (k - 1)   // k >= 1
/// ```
///
/// Tracking that length incrementally and applying the same `div_ceil(4)` token
/// estimate reproduces the identical accept/reject decision at every step — and
/// therefore the identical kept prefix — in O(n) with no per-step clone or
/// re-join. See the `hydration_fit_matches_reference_*` regression tests.
fn fit_entries_to_budget(entries: Vec<String>) -> Vec<String> {
    let mut selected: Vec<String> = Vec::with_capacity(entries.len());
    // Running byte length of `format_memory_lines(&selected)`, kept in lockstep
    // with `selected` so the budget check never re-scans the already-kept lines.
    let mut joined_len = 0usize;

    for entry in entries {
        // This entry adds its `- {entry}` line (2-byte prefix + entry bytes) and,
        // unless it is the first kept line, one newline separator.
        let separator = usize::from(!selected.is_empty());
        let trial_len = joined_len + separator + 2 + entry.len();
        if estimate_hydration_tokens_from_len(trial_len) <= MEMORY_HYDRATION_BUDGET_TOKENS {
            joined_len = trial_len;
            selected.push(entry);
        } else {
            break;
        }
    }

    selected
}

fn apply_hydration_budget(entries: Vec<String>) -> String {
    if entries.is_empty() {
        return "(no household context yet)".to_string();
    }

    let total_candidates = entries.len();
    let first_entry = entries.first().cloned();
    let mut selected = fit_entries_to_budget(entries);

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

    /// The original O(n^2) budget fit: clone the growing prefix and re-`join`
    /// every kept entry to measure the budget on each step. Kept verbatim as the
    /// oracle the linear `fit_entries_to_budget` must match byte-for-byte.
    fn fit_entries_reference(entries: Vec<String>) -> Vec<String> {
        let mut selected: Vec<String> = Vec::new();
        for entry in entries {
            let mut trial = selected.clone();
            trial.push(entry);
            if estimate_hydration_tokens(&format_memory_lines(&trial))
                <= MEMORY_HYDRATION_BUDGET_TOKENS
            {
                selected = trial;
            } else {
                break;
            }
        }
        selected
    }

    /// Entry sets that exercise the accept path, the exact-boundary case, and the
    /// break path — including the first entry alone already overflowing.
    fn fit_corpus() -> Vec<Vec<String>> {
        let short = || "[identity] Household member name is Jared".to_string();
        let long = || format!("[identity] {}", "household detail sentence. ".repeat(20));
        vec![
            vec![],
            vec![short()],
            vec![short(), short(), short()],
            // Straddle the 700-token budget so the greedy fit stops mid-list.
            (0..12).map(|i| format!("{} #{i}", long())).collect(),
            // First entry alone overflows -> empty selection (truncate path).
            vec![format!("{} {}", long(), long())],
            // Mixed lengths so the accepted prefix is a non-trivial boundary.
            vec![short(), long(), short(), long(), short()],
            // Multi-byte content: byte-length accounting must use bytes, not chars.
            vec!["[identity] café — naïve façade ✅".to_string(); 6],
        ]
    }

    #[test]
    fn hydration_fit_matches_reference_selection() {
        for entries in fit_corpus() {
            let expected = fit_entries_reference(entries.clone());
            let actual = fit_entries_to_budget(entries.clone());
            assert_eq!(
                actual, expected,
                "linear fit selected a different prefix than the O(n^2) oracle for {entries:?}"
            );
        }
    }

    #[test]
    fn hydration_fit_matches_reference_rendered_output() {
        // The user-visible contract is the rendered string; pin it to the oracle.
        for entries in fit_corpus() {
            let expected = format_memory_lines(&fit_entries_reference(entries.clone()));
            let actual = format_memory_lines(&fit_entries_to_budget(entries.clone()));
            assert_eq!(
                actual, expected,
                "rendered hydration differs for {entries:?}"
            );
        }
    }

    #[test]
    fn hydration_fit_kept_prefix_is_within_budget_and_maximal() {
        // Independent of the oracle: the kept prefix fits, and the next dropped
        // entry (if any) would have overflowed — i.e. the greedy fit is maximal.
        for entries in fit_corpus() {
            let kept = fit_entries_to_budget(entries.clone());
            assert!(
                estimate_hydration_tokens(&format_memory_lines(&kept))
                    <= MEMORY_HYDRATION_BUDGET_TOKENS,
                "kept prefix exceeds budget for {entries:?}"
            );
            if kept.len() < entries.len() {
                let mut with_next = kept.clone();
                with_next.push(entries[kept.len()].clone());
                assert!(
                    estimate_hydration_tokens(&format_memory_lines(&with_next))
                        > MEMORY_HYDRATION_BUDGET_TOKENS,
                    "fit stopped early while the next entry still fit, for {entries:?}"
                );
            }
        }
    }

    #[test]
    #[ignore = "benchmark; run with --release --ignored --nocapture"]
    fn bench_hydration_fit_linear_vs_quadratic() {
        use std::time::Instant;
        // A realistic-to-worst-case candidate set: the injector caps entries at 8
        // per turn, but the quadratic cost is in the re-join, so we also show a
        // larger set to make the asymptotic gap visible. Every entry is short
        // enough that the whole prefix fits, so both fits keep all entries and do
        // the maximum work.
        for n in [8usize, 64, 256] {
            let entries: Vec<String> = (0..n)
                .map(|i| format!("[identity] household member {i}"))
                .collect();
            let iters = 5000u32;

            // Warm up + prevent the optimizer from eliding the calls.
            let mut sink = 0usize;
            let t = Instant::now();
            for _ in 0..iters {
                sink += std::hint::black_box(fit_entries_reference(std::hint::black_box(
                    entries.clone(),
                )))
                .len();
            }
            let old_ns = t.elapsed().as_nanos() as f64 / iters as f64;

            let t = Instant::now();
            for _ in 0..iters {
                sink += std::hint::black_box(fit_entries_to_budget(std::hint::black_box(
                    entries.clone(),
                )))
                .len();
            }
            let new_ns = t.elapsed().as_nanos() as f64 / iters as f64;

            eprintln!(
                "BENCH hydration fit [n={n}]: old(O(n^2)) {old_ns:.0} ns -> new(O(n)) {new_ns:.0} ns ({:.1}x) (sink={sink})",
                old_ns / new_ns
            );
        }
    }

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
