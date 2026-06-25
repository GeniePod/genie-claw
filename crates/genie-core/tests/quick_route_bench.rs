use genie_core::tools::quick;
use std::hint::black_box;

fn run(label: &str, input: &str, iters: u32) {
    for _ in 0..1000 {
        black_box(quick::route(black_box(input)));
    }
    let start = std::time::Instant::now();
    let mut acc = 0u8;
    for _ in 0..iters {
        acc = acc.wrapping_add(quick::route(black_box(input)).is_some() as u8);
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH quick::route [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_quick_route() {
    run("home-control", "turn on the living room lights", 300_000);
    run("timer", "set a timer for five minutes", 300_000);
    run("weather", "what's the weather in denver", 300_000);
    run("no-match", "tell me a story about dragons", 300_000);
}
