//! Benchmark for the NLMS echo-cancellation hot loop (`voice::aec::cancel_echo`).
//!
//! Ignored by default — a timing harness, not a pass/fail test. Run on-device to
//! reproduce the before→after numbers for the sliding-energy + branchless-tap
//! change:
//!
//! ```text
//! cargo test -p genie-core --release --test aec_nlms_bench -- --ignored --nocapture
//! ```
//!
//! The same file runs unchanged on `main` (full energy recompute per sample) and
//! on the perf branch (sliding-window energy), so the delta isolates that cost.
//!
//! Gated on the `voice` feature (default-on); a `--no-default-features`
//! chat-only build has no `voice` module, so this test compiles to nothing.
#![cfg(feature = "voice")]

use genie_core::voice::aec::{cancel_echo, clear_echo_reference, set_echo_reference};

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_cancel_echo() {
    let sample_rate = 16000u32;
    let n = 48_000usize; // ~3 s of 16 kHz audio.

    // Reference: 440 Hz tone (what the speaker played).
    let reference: Vec<f32> = (0..n)
        .map(|i| {
            (i as f32 / sample_rate as f32 * 440.0 * 2.0 * std::f32::consts::PI).sin() * 3000.0
        })
        .collect();
    // Mic: same 440 Hz echo + 1000 Hz speech.
    let mic_template: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let echo = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 3000.0;
            let speech = (t * 1000.0 * 2.0 * std::f32::consts::PI).sin() * 2000.0;
            echo + speech
        })
        .collect();
    let ref_pcm: Vec<u8> = reference
        .iter()
        .flat_map(|&s| (s.clamp(-32767.0, 32767.0) as i16).to_le_bytes())
        .collect();
    set_echo_reference(&ref_pcm, sample_rate);

    // Warm up.
    let mut warm = mic_template.clone();
    cancel_echo(&mut warm, sample_rate);

    let iterations = 50usize;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let mut mic = mic_template.clone();
        cancel_echo(&mut mic, sample_rate);
        std::hint::black_box(&mic);
    }
    let elapsed = start.elapsed();
    clear_echo_reference();

    eprintln!(
        "BENCH cancel_echo: {n} samples, {iterations} iters, total {elapsed:?}, per-call {:?}",
        elapsed / iterations as u32,
    );
}
