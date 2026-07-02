use anyhow::Result;

use super::decay;
use super::policy;
use super::{Memory, MemoryEntry, SemanticMemoryHit};

/// Dreaming-inspired memory consolidation.
///
/// Three phases (inspired by OpenClaw's dreaming-phases.ts):
///
/// 1. Light: quick scan — identify frequently recalled memories
/// 2. Deep: promote high-scoring memories to permanent (evergreen)
/// 3. Prune: remove decayed memories below threshold
///
/// Called by genie-governor during night mode, or manually via CLI.

/// Promotion scoring weights (from OpenClaw's 6-component system).
/// Simplified to 5 components for V1 (no vector embeddings = no conceptual coherence).
#[derive(Debug, Clone)]
pub struct PromotionWeights {
    pub frequency: f64,     // how often recalled
    pub relevance: f64,     // best single score
    pub recency: f64,       // how recently recalled
    pub consolidation: f64, // temporal spread of recalls
    pub diversity: f64,     // how many distinct query shapes recalled it
}

impl Default for PromotionWeights {
    fn default() -> Self {
        Self {
            frequency: 0.25,
            relevance: 0.30,
            recency: 0.20,
            consolidation: 0.15,
            diversity: 0.10,
        }
    }
}

/// Scored promotion candidate.
#[derive(Debug, Clone)]
pub struct PromotionCandidate {
    pub entry: MemoryEntry,
    pub score: f64,
    pub frequency_score: f64,
    pub relevance_score: f64,
    pub recency_score: f64,
    pub consolidation_score: f64,
    pub diversity_score: f64,
}

#[derive(Debug, Clone)]
pub struct RecallableMemory {
    pub entry: MemoryEntry,
    pub decision: policy::MemoryPolicyDecision,
}

/// Run the dreaming consolidation cycle.
///
/// Returns: (promoted_count, pruned_count)
pub fn dream_cycle(
    memory: &Memory,
    weights: &PromotionWeights,
    min_score: f64,
    min_recalls: i64,
    max_promotions: usize,
    prune_threshold: f64,
) -> Result<(usize, usize)> {
    // Phase 1: Score candidates.
    let candidates = score_candidates(memory, weights, min_recalls)?;

    // Phase 2: Promote top candidates above threshold.
    //
    // Collect the winners first and promote them in a single batch so the
    // canonical MEMORY.md tree is rebuilt once for the whole cycle rather than
    // once per promotion (each rebuild is a full `promoted = 1` scan plus a
    // rewrite of MEMORY.md, INDEX.md, and every namespace file). Same rows
    // promoted, same per-promotion log lines.
    let winners: Vec<&PromotionCandidate> = candidates
        .iter()
        .take(max_promotions)
        .filter(|candidate| candidate.score >= min_score)
        .collect();

    if !winners.is_empty() {
        let ids: Vec<i64> = winners.iter().map(|candidate| candidate.entry.id).collect();
        memory.mark_promoted_many(&ids)?;
        for candidate in &winners {
            tracing::info!(
                id = candidate.entry.id,
                score = format!("{:.3}", candidate.score),
                recalls = candidate.entry.recall_count,
                content = &candidate.entry.content[..candidate.entry.content.len().min(60)],
                "memory promoted to permanent"
            );
        }
    }
    let promoted = winners.len();

    // Phase 3: Prune decayed memories.
    let pruned = memory.prune_decayed(prune_threshold)?;
    if pruned > 0 {
        tracing::info!(pruned, "decayed memories removed");
    }

    Ok((promoted, pruned))
}

