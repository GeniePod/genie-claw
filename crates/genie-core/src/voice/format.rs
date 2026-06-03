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

    // Raw URLs sound terrible in TTS and add no value in spoken replies.
    result = strip_raw_urls(&result);

    // Normalize whitespace.
    result = normalize_whitespace(&result);

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

fn strip_raw_urls(text: &str) -> String {
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
    let mut sentences = Vec::new();
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
            sentences.push(current.trim().to_string());
            current.clear();
            if sentences.len() >= max_sentences {
                break;
            }
        }
    }

    // Include the trailing fragment if we still have room.
    let trailing = current.trim().to_string();
    if !trailing.is_empty() && sentences.len() < max_sentences {
        sentences.push(trailing);
    }

    sentences.join(" ")
}

fn is_ascii_terminator(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?')
}

fn is_cjk_terminator(ch: char) -> bool {
    matches!(ch, '。' | '！' | '？')
}

/// True when the clause's final whitespace-delimited token is a known
/// abbreviation, so its trailing `.` should not end the sentence.
fn ends_with_abbreviation(current: &str) -> bool {
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
    text.replace("...", ", ")
        .replace(" - ", ", ")
        .replace(" — ", ", ")
        .replace(" – ", ", ")
        .replace("(", ", ")
        .replace(")", ", ")
        .replace("[", "")
        .replace("]", "")
        .replace("{", "")
        .replace("}", "")
        .replace("\"", "")
        .replace("'s", "s") // possessive sounds weird with some TTS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_bold_and_italic() {
        assert_eq!(
            for_voice("This is **bold** and *italic*"),
            "This is bold and italic"
        );
    }

    #[test]
    fn strip_markdown_headers() {
        assert_eq!(for_voice("## Weather\nIt's sunny."), "Weather Its sunny.");
    }

    #[test]
    fn strip_bullet_points() {
        let input = "Here's what I found:\n- Item one\n- Item two\n- Item three";
        let output = for_voice(input);
        assert!(output.contains("Item one"));
        assert!(!output.contains("- "));
    }

    #[test]
    fn strip_code_blocks() {
        let input = "Here's the code:\n```\nlet x = 5;\n```\nThat's it.";
        let output = for_voice(input);
        assert!(!output.contains("let x"));
        assert!(output.contains("Thats it"));
    }

    #[test]
    fn strip_links() {
        let input = "Check [this guide](https://example.com) for details.";
        let output = for_voice(input);
        assert!(output.contains("this guide"));
        assert!(!output.contains("https://"));
    }

    #[test]
    fn strip_raw_urls_from_plain_text() {
        let input = "Top result: ESP32-C6 supports Thread. https://example.com/thread";
        let output = for_voice(input);
        assert!(output.contains("ESP32-C6 supports Thread"));
        assert!(!output.contains("https://"));
    }

    #[test]
    fn truncate_long_response() {
        let input =
            "First sentence. Second sentence. Third sentence. Fourth sentence. Fifth sentence.";
        let output = for_voice(input);
        assert!(output.contains("First"));
        assert!(output.contains("Third"));
        assert!(!output.contains("Fourth"));
    }

    #[test]
    fn truncate_handles_chinese_punctuation() {
        let input = "第一句。第二句！第三句？第四句。";
        let output = for_voice(input);
        assert!(output.contains("第一句"));
        assert!(output.contains("第三句"));
        assert!(!output.contains("第四句"));
    }

    #[test]
    fn clean_special_chars() {
        let input = "The temperature is 72°F (about 22°C)...nice!";
        let output = for_voice(input);
        assert!(!output.contains("("));
        assert!(!output.contains("..."));
    }

    #[test]
    fn empty_input() {
        assert_eq!(for_voice(""), "");
    }

    #[test]
    fn already_clean() {
        assert_eq!(for_voice("The lights are on."), "The lights are on.");
    }

    #[test]
    fn decimal_numbers_are_not_split() {
        let output = for_voice("It is 72.5 degrees outside and feels warm.");
        assert!(
            output.contains("72.5"),
            "decimal must stay intact, got {output:?}"
        );
        assert!(
            !output.contains("72. 5"),
            "decimal must not be split, got {output:?}"
        );
    }

    #[test]
    fn version_strings_are_not_split() {
        let output = for_voice("Version v1.0.0 is installed and ready.");
        assert!(
            output.contains("v1.0.0"),
            "version dots must stay intact, got {output:?}"
        );
    }

    #[test]
    fn abbreviations_do_not_end_a_sentence() {
        let output = for_voice("Dinner is at 6 p.m. today in the kitchen.");
        // "p.m." must survive intact — no split at "p." or "p.m. " mid-clause.
        assert!(
            output.contains("p.m."),
            "abbreviation must stay intact, got {output:?}"
        );
        assert!(
            !output.contains("p. m."),
            "abbreviation must not be split, got {output:?}"
        );
        assert!(
            output.contains("today in the kitchen"),
            "clause after the abbreviation must remain, got {output:?}"
        );
    }

    #[test]
    fn cap_counts_real_sentences_not_decimal_fragments() {
        // Before the fix, "3.50" split into two fake sentences and burned the
        // 3-sentence budget, dropping "All systems normal." entirely.
        let output =
            for_voice("The CPU is at 3.50 GHz now. Memory usage is fine. All systems normal.");
        assert!(output.contains("3.50 GHz"), "got {output:?}");
        assert!(output.contains("Memory usage is fine"), "got {output:?}");
        assert!(
            output.contains("All systems normal"),
            "the third real sentence must not be dropped, got {output:?}"
        );
    }

    #[test]
    fn chinese_punctuation_still_segments_immediately() {
        // CJK terminators have no trailing space; they must still split.
        let output = for_voice("第一句。第二句！第三句？第四句。");
        assert!(output.contains("第一句"));
        assert!(output.contains("第三句"));
        assert!(!output.contains("第四句"));
    }
}
