#![cfg(feature = "voice")]
//! Behavioural + differential coverage for `detect_language_from_text`.
//!
//! The single-pass accent tally replaced ten separate full-string `matches()`
//! scans. To prove that refactor is output-identical, this file keeps a verbatim
//! copy of the pre-refactor implementation (`detect_language_reference`) and
//! asserts the optimized function agrees with it across a large deterministic
//! pseudo-random corpus, plus a set of curated behavioural cases.

use genie_core::voice::language::detect_language_from_text;

// ---------------------------------------------------------------------------
// Reference: the exact pre-optimization implementation (six Spanish + four
// German per-character `matches()` scans). `is_cjk_char` is replicated inline
// since the production one is private.
// ---------------------------------------------------------------------------

fn is_cjk_char_reference(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
    )
}

fn detect_language_reference(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.chars().any(is_cjk_char_reference) {
        return Some("zh".into());
    }

    let lower = trimmed.to_lowercase();

    let spanish_hits = [
        " el ",
        " la ",
        " los ",
        " las ",
        " un ",
        " una ",
        " por ",
        " para ",
        " gracias ",
        "hola",
        "qué",
        "como ",
        "está",
        "estoy",
        "buenos",
        "buenas",
    ]
    .iter()
    .filter(|pattern| lower.contains(**pattern))
    .count()
        + lower.matches('ñ').count()
        + lower.matches('á').count()
        + lower.matches('é').count()
        + lower.matches('í').count()
        + lower.matches('ó').count()
        + lower.matches('ú').count();

    if spanish_hits >= 2 {
        return Some("es".into());
    }

    let german_hits = [
        " der ", " die ", " das ", " und ", " nicht ", " ich ", " ist ", " wie ", " danke ",
        "hallo", "guten", "bitte",
    ]
    .iter()
    .filter(|pattern| lower.contains(**pattern))
    .count()
        + lower.matches('ä').count()
        + lower.matches('ö').count()
        + lower.matches('ü').count()
        + lower.matches('ß').count();

    if german_hits >= 2 {
        return Some("de".into());
    }

    if lower.is_ascii() {
        Some("en".into())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Curated behavioural cases.
// ---------------------------------------------------------------------------

#[test]
fn empty_and_whitespace_are_undetected() {
    assert_eq!(detect_language_from_text(""), None);
    assert_eq!(detect_language_from_text("   \n\t "), None);
}

#[test]
fn chinese_short_circuits_before_accent_scan() {
    assert_eq!(detect_language_from_text("你好世界"), Some("zh".into()));
    // CJK wins even alongside Spanish accents/markers.
    assert_eq!(detect_language_from_text(" el niño 好"), Some("zh".into()));
}

#[test]
fn spanish_detected_via_markers_and_accents() {
    // Two function-word markers.
    assert_eq!(
        detect_language_from_text("dame el control de la sala"),
        Some("es".into())
    );
    // One marker + one accent reaches the threshold of two.
    assert_eq!(
        detect_language_from_text("hola, ¿qué tal?"),
        Some("es".into())
    );
    // Accents alone (ó + í) count two.
    assert_eq!(
        detect_language_from_text("habitación frío"),
        Some("es".into())
    );
}

#[test]
fn german_detected_via_markers_and_accents() {
    // Two function-word markers (` ist ` + ` nicht `); markers need surrounding
    // spaces, so a leading word without one (e.g. "der …") would not count.
    assert_eq!(
        detect_language_from_text("das ist nicht kalt"),
        Some("de".into())
    );
    assert_eq!(
        detect_language_from_text("hallo, schließe die tür"),
        Some("de".into())
    );
}

#[test]
fn plain_ascii_defaults_to_english() {
    assert_eq!(
        detect_language_from_text("turn on the living room lights"),
        Some("en".into())
    );
}

#[test]
fn single_hit_is_not_enough() {
    // One Spanish marker only → falls through to English.
    assert_eq!(detect_language_from_text("set el timer"), Some("en".into()));
}

#[test]
fn non_ascii_without_enough_hits_is_undetected() {
    // A lone accent (one hit) and no markers, non-ascii → None.
    assert_eq!(detect_language_from_text("café"), None);
}

#[test]
fn accent_count_matches_reference_on_dense_input() {
    let dense = "ñáéíóú äöüß ñ ä";
    assert_eq!(
        detect_language_from_text(dense),
        detect_language_reference(dense)
    );
}

// ---------------------------------------------------------------------------
// Differential fuzz: optimized == reference across a deterministic corpus.
// ---------------------------------------------------------------------------

/// Tokens chosen to exercise every branch: function-word markers, accent marks,
/// CJK/kana, ascii fillers and whitespace.
const FUZZ_TOKENS: &[&str] = &[
    " el ", " la ", " los ", "una ", " por ", "hola", "qué", "está", "estoy", "buenos", " der ",
    " die ", " und ", " ich ", "hallo", "guten", "bitte", "ñ", "á", "é", "í", "ó", "ú", "ä", "ö",
    "ü", "ß", "a", "e", "i", "o", "u", "x", "z", " ", "  ", ".", "!", "好", "世", "あ", "ン",
    "café", "the", "room", "light",
];

/// A tiny deterministic LCG so the corpus is identical on every machine/run
/// (no `rand`, offline-friendly, reproducible in CI).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
}

#[test]
fn optimized_matches_reference_across_fuzz_corpus() {
    let mut rng = Lcg(0x1234_5678_9abc_def0);
    for _ in 0..20_000 {
        let token_count = (rng.next() % 8) as usize; // 0..=7 tokens
        let mut s = String::new();
        for _ in 0..token_count {
            let idx = (rng.next() >> 33) as usize % FUZZ_TOKENS.len();
            s.push_str(FUZZ_TOKENS[idx]);
        }
        assert_eq!(
            detect_language_from_text(&s),
            detect_language_reference(&s),
            "divergence on input {s:?}"
        );
    }
}
