/// Voice output formatter.
///
/// LLMs output markdown, bullet points, long paragraphs, and special characters
/// that sound terrible when spoken by TTS. This module cleans up LLM output
/// for natural-sounding voice delivery.

/// Clean LLM text for TTS output.
pub fn for_voice(text: &str) -> String {
    let mut result = text.to_string();

    // Strip markdown formatting.
    result = strip_markdown(&result);

    // Raw URLs sound terrible in TTS and add no value in spoken replies, but
    // scanning for them allocates a fresh token-joined string on every reply.
    // `strip_raw_urls` rebuilds the text via `split_whitespace`, which already
    // collapses whitespace runs and trims, so `normalize_whitespace` is a no-op
    // afterwards. When there is no raw-URL marker (the common case) skip the
    // URL scan entirely and just normalize whitespace; otherwise strip URLs and
    // let that pass do the whitespace collapse. Output is byte-identical either
    // way (the downstream sentence/`.trim()` passes absorb any edge difference).
    if has_raw_url_marker(&result) {
        result = strip_raw_urls(&result);
    } else {
        result = normalize_whitespace(&result);
    }

    // Shorten if too long for voice (>3 sentences).
    result = truncate_for_voice(&result, 3);

    // Clean up special characters that TTS handles badly.
    result = clean_for_tts(&result);

    result.trim().to_string()
}

/// Strip markdown formatting (bold, italic, headers, links, code blocks).
fn strip_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip code block markers.
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        // Skip lines inside code blocks.
        if in_code_block {
            continue;
        }

        // Strip header markers.
        let line = if trimmed.starts_with('#') {
            trimmed.trim_start_matches('#').trim()
        } else {
            trimmed
        };

        // Strip bullet points.
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
            .unwrap_or(line);

        // Strip numbered lists.
        let line = strip_numbered_prefix(line);

        if !line.is_empty() {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(line);
        }
    }

    // Strip inline formatting: **bold**, *italic*, `code`, [links](url).
    #[allow(clippy::collapsible_str_replace)]
    let result = result
        .replace("**", "")
        .replace("__", "")
        .replace('*', "")
        .replace('`', "");

    // Strip markdown links: [text](url) → text
    strip_links(&result)
}

fn strip_numbered_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;

    // Skip digits.
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }

    // Check for ". " after digits.
    if i > 0 && i < bytes.len() - 1 && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        &line[i + 2..]
    } else {
        line
    }
}

fn strip_links(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '[' {
            // Collect link text.
            let mut link_text = String::new();
            for c in chars.by_ref() {
                if c == ']' {
                    break;
                }
                link_text.push(c);
            }
            // Skip (url) part.
            if chars.peek() == Some(&'(') {
                chars.next(); // skip '('
                for c in chars.by_ref() {
                    if c == ')' {
                        break;
                    }
                }
            }
            result.push_str(&link_text);
        } else {
            result.push(ch);
        }
    }

    result
}

/// Cheap, conservative gate for [`strip_raw_urls`]: true when the text contains
/// any raw-URL marker it could act on. `strip_raw_urls` only drops tokens whose
/// (punctuation-trimmed) form *starts with* one of these prefixes, so the prefix
/// substring is always present in the text when a token would be stripped —
/// `contains` is therefore a safe superset of that decision and never skips a
/// token the scan would have removed. A false positive (e.g. `shttp://`) only
/// costs a redundant scan that still yields the correct output.
fn has_raw_url_marker(text: &str) -> bool {
    text.contains("http://") || text.contains("https://") || text.contains("www.")
}

fn strip_raw_urls(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for token in text.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '"' | '\''
            )
        });
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("www.")
        {
            continue;
        }
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(token);
    }
    result
}

/// Normalize whitespace: collapse multiple spaces, trim.
fn normalize_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
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

/// Minimum length (counted in `char`s, not bytes) a clause must reach before
/// an ASCII terminator is allowed to close it. Shorter fragments merge into the
/// following clause so TTS never receives a 1-4 char glitch like "OK!". CJK
/// terminators bypass this floor — CJK sentences are dense and conventionally
/// have no trailing space, so they always close immediately.
const MIN_SENTENCE_CHARS: usize = 5;

