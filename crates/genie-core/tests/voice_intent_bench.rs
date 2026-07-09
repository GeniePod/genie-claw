#![cfg(feature = "voice")]

use genie_core::voice::intent::{VoiceIntentDecision, assess_transcript};
use std::hint::black_box;

fn run(label: &str, transcript: &str, iters: u32) {
    for _ in 0..1000 {
        black_box(assess_transcript(black_box(transcript)));
    }
    let start = std::time::Instant::now();
    let mut acc = 0u8;
    for _ in 0..iters {
        acc = acc.wrapping_add(
            (black_box(assess_transcript(black_box(transcript))) == VoiceIntentDecision::Accept)
                as u8,
        );
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH assess_transcript [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_assess_transcript() {
    // Direct command accepted via the prefix table (first-byte dispatch).
    run("direct-command", "turn on the kitchen light", 300_000);
    // Accepted only via a containment marker mid-sentence (byte-gate path).
    run("marker-rescue", "make the lights warmer", 300_000);
    // Ambient narration rejected after the full scan chain.
    run(
        "ambient-reject",
        "the old house stood alone at the end of the road",
        300_000,
    );
    // Accepted with no marker at all: worst case, every gate consulted.
    run(
        "no-marker-accept",
        "i finished reading that book you mentioned yesterday evening",
        300_000,
    );
    // Messy STT whitespace and casing through the single-pass normalize.
    run(
        "messy-whitespace",
        "  TURN   on \t the\nKITCHEN light  ",
        300_000,
    );
}