/// Score all promotion candidates using weighted components.
pub fn score_candidates(
    memory: &Memory,
    weights: &PromotionWeights,
    min_recalls: i64,
) -> Result<Vec<PromotionCandidate>> {
    let entries = memory.promotion_candidates(min_recalls, 0.0, 1000)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;

    let mut candidates = Vec::new();

    // Fetch query diversity for every candidate in ONE query instead of a
    // `SELECT query_hashes WHERE id = ?` per entry — the N+1 that scaled with the
    // up-to-1000-row candidate set. Same per-id counts, so same scores/ordering.
    let candidate_ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    let diversity_by_id = memory
        .query_diversity_for_ids(&candidate_ids)
        .unwrap_or_default();

    for entry in entries {
        // Frequency: normalized recall count (asymptotic to 1.0).
        let frequency_score = (entry.recall_count as f64 / 10.0).min(1.0);

        // Relevance: best single score ever achieved.
        let relevance_score = entry.max_score.min(1.0);

        // Recency: exponential decay from last access.
        let days_since_access = (now_ms - entry.accessed_ms as f64) / 86_400_000.0;
        let recency_score = decay::exponential_decay(days_since_access, 14.0);

        // Consolidation: based on recall count spread (simplified).
        // More recalls = higher consolidation.
        let consolidation_score = consolidation_from_count(entry.recall_count);

        // Diversity: repeated daily usefulness matters more when the fact
        // is recalled from different prompts, not only the same phrase.
        let diversity_score =
            diversity_from_unique_queries(diversity_by_id.get(&entry.id).copied().unwrap_or(0));

        // Weighted sum.
        let score = weights.frequency * frequency_score
            + weights.relevance * relevance_score
            + weights.recency * recency_score
            + weights.consolidation * consolidation_score
            + weights.diversity * diversity_score;

        candidates.push(PromotionCandidate {
            entry,
            score,
            frequency_score,
            relevance_score,
            recency_score,
            consolidation_score,
            diversity_score,
        });
    }

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(candidates)
}

pub fn recall_with_context(
    memory: &Memory,
    query: &str,
    limit: usize,
    context: policy::MemoryReadContext,
) -> Result<Vec<RecallableMemory>> {
    let lexical = memory.search(query, limit)?;
    let semantic = memory.semantic_search(query, limit)?;
    let results = fuse_recall_candidates(lexical, semantic, limit);
    let raw_hits = results.len();
    let recalled = filter_recall_results(results, context);

    if !query.trim().is_empty() && recalled.is_empty() {
        // Distinguish "FTS/LIKE returned nothing" from "results were dropped by
        // shared-room / scope / sensitivity policy". Both produce an empty
        // recall context, but they need different remediation, so we emit a
        // labeled event instead of letting the miss vanish into a silent
        // empty prompt context (M1 exit criterion, issue #111).
        let cause = if raw_hits == 0 {
            "no_match"
        } else {
            "policy_filtered"
        };
        tracing::warn!(
            target: "memory.recall.miss",
            cause,
            raw_hits = raw_hits as u64,
            query_len = query.len() as u64,
            identity_confidence = ?context.identity_confidence,
            explicit_named_person = context.explicit_named_person,
            shared_space_voice = context.shared_space_voice,
            "memory recall miss"
        );
    }

    Ok(recalled)
}

/// Reciprocal-Rank-Fusion constant. The standard value from the original RRF
/// paper (Cormack et al.); larger `k` flattens the contribution of top ranks,
/// smaller `k` sharpens it. 60 is the widely-used default.
const RRF_K: f64 = 60.0;

