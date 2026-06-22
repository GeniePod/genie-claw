//! Benchmark for `Memory::open`, which re-derives every derived table (profiles,
//! aliases, rules, calendar, shopping, inventory, embeddings, …) from the
//! canonical `memories` rows on each open.
//!
//! Ignored by default — a timing harness, not a pass/fail test. Run on-device to
//! reproduce the before→after numbers for batching the rebuild pass into one
//! transaction:
//!
//! ```text
//! cargo test -p genie-core --release --test memory_open_bench -- --ignored --nocapture
//! ```
//!
//! The same file runs unchanged on `main` (per-row auto-commit rebuilds) and on
//! the perf branch (single rebuild transaction), so the delta isolates that cost.

use genie_core::Memory;

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_memory_open_rebuild() {
    // Own subdirectory so the per-db canonical "memory" dir is fresh and owned
    // by this run.
    let dir = std::env::temp_dir().join(format!("genie-open-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("mem.db");

    // Seed a household-sized memory store once (not timed). store() maintains the
    // derived tables incrementally; the full re-derivation happens on open().
    let memories = 2_000usize;
    {
        let mem = Memory::open(&path).expect("seed open");
        for i in 0..memories {
            mem.store(
                "fact",
                &format!(
                    "Household note {i}: the family keeps the room {} thermostat warm in the \
                     evening and restocks grocery and lunchbox snacks for school.",
                    i % 12
                ),
            )
            .expect("store");
        }
    }

    // Warm the OS page cache for the db file.
    {
        let _m = Memory::open(&path).expect("warm open");
    }

    let iterations = 10usize;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let m = Memory::open(&path).expect("open");
        std::hint::black_box(&m);
    }
    let elapsed = start.elapsed();

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "BENCH memory_open_rebuild: {memories} memories, {iterations} opens, total {elapsed:?}, \
         per-open {:?}",
        elapsed / iterations as u32,
    );
}
