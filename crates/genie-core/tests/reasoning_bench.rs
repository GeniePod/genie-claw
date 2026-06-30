//! Benchmark for `apply_reasoning_mode` (Qwen path).
//!
//! Run with: `cargo test -p genie-core --release --test reasoning_bench -- --ignored --nocapture`

use genie_core::llm::Message;
use genie_core::prompt::ModelFamily;
use genie_core::reasoning::{InteractionKind, apply_reasoning_mode};
use std::hint::black_box;

fn single_user_message(text: &str) -> Vec<Message> {
    vec![Message {
        role: "user".into(),
        content: text.into(),
    }]
}

fn run(label: &str, text: &str, interaction: InteractionKind, iters: u32) {
    let messages = single_user_message(text);
    for _ in 0..500 {
        black_box(apply_reasoning_mode(
            ModelFamily::Qwen,
            &messages,
            text,
            interaction,
        ));
    }
    let start = std::time::Instant::now();
    for _ in 0..iters {
        black_box(apply_reasoning_mode(
            ModelFamily::Qwen,
            &messages,
            text,
            interaction,
        ));
    }
    let elapsed = start.elapsed();
    eprintln!(
        "BENCH apply_reasoning_mode [{label}]: {iters} calls, total {elapsed:?}, per-call {:?}",
        elapsed / iters,
    );
}

#[test]
#[ignore]
fn bench_apply_reasoning_mode() {
    run(
        "simple-voice",
        "turn on the kitchen light",
        InteractionKind::Voice,
        300_000,
    );
    run(
        "complex-chat",
        "Compare these two Rust designs, explain the tradeoffs, and recommend the safer refactor step by step.",
        InteractionKind::Chat,
        300_000,
    );
    run(
        "explicit-think",
        "debug this crash /think",
        InteractionKind::Chat,
        300_000,
    );
    run(
        "no-match",
        "tell me something interesting about the garden",
        InteractionKind::Chat,
        300_000,
    );
}
