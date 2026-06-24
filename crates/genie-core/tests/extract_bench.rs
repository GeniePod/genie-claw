use genie_core::memory::extract::extract_facts;
use std::hint::black_box;

fn run(label: &str, input: &str, iters: u32) {
    for _ in 0..1000 {
        black_box(extract_facts(black_box(input)));
    }
    let start = std::time::Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        acc += black_box(extract_facts(black_box(input))).len();
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH extract_facts [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_extract_facts() {
    // Common case: no relationship, identity, or preference phrase — skips all
    // prefix scans after the single to_lowercase (see #495 early-outs).
    run(
        "no-match",
        "the weather today is nice and i went for a walk",
        300_000,
    );
    // Has "my " but no relationship match: exercises the 57-pattern scan only.
    run(
        "my-no-match",
        "my plan today is to relax and read a good book",
        300_000,
    );
    // Real relationship hit.
    run(
        "relationship",
        "my dog is named rex and we play fetch",
        300_000,
    );
    // Identity phrases without "my " or preference markers.
    run(
        "identity-hit",
        "i work at google and i live in seattle",
        300_000,
    );
    // Preference phrases only.
    run(
        "preference-hit",
        "i love hiking and i hate cold mornings",
        300_000,
    );
}
