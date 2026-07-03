//! Regression and differential-fuzz tests for the `voice::streaming`
//! per-sentence cleanup helpers after the allocation-reduction refactor.
//!
//! `clean_sentence` previously allocated ~16 intermediate strings per call
//! (Vec in `strip_raw_urls`, four chained `replace` calls for emphasis,
//! two extra `replace` calls for single-char punctuation).  The refactor
//! reduces that to 13 while producing byte-identical output.
//!
//! The `*_reference` functions below are verbatim copies of the
//! *pre-refactor* implementations.  Every fuzz assertion uses them as the
//! oracle: if the new code returns a different string for any input the
//! test fails, proving the optimisation is behaviour-preserving.

#![cfg(feature = "voice")]

use genie_core::voice::streaming::SentenceStreamer;
use genie_core::voice::streaming::{clean_sentence, strip_inline_links, strip_raw_urls};

// ── Reference implementations (verbatim pre-refactor) ────────────────────────

fn strip_raw_urls_reference(text: &str) -> String {
    text.split_whitespace()
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
        .join(" ")
}

fn strip_inline_links_reference(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
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
                result.push_str(&link_text);
            } else {
                result.push('[');
                result.push_str(&link_text);
                if closed {
                    result.push(']');
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[allow(clippy::collapsible_str_replace)]
fn clean_sentence_reference(text: &str) -> String {
    // Verbatim pre-refactor body of clean_sentence.
    let stripped_links = strip_inline_links_reference(text);
    let stripped_urls = strip_raw_urls_reference(&stripped_links);
    let stripped_prefix = {
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
        let trimmed = if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
            &trimmed[i + 2..]
        } else {
            trimmed
        };
        trimmed.to_string()
    };
    let stripped_inline = stripped_prefix
        .replace("**", "")
        .replace("__", "")
        .replace('*', "")
        .replace('`', "");
    let punct_cleaned = stripped_inline
        .replace("...", ", ")
        .replace(" - ", ", ")
        .replace(" — ", ", ")
        .replace(" – ", ", ")
        .replace(['(', ')'], ", ")
        .replace(['[', ']', '{', '}', '"'], "")
        .replace("'s", "s");
    // collapse_whitespace (reference inline)
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

// ── LCG corpus ────────────────────────────────────────────────────────────────

/// Advance the LCG and return the raw u64 state value.
fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state
}

/// Generate a deterministic pseudo-random string of up to `max_len` chars.
fn lcg_string(state: &mut u64, max_len: usize) -> String {
    let rng = lcg_next(state);
    let len = (rng >> 48) as usize % max_len + 1;
    (0..len)
        .map(|_| {
            let r = lcg_next(state);
            // Printable ASCII + key markdown/URL characters.
            let choices: &[u8] =
                b" abcdefghijklmnopqrstuvwxyzABCDEFGH0123456789*`[](){}\".__-/:;,'!?";
            choices[(r >> 40) as usize % choices.len()] as char
        })
        .collect()
}

fn lcg_corpus(count: usize, max_len: usize) -> Vec<String> {
    let mut state: u64 = 0x517cc1b727220a95;
    (0..count)
        .map(|_| lcg_string(&mut state, max_len))
        .collect()
}

// ── Differential fuzz: strip_raw_urls ────────────────────────────────────────

#[test]
fn strip_raw_urls_matches_reference_plain() {
    for input in lcg_corpus(10_000, 120) {
        let expected = strip_raw_urls_reference(&input);
        let actual = strip_raw_urls(&input);
        assert_eq!(actual, expected, "strip_raw_urls mismatch for {:?}", &input);
    }
}

#[test]
fn strip_raw_urls_matches_reference_with_urls() {
    let url_fragments = [
        "https://example.com/path",
        "http://foo.bar",
        "www.site.org",
        "https://x.co/a?b=c&d=e",
        "(https://example.com)",
        "[www.foo.com]",
    ];
    let mut state: u64 = 0xdeadbeefcafe1234;
    for _ in 0..5_000 {
        let base = lcg_string(&mut state, 60);
        let idx = (lcg_next(&mut state) >> 50) as usize % url_fragments.len();
        let url = url_fragments[idx];
        let input = format!("{base} {url} {}", lcg_string(&mut state, 30));
        let expected = strip_raw_urls_reference(&input);
        let actual = strip_raw_urls(&input);
        assert_eq!(
            actual, expected,
            "strip_raw_urls URL-inject mismatch for {:?}",
            &input
        );
    }
}

// ── Differential fuzz: strip_inline_links ────────────────────────────────────

#[test]
fn strip_inline_links_matches_reference_fuzz() {
    let link_inserts = [
        "[label](https://example.com)",
        "[](url)",
        "[text with spaces](http://x.y)",
        "[unclosed bracket",
        "[no-url]text",
        "not [a link just [nested",
    ];
    let mut state: u64 = 0x1234abcd5678ef90;
    for _ in 0..8_000 {
        let base = lcg_string(&mut state, 50);
        let idx = (lcg_next(&mut state) >> 50) as usize % link_inserts.len();
        let link = link_inserts[idx];
        let input = format!("{base}{link}{}", lcg_string(&mut state, 30));
        let expected = strip_inline_links_reference(&input);
        let actual = strip_inline_links(&input);
        assert_eq!(
            actual, expected,
            "strip_inline_links mismatch for {:?}",
            &input
        );
    }
}

// ── Differential fuzz: clean_sentence ────────────────────────────────────────

#[test]
fn clean_sentence_matches_reference_plain_fuzz() {
    for input in lcg_corpus(10_000, 100) {
        let expected = clean_sentence_reference(&input);
        let actual = clean_sentence(&input);
        assert_eq!(actual, expected, "clean_sentence mismatch for {:?}", &input);
    }
}

#[test]
fn clean_sentence_matches_reference_markdown_fuzz() {
    let markdown_inserts = [
        "**bold**",
        "*italic*",
        "`code`",
        "__underline__",
        "***triple***",
        "**nested *mixed* bold**",
        "`backtick with **bold** inside`",
        "__under__ and **bold**",
    ];
    let mut state: u64 = 0xfedcba9876543210;
    for _ in 0..6_000 {
        let before = lcg_string(&mut state, 30);
        let idx = (lcg_next(&mut state) >> 50) as usize % markdown_inserts.len();
        let md = markdown_inserts[idx];
        let after = lcg_string(&mut state, 30);
        let input = format!("{before} {md} {after}");
        let expected = clean_sentence_reference(&input);
        let actual = clean_sentence(&input);
        assert_eq!(
            actual, expected,
            "clean_sentence markdown mismatch for {:?}",
            &input
        );
    }
}

#[test]
fn clean_sentence_matches_reference_punctuation_fuzz() {
    let punct_inserts = [
        "...",
        " - ",
        " — ",
        " – ",
        "(parenthetical)",
        "[bracket]",
        "{brace}",
        "\"quoted\"",
        "user's choice",
        "it's fine",
    ];
    let mut state: u64 = 0x0f1e2d3c4b5a6978;
    for _ in 0..6_000 {
        let before = lcg_string(&mut state, 25);
        let idx = (lcg_next(&mut state) >> 50) as usize % punct_inserts.len();
        let punct = punct_inserts[idx];
        let after = lcg_string(&mut state, 25);
        let input = format!("{before}{punct}{after}");
        let expected = clean_sentence_reference(&input);
        let actual = clean_sentence(&input);
        assert_eq!(
            actual, expected,
            "clean_sentence punct mismatch for {:?}",
            &input
        );
    }
}

#[test]
fn clean_sentence_matches_reference_combined_fuzz() {
    // Mix markdown, URLs, punctuation, and list prefixes together.
    let templates = [
        "**{word}** — see https://example.com for [{link}](http://foo.bar).",
        "- {word} (context) with user's `{word}` data.",
        "## {word}... the answer is [{link}](url) — done.",
        "{word} [note] and (aside) with www.example.org link.",
        "* {word}__suffix__: see https://x.com and `code`.",
        "1. {word}'s result — ([ref](http://bar.baz)) confirmed.",
    ];
    let words = ["hello", "test", "value", "item", "result", "data", "answer"];
    let mut state: u64 = 0xa1b2c3d4e5f60718;
    for _ in 0..4_000 {
        let tidx = (lcg_next(&mut state) >> 50) as usize % templates.len();
        let tmpl = templates[tidx];
        let widx = (lcg_next(&mut state) >> 50) as usize % words.len();
        let word = words[widx];
        let lidx = (lcg_next(&mut state) >> 50) as usize % words.len();
        let link = words[lidx];
        let input = tmpl.replace("{word}", word).replace("{link}", link);
        let expected = clean_sentence_reference(&input);
        let actual = clean_sentence(&input);
        assert_eq!(
            actual, expected,
            "clean_sentence combined mismatch for {:?}",
            &input
        );
    }
}

// ── Curated edge-case regression tests ───────────────────────────────────────

#[test]
fn clean_sentence_plain_text() {
    assert_eq!(clean_sentence("Hello world"), "Hello world");
}

#[test]
fn clean_sentence_strips_bold() {
    assert_eq!(clean_sentence("**bold text**"), "bold text");
}

#[test]
fn clean_sentence_strips_italic() {
    assert_eq!(clean_sentence("*italic*"), "italic");
}

#[test]
fn clean_sentence_strips_backtick() {
    assert_eq!(clean_sentence("`code`"), "code");
}

#[test]
fn clean_sentence_strips_double_underscore() {
    assert_eq!(clean_sentence("__text__"), "text");
}

#[test]
fn clean_sentence_strips_mixed_emphasis() {
    // ***bold italic*** — all asterisks removed.
    assert_eq!(clean_sentence("***bold italic***"), "bold italic");
}

#[test]
fn clean_sentence_strips_inline_link() {
    // [label](url) → label
    assert_eq!(
        clean_sentence("[click here](https://example.com)"),
        "click here"
    );
}

#[test]
fn clean_sentence_strips_raw_url_https() {
    assert_eq!(clean_sentence("visit https://example.com now"), "visit now");
}

#[test]
fn clean_sentence_strips_raw_url_www() {
    assert_eq!(clean_sentence("see www.foo.org"), "see");
}

#[test]
fn clean_sentence_strips_raw_url_http() {
    assert_eq!(clean_sentence("go to http://example.com/path"), "go to");
}

#[test]
fn clean_sentence_replaces_ellipsis() {
    assert_eq!(clean_sentence("wait... yes"), "wait, yes");
}

#[test]
fn clean_sentence_replaces_hyphen_spaced() {
    assert_eq!(clean_sentence("go - stop"), "go, stop");
}

#[test]
fn clean_sentence_replaces_em_dash() {
    assert_eq!(clean_sentence("great — done"), "great, done");
}

#[test]
fn clean_sentence_replaces_en_dash() {
    assert_eq!(clean_sentence("pages 1 – 5"), "pages 1, 5");
}

#[test]
fn clean_sentence_replaces_parens() {
    // Matches old behaviour: each '(' or ')' becomes ", ".
    assert_eq!(
        clean_sentence_reference("hello (world)"),
        clean_sentence("hello (world)")
    );
}

#[test]
fn clean_sentence_removes_square_brackets() {
    assert_eq!(clean_sentence("a [note] here"), "a note here");
}

#[test]
fn clean_sentence_removes_curly_braces() {
    assert_eq!(clean_sentence("value {x}"), "value x");
}

#[test]
fn clean_sentence_removes_double_quotes() {
    // Matches old behaviour exactly.
    assert_eq!(
        clean_sentence_reference("he said \"hello\""),
        clean_sentence("he said \"hello\"")
    );
}

#[test]
fn clean_sentence_possessive() {
    assert_eq!(clean_sentence("user's data"), "users data");
}

#[test]
fn clean_sentence_strips_bullet() {
    assert_eq!(clean_sentence("- item one"), "item one");
}

#[test]
fn clean_sentence_strips_numbered_list() {
    assert_eq!(clean_sentence("1. first item"), "first item");
}

#[test]
fn clean_sentence_strips_header() {
    assert_eq!(clean_sentence("## Title"), "Title");
}

#[test]
fn clean_sentence_collapses_whitespace() {
    assert_eq!(clean_sentence("a  b   c"), "a b c");
}

#[test]
fn clean_sentence_trims_leading_trailing() {
    assert_eq!(clean_sentence("  hello  "), "hello");
}

#[test]
fn clean_sentence_empty_string() {
    assert_eq!(clean_sentence(""), "");
}

#[test]
fn clean_sentence_only_whitespace() {
    assert_eq!(clean_sentence("   "), "");
}

#[test]
fn clean_sentence_only_url() {
    // URL-only input — all tokens removed → empty output.
    assert_eq!(clean_sentence("https://example.com"), "");
}

#[test]
fn clean_sentence_url_at_start() {
    assert_eq!(
        clean_sentence("https://foo.com rest of text"),
        "rest of text"
    );
}

#[test]
fn clean_sentence_url_at_end() {
    assert_eq!(clean_sentence("some text https://bar.com"), "some text");
}

#[test]
fn clean_sentence_url_in_middle() {
    assert_eq!(
        clean_sentence("before https://mid.example.com after"),
        "before after"
    );
}

#[test]
fn strip_raw_urls_empty() {
    assert_eq!(strip_raw_urls(""), "");
}

#[test]
fn strip_raw_urls_no_urls() {
    assert_eq!(strip_raw_urls("hello world"), "hello world");
}

#[test]
fn strip_raw_urls_only_url() {
    assert_eq!(strip_raw_urls("https://example.com"), "");
}

#[test]
fn strip_raw_urls_multiple_urls() {
    assert_eq!(strip_raw_urls("a https://x.com b http://y.com c"), "a b c");
}

#[test]
fn strip_raw_urls_preserves_non_url_tokens() {
    let input = "check this out: http://foo.bar amazing stuff";
    let expected = strip_raw_urls_reference(input);
    assert_eq!(strip_raw_urls(input), expected);
}

#[test]
fn strip_inline_links_no_links() {
    assert_eq!(strip_inline_links("hello world"), "hello world");
}

#[test]
fn strip_inline_links_single() {
    assert_eq!(strip_inline_links("[click](url)"), "click");
}

#[test]
fn strip_inline_links_multiple() {
    assert_eq!(strip_inline_links("[a](x) and [b](y)"), "a and b");
}

#[test]
fn strip_inline_links_unclosed_bracket() {
    // Unclosed bracket passed through as-is.
    assert_eq!(strip_inline_links("[unclosed"), "[unclosed");
}

#[test]
fn strip_inline_links_bracket_no_paren() {
    // [text] with no following '(' is not a link.
    assert_eq!(strip_inline_links("[text] rest"), "[text] rest");
}

// ── SentenceStreamer integration ──────────────────────────────────────────────

fn stream_all(chunks: &[&str]) -> Vec<String> {
    let mut s = SentenceStreamer::new(10);
    let mut out: Vec<String> = chunks.iter().flat_map(|c| s.feed(c)).collect();
    out.extend(s.finish());
    out
}

#[test]
fn streamer_emits_cleaned_sentence() {
    let out = stream_all(&["**Hello**! Visit https://example.com for help."]);
    assert!(!out.is_empty(), "should emit at least one sentence");
    let joined = out.join(" ");
    assert!(!joined.contains('*'), "bold markers removed: {:?}", out);
    assert!(!joined.contains("https://"), "URL removed: {:?}", out);
}

#[test]
fn streamer_chunked_matches_whole() {
    let full = "**Bold** — see https://x.com (note). Next sentence!";
    let out_full = stream_all(&[full]);
    let out_chunked = stream_all(&[
        "**Bold** — see ",
        "https://x.com (note). ",
        "Next sentence!",
    ]);
    assert_eq!(
        out_full, out_chunked,
        "chunked vs whole must match: {:?} vs {:?}",
        out_chunked, out_full
    );
}

#[test]
fn streamer_markdown_link_extracted() {
    let out = stream_all(&["Visit [GitHub](https://github.com) today."]);
    assert!(!out.is_empty());
    assert!(out[0].contains("GitHub"), "link text kept: {:?}", out[0]);
    assert!(
        !out[0].contains("https://"),
        "URL removed from link: {:?}",
        out[0]
    );
}
