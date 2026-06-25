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

fn run(label: &str, input: &str, iters: u32) {
    for _ in 0..1000 {
        let _ = apply_reasoning_mode(
            ModelFamily::Qwen,
            &single_user_message(input),
            input,
            InteractionKind::Voice,
        );
    }
    let start = std::time::Instant::now();
    let mut acc = 0u8;
    for _ in 0..iters {
        let (_, decision) = apply_reasoning_mode(
            ModelFamily::Qwen,
            &single_user_message(black_box(input)),
            black_box(input),
            InteractionKind::Voice,
        );
        acc = acc.wrapping_add(decision.applied as u8);
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
    run("simple-voice", "what time is it", 300_000);
    run(
        "complex-chat",
        "compare these two rust module designs and recommend the safer refactor",
        300_000,
    );
    run("explicit-think", "debug this crash /think", 300_000);
}
