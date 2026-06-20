//! Benchmark for the per-turn semantic-recall hot path (`Memory::semantic_search`).
//!
//! Ignored by default — it is a timing harness, not a pass/fail test. Run it
//! on-device to reproduce the before→after numbers for storing embeddings as a
//! packed little-endian f32 BLOB instead of a JSON float array:
//!
//! ```text
//! cargo test -p genie-core --release --test semantic_recall_bench -- \
//!     --ignored --nocapture
//! ```
//!
//! The same file runs unchanged on `main` (JSON embeddings) and on the perf
//! branch (BLOB embeddings); the only difference on the timed path is how each
//! stored embedding is decoded before cosine similarity, so the delta isolates
//! that cost.

use genie_core::Memory;

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_semantic_search_recall() {
    let path = std::env::temp_dir().join(format!("genie-semantic-bench-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mem = Memory::open(&path).expect("open memory db");

    // Populate a household-sized, embeddable memory set.
    let memories = 1_000usize;
    for i in 0..memories {
        mem.store(
            "fact",
            &format!(
                "Household note {i}: the family prefers the room {} thermostat warmer in the \
                 evening and keeps grocery and lunchbox snacks stocked for school.",
                i % 12
            ),
        )
        .expect("store memory");
    }

    let queries = [
        "the family prefers a warm room in the evening",
        "thermostat temperature comfort preference",
        "shopping grocery lunchbox snacks for school",
        "what does the household keep stocked",
    ];

    // Warm the SQLite page cache so we measure steady-state recall.
    for q in &queries {
        let _ = mem.semantic_search(q, 10).expect("warm recall");
    }

    let iterations = 100usize;
    let start = std::time::Instant::now();
    let mut total_hits = 0usize;
    for i in 0..iterations {
        let hits = mem
            .semantic_search(queries[i % queries.len()], 10)
            .expect("recall");
        total_hits += hits.len();
    }
    let elapsed = start.elapsed();

    eprintln!(
        "BENCH semantic_search_recall: {memories} embedded memories, {iterations} recalls, total \
         {elapsed:?}, per-recall {:?} ({total_hits} hits)",
        elapsed / iterations as u32,
    );

    let _ = std::fs::remove_file(&path);
    assert!(total_hits > 0, "benchmark should return hits");
}
