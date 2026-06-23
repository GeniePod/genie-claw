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
    // Common case: a normal utterance with no relationship phrase. The old code
    // still built all 57 `format!` pattern strings here; the new code skips the
    // scan entirely via the "my " early-out.
    run(
        "no-my",
        "the weather today is nice and i went for a walk",
        300_000,
    );
    // Has "my " but no relationship match: exercises the full 57-pattern scan.
    run(
        "my-no-match",
        "my plan today is to relax and read a good book",
        300_000,
    );
    // A real relationship hit.
    run(
        "relationship",
        "my dog is named rex and we play fetch",
        300_000,
    );
}
