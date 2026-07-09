use genie_core::llm::Message;
use genie_core::prompt::ModelFamily;
use genie_core::reasoning::{InteractionKind, apply_reasoning_mode};
use std::hint::black_box;

fn run(label: &str, user_text: &str, interaction: InteractionKind, iters: u32) {
    let messages = vec![Message {
        role: "user".into(),
        content: user_text.into(),
    }];
    for _ in 0..1000 {
        black_box(apply_reasoning_mode(
            ModelFamily::Qwen,
            black_box(&messages),
            black_box(user_text),
            interaction,
        ));
    }
    let start = std::time::Instant::now();
    let mut acc = 0u8;
    for _ in 0..iters {
        acc = acc.wrapping_add(
            black_box(apply_reasoning_mode(
                ModelFamily::Qwen,
                black_box(&messages),
                black_box(user_text),
                interaction,
            ))
            .1
            .applied as u8,
        );
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH apply_reasoning_mode [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_apply_reasoning_mode() {
    // Common voice transcript: already lowercase ASCII, no directive, simple.
    // Skips every lowercase/replace allocation on the optimized path.
    run(
        "voice-simple",
        "turn on the kitchen lights",
        InteractionKind::Voice,
        300_000,
    );
    // Typed chat with mixed case: takes the single shared lowercase alloc.
    run(
        "chat-mixed-case",
        "What's the WEATHER like today?",
        InteractionKind::Chat,
        300_000,
    );
    // Deep-reasoning marker prompt: full marker scan plus /think adjust.
    run(
        "deep-marker",
        "compare the two thermostats and explain the tradeoffs step by step",
        InteractionKind::Chat,
        300_000,
    );
    // Long prompt (> 140 bytes): early length short-circuit in the deep scan.
    run(
        "long-prompt",
        "I want a detailed comparison of running the media server on the jetson \
         versus the nas, including power draw, thermals, and which one is easier \
         to keep updated over time.",
        InteractionKind::Chat,
        300_000,
    );
}
