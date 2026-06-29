#![cfg(feature = "voice")]
//! Benchmark for the `detect_language_from_text` accent-scan optimization.
//!
//! Run with: `cargo test -p genie-core --release --test language_detect_bench -- --ignored --nocapture`
//!
//! The optimization replaced ten separate full-string `matches()` scans (six
//! Spanish + four German accents) with a single `chars()` pass. This bench times
//! the public function end-to-end and isolates the accent tally old-vs-new to
//! show the win on accent-dense input.

use genie_core::voice::language::detect_language_from_text;
use std::hint::black_box;

fn spanish_reply() -> String {
    "hola, ¿podrías encender las luces de la habitación? \
     está un poco frío aquí y quiero más calor por favor."
        .to_string()
}

fn english_reply() -> String {
    "could you turn on the living room lights and set the thermostat \
     to a comfortable temperature for the evening please"
        .to_string()
}

fn accent_dense() -> String {
    "ñáéíóú äöüß ñ ä é ö í ü ó ß ú á habitación frío schließen über".to_string()
}

// The pre-optimization accent tally: ten independent full-string scans.
fn accent_scan_old(lower: &str) -> usize {
    lower.matches('ñ').count()
        + lower.matches('á').count()
        + lower.matches('é').count()
        + lower.matches('í').count()
        + lower.matches('ó').count()
        + lower.matches('ú').count()
        + lower.matches('ä').count()
        + lower.matches('ö').count()
        + lower.matches('ü').count()
        + lower.matches('ß').count()
}

// The optimized accent tally: a single pass with disjoint-char counters.
fn accent_scan_new(lower: &str) -> usize {
    let mut n = 0;
    for ch in lower.chars() {
        if matches!(
            ch,
            'ñ' | 'á' | 'é' | 'í' | 'ó' | 'ú' | 'ä' | 'ö' | 'ü' | 'ß'
        ) {
            n += 1;
        }
    }
    n
}

fn run_detect(label: &str, input: &str, iters: u32) {
    for _ in 0..1000 {
        black_box(detect_language_from_text(black_box(input)));
    }
    let start = std::time::Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        acc += black_box(detect_language_from_text(black_box(input)))
            .map(|s| s.len())
            .unwrap_or(0);
    }
    let elapsed = start.elapsed();
    black_box(acc);
    eprintln!(
        "BENCH detect_language [{label}]: {} bytes, {iters} calls, total {elapsed:?}, \
         per-call {:?}",
        input.len(),
        elapsed / iters,
    );
}

fn run_accent_scan(input: &str, iters: u32) {
    let lower = input.to_lowercase();
    // Correctness guard: both tallies must agree.
    assert_eq!(accent_scan_old(&lower), accent_scan_new(&lower));

    for _ in 0..1000 {
        black_box(accent_scan_old(black_box(&lower)));
        black_box(accent_scan_new(black_box(&lower)));
    }

    let start = std::time::Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        acc += black_box(accent_scan_old(black_box(&lower)));
    }
    let old = start.elapsed();
    black_box(acc);

    let start = std::time::Instant::now();
    let mut acc = 0usize;
    for _ in 0..iters {
        acc += black_box(accent_scan_new(black_box(&lower)));
    }
    let new = start.elapsed();
    black_box(acc);

    eprintln!(
        "BENCH accent_scan [{} bytes]: old(10 scans) {old:?} vs new(1 pass) {new:?}, \
         speedup {:.2}x",
        lower.len(),
        old.as_secs_f64() / new.as_secs_f64().max(f64::MIN_POSITIVE),
    );
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_detect_language() {
    let es = spanish_reply();
    let en = english_reply();
    run_detect("spanish", &es, 300_000);
    run_detect("english", &en, 300_000);
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_accent_scan() {
    run_accent_scan(&accent_dense(), 1_000_000);
    run_accent_scan(&spanish_reply(), 1_000_000);
}
