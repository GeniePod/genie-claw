use genie_core::memory::policy::assess_memory_write;
use std::hint::black_box;

fn run(label: &str, kind: &str, content: &str, iters: u32) {
    for _ in 0..1000 {
        black_box(assess_memory_write(black_box(kind), black_box(content)));
    }
    let start = std::time::Instant::now();
    let mut acc = 0u8;
    for _ in 0..iters {
        acc = acc.wrapping_add(black_box(
            assess_memory_write(black_box(kind), black_box(content)).allowed as u8,
        ));
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH assess_memory_write [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_assess_memory_write() {
    // Common allow path after auto-capture (no secret/private/cautious markers).
    // Skips the allocating content to_lowercase when triggers are absent.
    run(
        "allow-preference",
        "preference",
        "User likes hiking in the mountains",
        300_000,
    );
    // Restricted secret rejection.
    run(
        "reject-password",
        "fact",
        "my password is swordfish",
        300_000,
    );
    // Cautious health classification via infer_metadata.
    run(
        "cautious-health",
        "fact",
        "Grandma takes metformin at 8am for diabetes",
        300_000,
    );
}
