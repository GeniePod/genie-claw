use genie_core::voice::intent::assess_transcript;
use std::hint::black_box;

fn run(label: &str, input: &str, iters: u32) {
    for _ in 0..500 {
        black_box(assess_transcript(input));
    }
    let start = std::time::Instant::now();
    for _ in 0..iters {
        black_box(assess_transcript(black_box(input)));
    }
    let elapsed = start.elapsed();
    eprintln!(
        "BENCH voice_intent [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_voice_intent_assess_transcript() {
    run("home-command", "turn on the kitchen light", 300_000);
    run("question", "what time is it?", 300_000);
    run(
        "ambient-narration",
        "the old house stood alone at the end of the road",
        300_000,
    );
    run("filler", "thank you", 300_000);
}