/// Common abbreviations whose trailing `.` is part of the token, not a sentence
/// end, even when followed by a space. Stored lowercased and without the final
/// dot; internal dots (e.g. `p.m`, `u.s.a`) are kept so the whole token matches.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "rev", "gen", "sen", "rep", "gov", "pres",
    "vs", "etc", "inc", "ltd", "co", "corp", "llc", "dept", "est", "approx", "vol", "no", "fig",
    "e.g", "i.e", "a.m", "p.m", "u.s", "u.k", "u.s.a",
];

/// Truncate to at most `max_sentences` sentences for voice output.
///
/// Boundary rules mirror `voice::streaming::SentenceStreamer`, the streaming
/// path that already segments correctly:
/// - An ASCII terminator (`.`/`!`/`?`) ends a sentence only when the next char
///   is whitespace or end-of-text. A `.` sitting between two ASCII digits is a
///   decimal point (e.g. `3.50`, `72.5`, `v1.0.0`) and never a boundary, and a
///   `.` closing a known abbreviation (`p.m.`, `Dr.`, `U.S.A.`) does not split.
/// - A CJK terminator (`。`/`！`/`？`) ends a sentence immediately.
/// - Clauses shorter than `MIN_SENTENCE_CHARS` merge into the next one.
fn truncate_for_voice(text: &str, max_sentences: usize) -> String {
    if max_sentences == 0 {
        return String::new();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    let mut sentence_count = 0;
    let mut current = String::new();

    for (i, &ch) in chars.iter().enumerate() {
        current.push(ch);

        let ends_sentence = if is_cjk_terminator(ch) {
            true
        } else if is_ascii_terminator(ch) {
            let next = chars.get(i + 1).copied();
            let prev = chars.get(i.wrapping_sub(1)).copied();
            // A '.' between two digits is a decimal point, not a boundary.
            let is_decimal = ch == '.'
                && prev.is_some_and(|c| c.is_ascii_digit())
                && next.is_some_and(|c| c.is_ascii_digit());
            // Close only when followed by whitespace/end-of-text, the clause
            // is long enough to stand alone, and it isn't a decimal or a
            // known abbreviation whose dot belongs to the word.
            !is_decimal
                && next.is_none_or(|c| c.is_whitespace())
                && current.trim().chars().count() >= MIN_SENTENCE_CHARS
                && !ends_with_abbreviation(&current)
        } else {
            false
        };

        if ends_sentence {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(current.trim());
            sentence_count += 1;
            current.clear();
            if sentence_count >= max_sentences {
                break;
            }
        }
    }

    // Include the trailing fragment if we still have room.
    let trailing = current.trim();
    if !trailing.is_empty() && sentence_count < max_sentences {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(trailing);
    }

    result
}

fn is_ascii_terminator(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?')
}

fn is_cjk_terminator(ch: char) -> bool {
    matches!(ch, '。' | '！' | '？')
}

/// True when the clause's final whitespace-delimited token is a known
/// abbreviation, so its trailing `.` should not end the sentence. Shared with
/// `voice::streaming::SentenceStreamer` so both voice paths apply the same rule.
pub(crate) fn ends_with_abbreviation(current: &str) -> bool {
    let last_token = current
        .split_whitespace()
        .next_back()
        .unwrap_or("")
        .trim_end_matches('.');
    if last_token.is_empty() {
        return false;
    }
    let lowered = last_token.to_ascii_lowercase();
    ABBREVIATIONS.contains(&lowered.as_str())
}

/// Clean special characters that TTS engines handle poorly.
fn clean_for_tts(text: &str) -> String {
    let pre = text
        .replace("...", ", ")
        .replace(" - ", ", ")
        .replace(" — ", ", ")
        .replace(" – ", ", ");

    let mut result = String::with_capacity(pre.len());
    for ch in pre.chars() {
        match ch {
            '(' | ')' => result.push_str(", "),
            '[' | ']' | '{' | '}' | '"' => {}
            _ => result.push(ch),
        }
    }

    result.replace("'s", "s")
}