/// Fuse the lexical (BM25/LIKE) and semantic (cosine) recall rankings into a
/// single relevance-ordered list using Reciprocal Rank Fusion.
///
/// Previously the two result sets were concatenated (lexical first, semantic
/// appended) and then truncated to `limit`, which silently discarded every
/// semantic hit whenever lexical search already filled the limit, and never
/// ranked the combined set by relevance. RRF fixes both: each list contributes
/// `1 / (RRF_K + rank)` per item (rank is 1-based), contributions for an id
/// present in both lists are summed (agreement boosts it), and the merged set
/// is sorted by the fused score before truncation.
///
/// RRF is rank-based, so it sidesteps the fact that BM25-derived scores and
/// cosine similarities live on different, non-comparable scales. Ties are
/// broken by first-seen order (lexical before semantic, then original rank),
/// giving a deterministic result.
fn fuse_recall_candidates(
    lexical: Vec<MemoryEntry>,
    semantic: Vec<SemanticMemoryHit>,
    limit: usize,
) -> Vec<MemoryEntry> {
    use std::collections::HashMap;
    use std::collections::hash_map::Entry as MapEntry;

    let mut score_by_id: HashMap<i64, f64> = HashMap::new();
    let mut entry_by_id: HashMap<i64, MemoryEntry> = HashMap::new();
    // First-seen order, used as a stable tie-breaker.
    let mut seen_order: Vec<i64> = Vec::new();

    let mut accumulate = |id: i64, rank: usize, entry: MemoryEntry| {
        // `rank` is 0-based here; RRF uses 1-based ranks.
        let contribution = 1.0 / (RRF_K + (rank as f64) + 1.0);
        *score_by_id.entry(id).or_insert(0.0) += contribution;
        if let MapEntry::Vacant(slot) = entry_by_id.entry(id) {
            slot.insert(entry);
            seen_order.push(id);
        }
    };

    // Lexical hits arrive already ordered best-first (decayed BM25).
    for (rank, entry) in lexical.into_iter().enumerate() {
        accumulate(entry.id, rank, entry);
    }
    // Semantic hits arrive already ordered best-first (cosine similarity).
    for (rank, hit) in semantic.into_iter().enumerate() {
        accumulate(hit.entry.id, rank, hit.entry);
    }

    // Stable sort by fused score (desc); equal scores keep first-seen order.
    let mut ranked = seen_order;
    ranked.sort_by(|a, b| {
        let sa = score_by_id.get(a).copied().unwrap_or(0.0);
        let sb = score_by_id.get(b).copied().unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    ranked
        .into_iter()
        .take(limit.max(1))
        .filter_map(|id| entry_by_id.remove(&id))
        .collect()
}

pub fn filter_recall_results(
    entries: Vec<MemoryEntry>,
    context: policy::MemoryReadContext,
) -> Vec<RecallableMemory> {
    entries
        .into_iter()
        .filter_map(|entry| {
            let decision = policy::assess_memory_read(entry.metadata, context);
            if decision.allowed {
                Some(RecallableMemory { entry, decision })
            } else {
                None
            }
        })
        .collect()
}

/// Consolidation score from recall count (log-scaled).
///
/// 1 recall → 0.0
/// 3 recalls → 0.50
/// 5 recalls → 0.80
/// 10+ recalls → 1.0
fn consolidation_from_count(recall_count: i64) -> f64 {
    if recall_count <= 1 {
        return 0.0;
    }
    let x = (recall_count - 1) as f64;
    (x.ln_1p() / 9.0_f64.ln_1p()).min(1.0)
}

/// Diversity score from distinct query hashes.
///
/// 0 queries → 0.0
/// 1 query   → 0.25
/// 2 queries → 0.50
/// 4+ queries → 1.0
fn diversity_from_unique_queries(unique_queries: usize) -> f64 {
    (unique_queries as f64 / 4.0).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(id: i64, content: &str) -> MemoryEntry {
        MemoryEntry {
            id,
            kind: "fact".to_string(),
            content: content.to_string(),
            created_ms: 0,
            accessed_ms: 0,
            recall_count: 0,
            max_score: 0.0,
            promoted: false,
            metadata: policy::MemoryPolicyMetadata {
                scope: policy::MemoryScope::Household,
                sensitivity: policy::MemorySensitivity::Normal,
                spoken_policy: policy::SpokenMemoryPolicy::Allow,
            },
        }
    }

    fn test_hit(id: i64, content: &str, score: f64) -> SemanticMemoryHit {
        SemanticMemoryHit {
            entry: test_entry(id, content),
            score,
            embedding_model: "test".to_string(),
        }
    }

    /// The regression for the core bug: when lexical search already fills the
    /// limit, the top semantic hit must still survive (RRF), not be truncated.
    #[test]
    fn fusion_keeps_semantic_hit_when_lexical_fills_the_limit() {
        let lexical: Vec<MemoryEntry> = (1..=10).map(|i| test_entry(i, "common token")).collect();
        let semantic = vec![test_hit(100, "semantically relevant", 0.95)];

        let fused = fuse_recall_candidates(lexical, semantic, 10);

        assert_eq!(fused.len(), 10);
        assert!(
            fused.iter().any(|e| e.id == 100),
            "the top semantic hit must survive fusion, got ids {:?}",
            fused.iter().map(|e| e.id).collect::<Vec<_>>()
        );
        assert!(
            !fused.iter().any(|e| e.id == 10),
            "the weakest lexical hit should be displaced by the semantic hit"
        );
    }

    /// An id present in BOTH rankings has its contributions summed, so it
    /// outranks items that appear in only one list.
    #[test]
    fn fusion_boosts_ids_present_in_both_rankings() {
        let lexical = vec![test_entry(1, "a"), test_entry(2, "b"), test_entry(3, "c")];
        let semantic = vec![test_hit(3, "c", 0.9)];

        let fused = fuse_recall_candidates(lexical, semantic, 10);
        let ids: Vec<i64> = fused.iter().map(|e| e.id).collect();

        // id 3: lexical rank 3 (1/63) + semantic rank 1 (1/61) — highest fused.
        // id 1: 1/61; id 2: 1/62.
        assert_eq!(ids, vec![3, 1, 2]);
    }

    /// The same memory appearing in both lists must be de-duplicated.
    #[test]
    fn fusion_deduplicates_ids() {
        let lexical = vec![test_entry(1, "a"), test_entry(2, "b")];
        let semantic = vec![test_hit(1, "a", 0.9), test_hit(2, "b", 0.8)];

        let fused = fuse_recall_candidates(lexical, semantic, 10);

        assert_eq!(fused.len(), 2);
        let mut ids: Vec<i64> = fused.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn default_weights_sum_to_one() {
        let w = PromotionWeights::default();
        let sum = w.frequency + w.relevance + w.recency + w.consolidation + w.diversity;
        assert!((sum - 1.0).abs() < 0.001);
    }

    #[test]
    fn consolidation_scaling() {
        assert_eq!(consolidation_from_count(0), 0.0);
        assert_eq!(consolidation_from_count(1), 0.0);
        assert!(consolidation_from_count(3) > 0.3);
        assert!(consolidation_from_count(5) > 0.6);
        assert!((consolidation_from_count(10) - 1.0).abs() < 0.1);
    }

    #[test]
    fn diversity_scaling() {
        assert_eq!(diversity_from_unique_queries(0), 0.0);
        assert_eq!(diversity_from_unique_queries(1), 0.25);
        assert_eq!(diversity_from_unique_queries(2), 0.5);
        assert_eq!(diversity_from_unique_queries(4), 1.0);
        assert_eq!(diversity_from_unique_queries(10), 1.0);
    }

    #[test]
    fn dream_cycle_integration() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let id = CTR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("geniepod-dream-{}-{}.db", std::process::id(), id));
        let _ = std::fs::remove_file(&path);
        let mem = Memory::open(&path).unwrap();

        // Store and recall a memory many times.
        mem.store("fact", "GeniePod uses Nemotron 4B").unwrap();
        for _ in 0..6 {
            mem.search("Nemotron", 10).unwrap();
        }

        let weights = PromotionWeights::default();
        let (promoted, _pruned) = dream_cycle(&mem, &weights, 0.1, 3, 10, 0.01).unwrap();

        assert!(promoted >= 1, "should promote frequently recalled memory");
        assert!(mem.promoted_count().unwrap() >= 1);
    }

    #[test]
    fn filter_recall_results_respects_person_scope() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let id = CTR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-recall-filter-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        let mem = Memory::open(&path).unwrap();

        mem.store("person_preference", "Maya likes oat milk")
            .unwrap();

        let shared = recall_with_context(
            &mem,
            "oat milk",
            10,
            policy::MemoryReadContext::shared_room_voice(),
        )
        .unwrap();
        assert!(shared.is_empty());

        let identified = recall_with_context(
            &mem,
            "oat milk",
            10,
            policy::MemoryReadContext {
                identity_confidence: policy::IdentityConfidence::High,
                explicit_named_person: false,
                explicit_private_intent: false,
                shared_space_voice: true,
            },
        )
        .unwrap();
        assert_eq!(identified.len(), 1);
        assert_eq!(
            identified[0].decision.disclosure,
            policy::MemoryDisclosure::Speak
        );
    }

    #[test]
    fn recall_with_context_uses_semantic_hits_when_lexical_query_misses() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let id = CTR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-recall-semantic-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        let mem = Memory::open(&path).unwrap();
        mem.store(
            "preference",
            "Jared prefers the living room thermostat at 72F.",
        )
        .unwrap();

        let recalled = recall_with_context(
            &mem,
            "I'm feeling cold",
            10,
            policy::MemoryReadContext::shared_room_voice(),
        )
        .unwrap();

        assert_eq!(recalled.len(), 1);
        assert!(recalled[0].entry.content.contains("thermostat"));
    }
}
