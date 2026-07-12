//! Benchmark: `canon_home_control_action` (reached here through the public
//! `canonicalize_household_action`) canonicalizes the action verb on every
//! `home_control` dispatch — both the LLM tool-call path and the deterministic
//! quick-router path. It used to run `raw.trim().to_lowercase().replace([' ',
//! '-'], "_")` unconditionally, allocating two `String`s per call. In the common
//! case the emitter already hands over a canonical verb (`turn_off`,
//! `set_brightness`), so the normalization is a no-op and both allocations are
//! pure overhead. The fast path matches canonical-shape input directly and skips
//! them; only the uncommon natural-language forms ("turn off") still allocate.
//!
//! This times the canonical (fast) case against the natural-language (slow)
//! case, so a single run shows the per-call cost the allocation-skip removes.
//!
//! Run with:
//!   cargo test -p genie-core --release --test home_action_canon_bench -- --ignored --nocapture

use genie_core::tools::home_action::canonicalize_household_action;
use std::hint::black_box;

fn run(label: &str, action: &str, iters: u32) {
    // Warm.
    for _ in 0..2000 {
        black_box(canonicalize_household_action(black_box(action), None));
    }
    let start = std::time::Instant::now();
    let mut acc = 0u64;
    for _ in 0..iters {
        if canonicalize_household_action(black_box(action), None).is_some() {
            acc += 1;
        }
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH canon_home_control_action [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_canon_home_control_action() {
    // Common case: emitter already sends a canonical verb — fast path, no alloc.
    run("canonical-turn_off", "turn_off", 500_000);
    run("canonical-set_brightness", "set_brightness", 500_000);
    // Canonical-shape synonym — still fast path (no separators/casing).
    run("synonym-switch_off", "switch_off", 500_000);
    // Uncommon natural-language forms — slow path, two allocations each.
    run("nl-turn off", "turn off", 500_000);
    run("nl-Turn-Off", "Turn-Off", 500_000);
}
