#![cfg(feature = "voice")]
use genie_core::voice::format::for_voice;
use std::hint::black_box;

fn short_reply() -> String {
    "Sure! The **living room** lights are on and set to *warm white* \
     (about 2700K). Let me know if you'd like them dimmed."
        .to_string()
}

fn long_reply() -> String {
    "## Here's what I found\n\
     The thermostat is set to 72.5 degrees — comfortable for the evening. \
     Here are the highlights:\n\
     - The **kitchen** lights are off.\n\
     - The *bedroom* fan is running at low.\n\
     - See [the energy report](https://example.com/report) for details.\n\
     1. Living room: 21°C now.\n\
     2. Office: 22°C now.\n\
     Note: visit https://example.com or www.example.org to adjust schedules. \
     It's the house's quiet hour now... everything looks normal!"
        .to_string()
}

fn run(label: &str, input: &str, iters: u32) {
    for _ in 0..1000 {
        black_box(for_voice(black_box(input)));
    }
    let start = std::time::Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        acc += black_box(for_voice(black_box(input))).len();
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH for_voice [{label}]: {} bytes in, {iters} calls, total {elapsed:?}, \
         per-call {:?}",
        input.len(),
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_for_voice() {
    let short = short_reply();
    let long = long_reply();
    run("short", &short, 300_000);
    run("long", &long, 300_000);
}
