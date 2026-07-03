//! Benchmark: a dream-cycle that promotes P memories should rebuild the
//! canonical MEMORY.md tree once for the whole batch, not once per promotion.
//!
//! `rebuild_root_memory_file()` scans every `promoted = 1` row and rewrites
//! MEMORY.md, INDEX.md, and every namespace file. Doing that once per promotion
//! makes a single dream cycle's file work grow ~quadratically with the number of
//! promotions; only the final on-disk state is ever observed.
//!
//! Run with:
//!   cargo test -p genie-core --release --test dream_promote_bench -- --ignored --nocapture

use genie_core::Memory;
use genie_core::memory::recall::{PromotionWeights, dream_cycle};

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_dream_cycle_batch_promote() {
    let dir = std::env::temp_dir().join(format!("genie-dream-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("mem.db");

    let mem = Memory::open(&path).expect("open");

    // Store N facts and recall each enough times to clear the promotion
    // candidate floor (min_recalls). Each fact carries a unique token so the
    // search bumps that row's recall_count.
    let n = 200usize;
    for i in 0..n {
        mem.store("fact", &format!("household bench token qzt{i} note"))
            .expect("store");
    }
    for i in 0..n {
        for _ in 0..4 {
            let _ = mem.search(&format!("qzt{i}"), 3).expect("search");
        }
    }

    let weights = PromotionWeights::default();
    let start = std::time::Instant::now();
    // min_score 0.0 so every candidate promotes; max_promotions covers all.
    let (promoted, _pruned) = dream_cycle(&mem, &weights, 0.0, 3, n, 0.0).expect("dream_cycle");
    let elapsed = start.elapsed();

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "BENCH dream_cycle_batch_promote: {n} candidates, promoted {promoted}, \
         dream_cycle {elapsed:?}"
    );
    assert!(promoted > 0, "expected promotions");
}
