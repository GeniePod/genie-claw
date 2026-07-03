//! Benchmark: per-sentence cleanup allocation reduction in `voice::streaming`.
//!
//! Compares the refactored `clean_sentence` (12 intermediate allocations) with
//! the pre-refactor chain (14 intermediate allocations) over a corpus of
//! realistic TTS sentence inputs.  Run with:
//!
//!   cargo test -p genie-core --release -- --ignored --nocapture voice_streaming_bench

#![cfg(feature = "voice")]

use genie_core::voice::streaming::clean_sentence;
use std::hint::black_box;
use std::time::Instant;

// Verbatim pre-refactor clean_sentence for the before measurement.
#[allow(clippy::collapsible_str_replace)]
fn clean_sentence_old(text: &str) -> String {
    // strip_inline_links (unchanged — same in old and new)
    let mut stripped_links = String::with_capacity(text.len());
    {
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '[' {
                let mut link_text = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    link_text.push(c);
                }
                if closed && chars.peek() == Some(&'(') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == ')' {
                            break;
                        }
                    }
                    stripped_links.push_str(&link_text);
                } else {
                    stripped_links.push('[');
                    stripped_links.push_str(&link_text);
                    if closed {
                        stripped_links.push(']');
                    }
                }
            } else {
                stripped_links.push(ch);
            }
        }
    }

    // OLD strip_raw_urls: collect into Vec<&str> then join
    let stripped_urls = stripped_links
        .split_whitespace()
        .filter(|token| {
            let trimmed = token.trim_matches(|c: char| {
                matches!(
                    c,
                    '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '"' | '\''
                )
            });
            !(trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with("www."))
        })
        .collect::<Vec<_>>()
        .join(" ");

    // strip_list_or_header_prefix (unchanged)
    let line = stripped_urls.trim();
    let trimmed = line.trim_start_matches('#').trim_start();
    let trimmed = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
        .unwrap_or(trimmed);
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let stripped_prefix =
        if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
            trimmed[i + 2..].to_string()
        } else {
            trimmed.to_string()
        };

    // OLD: 4 separate emphasis replace() calls
    let stripped_inline = stripped_prefix
        .replace("**", "")
        .replace("__", "")
        .replace('*', "")
        .replace('`', "");

    // OLD: 7 separate punctuation replace() calls
    let punct_cleaned = stripped_inline
        .replace("...", ", ")
        .replace(" - ", ", ")
        .replace(" — ", ", ")
        .replace(" – ", ", ")
        .replace(['(', ')'], ", ")
        .replace(['[', ']', '{', '}', '"'], "")
        .replace("'s", "s");

    // collapse_whitespace
    let mut result = String::with_capacity(punct_cleaned.len());
    let mut last_was_space = false;
    for ch in punct_cleaned.trim().chars() {
        if ch.is_whitespace() {
            if !last_was_space && !result.is_empty() {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
    }
    result
}

/// A realistic corpus of TTS sentence inputs exercising markdown, URLs,
/// punctuation, list markers, and plain prose.
fn bench_corpus() -> Vec<&'static str> {
    vec![
        "**Hello**, the temperature is 72 degrees.",
        "Visit https://home-assistant.io for more details.",
        "The kitchen light — [status](http://ha.local/api) — is on.",
        "- Item one: check `config.yaml` and restart.",
        "Good morning! Your schedule for today (Tuesday) is clear.",
        "## Summary: three tasks complete, two pending.",
        "The user's preference is set to *night mode* by default.",
        "Pages 1 – 5 are ready; see www.example.org for reference.",
        "Here is the result... wait, actually [click here](https://x.com).",
        "1. First step - open the app and sign in with your credentials.",
        "Great job! The `home_control` command ran successfully.",
        "Your alarm__override__ is set for 7 AM (tomorrow morning).",
        "Check __this__ out: ***bold italic*** text with `code` block.",
        "The sensor (bedroom) reads 68°F — optimal for sleep.",
        "See https://grafana.example.com/d/abc123 for the dashboard.",
    ]
}

#[test]
#[ignore = "benchmark: cargo test --release -- --ignored --nocapture voice_streaming_bench"]
fn bench_clean_sentence_old_vs_new() {
    let corpus = bench_corpus();
    let iters = 20_000u32;

    // Warm-up pass to stabilise allocator and branch predictor.
    let mut sink = 0usize;
    for _ in 0..500 {
        for s in &corpus {
            sink += clean_sentence_old(black_box(s)).len();
            sink += clean_sentence(black_box(s)).len();
        }
    }

    // Old implementation timing.
    let t = Instant::now();
    for _ in 0..iters {
        for s in &corpus {
            sink += black_box(clean_sentence_old(black_box(s))).len();
        }
    }
    let old_ns = t.elapsed().as_nanos() as f64 / (iters as f64 * corpus.len() as f64);

    // New implementation timing.
    let t = Instant::now();
    for _ in 0..iters {
        for s in &corpus {
            sink += black_box(clean_sentence(black_box(s))).len();
        }
    }
    let new_ns = t.elapsed().as_nanos() as f64 / (iters as f64 * corpus.len() as f64);

    eprintln!(
        "BENCH clean_sentence [corpus={} sentences, iters={}]: \
         old(14 allocs) {old_ns:.0} ns/call → new(12 allocs) {new_ns:.0} ns/call \
         ({:.2}x) [sink={sink}]",
        corpus.len(),
        iters,
        old_ns / new_ns,
    );
}
