//! Automatic fact extraction from user utterances.
//!
//! Tier 1: Pattern-based extraction (zero LLM cost, real-time).
//! Tier 2: LLM-based extraction (during dreaming, from conversation transcripts).
//!
//! Inspired by OpenClaw's auto-capture, adapted for voice-first.

use super::Memory;

/// A fact extracted from user text.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub category: String,
    pub content: String,
}

/// Extract facts from a user utterance using pattern matching (Tier 1).
///
/// Returns zero or more facts with categories:
/// - `identity`: name, age, occupation, location
/// - `preference`: likes, dislikes, favorites
/// - `relationship`: family, pets, friends
/// - `fact`: explicit "remember" requests, general statements
pub fn extract_facts(text: &str) -> Vec<ExtractedFact> {
    let mut facts = Vec::new();
    let trimmed = text.trim();

    if !needs_extract_facts_lower(text) {
        if starts_with_ascii_ci(trimmed, "remember")
            && let Some(content) = extract_remember(trimmed)
        {
            facts.push(ExtractedFact {
                category: "fact".into(),
                content,
            });
        }
        return facts;
    }

    let lower = text.to_lowercase();

    if needs_identity_scan(&lower) {
        if let Some(name) = extract_pattern(&lower, &["my name is ", "call me ", "i'm called "]) {
            facts.push(ExtractedFact {
                category: "identity".into(),
                content: format!("User's name is {}", capitalize(&name)),
            });
        }

        if let Some(age) = extract_age(&lower) {
            facts.push(ExtractedFact {
                category: "identity".into(),
                content: format!("User is {} years old", age),
            });
        }

        if let Some(job) = extract_pattern(
            &lower,
            &[
                "i work at ",
                "i work for ",
                "i'm working at ",
                "i am working at ",
            ],
        ) {
            facts.push(ExtractedFact {
                category: "identity".into(),
                content: format!("User works at {}", job),
            });
        }

        if let Some(job) = extract_pattern(
            &lower,
            &["i'm a ", "i am a ", "i work as a ", "i work as an "],
        ) && !job.starts_with("bit ")
            && !job.starts_with("lot ")
            && !job.starts_with("fan ")
        {
            facts.push(ExtractedFact {
                category: "identity".into(),
                content: format!("User is a {}", job),
            });
        }

        if let Some(loc) = extract_pattern(
            &lower,
            &["i live in ", "i'm from ", "i am from ", "i'm based in "],
        ) {
            facts.push(ExtractedFact {
                category: "identity".into(),
                content: format!("User lives in {}", loc),
            });
        }
    }

    if needs_preference_scan(&lower) {
        if let Some(pref) =
            extract_pattern(&lower, &["i like ", "i love ", "i enjoy ", "i prefer "])
            && pref.split_whitespace().count() <= 8
        {
            facts.push(ExtractedFact {
                category: "preference".into(),
                content: format!("User likes {}", pref),
            });
        }

        if let Some(pref) = extract_pattern(
            &lower,
            &["i hate ", "i dislike ", "i don't like ", "i can't stand "],
        ) && pref.split_whitespace().count() <= 8
        {
            facts.push(ExtractedFact {
                category: "preference".into(),
                content: format!("User dislikes {}", pref),
            });
        }

        if lower.contains("favo")
            && let Some(fav) = extract_favorite(&lower)
        {
            facts.push(ExtractedFact {
                category: "preference".into(),
                content: fav,
            });
        }
    }

    // Relationship patterns.
    for (relation, name) in extract_relationships(&lower) {
        facts.push(ExtractedFact {
            category: "relationship".into(),
            content: format!("User's {} is named {}", relation, capitalize(&name)),
        });
    }

    // Explicit "remember" requests.
    if starts_with_ascii_ci(trimmed, "remember")
        && let Some(content) = extract_remember(trimmed)
        && facts.is_empty()
    {
        facts.push(ExtractedFact {
            category: "fact".into(),
            content,
        });
    }

    facts
}

/// Extract facts and store them, with deduplication.
/// Returns the number of new memories stored.
pub fn extract_and_store(memory: &Memory, user_text: &str) -> usize {
    let facts = extract_facts(user_text);
    let mut stored = 0;

    for fact in facts {
        // Skip if similar memory already exists. Single-valued facts are
        // exempt: for those a similar stored value is the one being corrected,
        // so they fall through to `store_resolved`, which supersedes it rather
        // than dropping the correction as a duplicate.
        if !super::is_single_valued_fact(&fact.category, &fact.content) {
            match memory.has_similar(&fact.content) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        content = %fact.content,
                        "auto-capture deduplication check failed; skipping fact"
                    );
                    continue;
                }
            }
        }

        let policy = super::policy::assess_memory_write(&fact.category, &fact.content);
        if !policy.allowed {
            tracing::debug!(
                category = %fact.category,
                reason = policy.reason,
                "skipping auto-captured memory by policy"
            );
            continue;
        }

        match memory.store_resolved(&fact.category, &fact.content) {
            Ok(outcome) if !outcome.duplicate => {
                tracing::debug!(
                    category = %fact.category,
                    content = %fact.content,
                    replaced = outcome.replaced,
                    "auto-captured memory"
                );
                stored += 1;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    category = %fact.category,
                    content = %fact.content,
                    "auto-capture store failed"
                );
            }
        }
    }

    stored
}

// --- Pattern helpers ---

/// ASCII case-insensitive substring check for pre-lowercase trigger gates.
fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.as_bytes().windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle.bytes())
            .all(|(left, right)| left.eq_ignore_ascii_case(&right))
    })
}

/// ASCII case-insensitive prefix check that is UTF-8-safe (#634).
///
/// `&haystack[..needle.len()]` panics when `needle.len()` lands inside a
/// multi-byte codepoint (e.g. an emoji or accented char whose 8th byte is not
/// a char boundary). Comparing the leading bytes never slices a `str`, so a
/// non-ASCII utterance is simply "no match" instead of a crash. `needle` is
/// an ASCII literal at every call site.
fn starts_with_ascii_ci(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let n = needle.len();
    if bytes.len() < n {
        return false;
    }
    bytes[..n]
        .iter()
        .zip(needle.bytes())
        .all(|(left, right)| left.eq_ignore_ascii_case(&right))
}

/// True when any Tier-1 extractor needs the allocating lowercase view.
fn needs_extract_facts_lower(text: &str) -> bool {
    needs_identity_scan_ci(text)
        || needs_preference_scan_ci(text)
        || needs_relationship_scan_ci(text)
}

/// Phrases that mark where a captured value ends. A fact like "my name is X"
/// must capture only X, not the conjunction or subordinate clause that follows
/// it ("...and I love coding", "...but I hate meetings", "...who lives nearby").
///
/// Each marker is space-padded and matched as a substring, so it only fires on
/// a real word boundary — `" and "` never matches inside "android", and
/// `" or "` never matches inside "doctor".
const VALUE_BOUNDARY_MARKERS: &[&str] = &[
    " and ",
    " but ",
    " or ",
    " nor ",
    " so ",
    " yet ",
    " because ",
    " since ",
    " while ",
    " when ",
    " where ",
    " who ",
    " whom ",
    " whose ",
    " which ",
    " that ",
    " with ",
    " then ",
    " though ",
    " although ",
    " however ",
    " also ",
    " plus ",
    " too ",
];

/// Cut a captured value at the first clause boundary so trailing conjunctions
/// and subordinate clauses are not swallowed into an identity/preference fact.
///
/// `value` is expected to already be a single sentence fragment (split on
/// sentence punctuation by the caller). Returns the slice up to the earliest
/// boundary marker, right-trimmed.
fn first_clause(value: &str) -> &str {
    let mut end = value.len();
    for marker in VALUE_BOUNDARY_MARKERS {
        if let Some(pos) = value.find(marker) {
            end = end.min(pos);
        }
    }
    value[..end].trim_end()
}

fn extract_pattern(text: &str, prefixes: &[&str]) -> Option<String> {
    for prefix in prefixes {
        if let Some(rest) = text.find(prefix).map(|i| &text[i + prefix.len()..]) {
            let sentence = rest.split(['.', ',', '!', '?']).next().unwrap_or("").trim();
            let value = first_clause(sentence).trim();
            if !value.is_empty() && value.split_whitespace().count() <= 10 {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn extract_age(text: &str) -> Option<u32> {
    // "I'm 25" / "I am 25 years old" / "I'm 25 years old"
    let patterns = ["i'm ", "i am "];
    for pat in patterns {
        if let Some(rest) = text.find(pat).map(|i| &text[i + pat.len()..]) {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(age) = num.parse::<u32>()
                && (1..=120).contains(&age)
            {
                // Check it's followed by "years" or end of phrase.
                let after = &rest[num.len()..].trim_start();
                if after.is_empty()
                    || after.starts_with("years")
                    || after.starts_with("year")
                    || after.starts_with(',')
                    || after.starts_with('.')
                {
                    return Some(age);
                }
            }
        }
    }
    None
}

fn needs_identity_scan(lower: &str) -> bool {
    needs_identity_scan_ci(lower)
}

fn needs_identity_scan_ci(text: &str) -> bool {
    contains_ascii_ci(text, "my name")
        || contains_ascii_ci(text, "call me")
        || contains_ascii_ci(text, "called")
        || contains_ascii_ci(text, "i work")
        || contains_ascii_ci(text, "working at")
        || contains_ascii_ci(text, "work as")
        || contains_ascii_ci(text, "i live")
        || contains_ascii_ci(text, "i'm from")
        || contains_ascii_ci(text, "i am from")
        || contains_ascii_ci(text, "based in")
        || contains_ascii_ci(text, "i'm a")
        || contains_ascii_ci(text, "i am a")
        || contains_ascii_ci(text, "i'm ")
        || contains_ascii_ci(text, "i am ")
}

fn needs_preference_scan(lower: &str) -> bool {
    needs_preference_scan_ci(lower)
}

fn needs_preference_scan_ci(text: &str) -> bool {
    contains_ascii_ci(text, "i like")
        || contains_ascii_ci(text, "i love")
        || contains_ascii_ci(text, "i enjoy")
        || contains_ascii_ci(text, "i prefer")
        || contains_ascii_ci(text, "i hate")
        || contains_ascii_ci(text, "i dislike")
        || contains_ascii_ci(text, "i don't like")
        || contains_ascii_ci(text, "i can't stand")
        || contains_ascii_ci(text, "favo")
}

fn needs_relationship_scan_ci(text: &str) -> bool {
    contains_ascii_ci(text, "my ")
}

fn extract_favorite(text: &str) -> Option<String> {
    // "my favorite color is blue" / "my favourite food is pizza"
    let start = text.find("my favo")?;
    let rest = &text[start..];

    let is_pos = rest.find(" is ")?;
    let before_is = &rest[..is_pos];
    let after_is = rest[is_pos + 4..].trim();

    let thing = before_is
        .strip_prefix("my favorite ")
        .or_else(|| before_is.strip_prefix("my favourite "))?;

    let sentence = after_is.split(['.', ',', '!']).next().unwrap_or("").trim();
    let value = first_clause(sentence).trim();

    if value.is_empty() {
        None
    } else {
        Some(format!("User's favorite {} is {}", thing.trim(), value))
    }
}

const RELATIONSHIP_PATTERNS: &[(&str, [&str; 3])] = &[
    (
        "wife",
        [
            "my wife is named ",
            "my wife's name is ",
            "my wife is called ",
        ],
    ),
    (
        "husband",
        [
            "my husband is named ",
            "my husband's name is ",
            "my husband is called ",
        ],
    ),
    (
        "partner",
        [
            "my partner is named ",
            "my partner's name is ",
            "my partner is called ",
        ],
    ),
    (
        "son",
        ["my son is named ", "my son's name is ", "my son is called "],
    ),
    (
        "daughter",
        [
            "my daughter is named ",
            "my daughter's name is ",
            "my daughter is called ",
        ],
    ),
    (
        "mom",
        ["my mom is named ", "my mom's name is ", "my mom is called "],
    ),
    (
        "dad",
        ["my dad is named ", "my dad's name is ", "my dad is called "],
    ),
    (
        "mother",
        [
            "my mother is named ",
            "my mother's name is ",
            "my mother is called ",
        ],
    ),
    (
        "father",
        [
            "my father is named ",
            "my father's name is ",
            "my father is called ",
        ],
    ),
    (
        "brother",
        [
            "my brother is named ",
            "my brother's name is ",
            "my brother is called ",
        ],
    ),
    (
        "sister",
        [
            "my sister is named ",
            "my sister's name is ",
            "my sister is called ",
        ],
    ),
    (
        "friend",
        [
            "my friend is named ",
            "my friend's name is ",
            "my friend is called ",
        ],
    ),
    (
        "dog",
        ["my dog is named ", "my dog's name is ", "my dog is called "],
    ),
    (
        "cat",
        ["my cat is named ", "my cat's name is ", "my cat is called "],
    ),
    (
        "pet",
        ["my pet is named ", "my pet's name is ", "my pet is called "],
    ),
    (
        "child",
        [
            "my child is named ",
            "my child's name is ",
            "my child is called ",
        ],
    ),
    (
        "baby",
        [
            "my baby is named ",
            "my baby's name is ",
            "my baby is called ",
        ],
    ),
    (
        "boyfriend",
        [
            "my boyfriend is named ",
            "my boyfriend's name is ",
            "my boyfriend is called ",
        ],
    ),
    (
        "girlfriend",
        [
            "my girlfriend is named ",
            "my girlfriend's name is ",
            "my girlfriend is called ",
        ],
    ),
];

fn extract_relationships(text: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    if !text.contains("my ") {
        return results;
    }

    for (relation, patterns) in RELATIONSHIP_PATTERNS {
        for pat in patterns {
            if let Some(pos) = text.find(pat) {
                let rest = &text[pos + pat.len()..];
                let name: String = rest
                    .split(|c: char| !c.is_alphanumeric() && c != '\'')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    results.push((relation.to_string(), name));
                }
            }
        }
    }

    results
}

fn extract_remember(text: &str) -> Option<String> {
    const PREFIX: &str = "remember";
    if !starts_with_ascii_ci(text, PREFIX) {
        return None;
    }
    let rest = text[PREFIX.len()..].trim();
    let rest = rest.strip_prefix("that").unwrap_or(rest).trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_memory() -> Memory {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("geniepod-extract-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        Memory::open(&dir.join("memory.db")).unwrap()
    }

    #[test]
    fn auto_capture_stores_a_name_correction_that_shortens_the_stored_name() {
        let mem = temp_memory();
        extract_and_store(&mem, "my name is Dana");

        // The correction shares enough substrings with "Dana" that the
        // similarity pre-check used to drop it before `store_resolved` ran.
        extract_and_store(&mem, "my name is Dan");

        let identities = mem.get_by_kind("identity", 10).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].content, "User's name is Dan");
    }

    #[test]
    fn auto_capture_still_skips_an_identical_restatement() {
        let mem = temp_memory();
        extract_and_store(&mem, "my name is Dana");
        let stored = extract_and_store(&mem, "my name is Dana");

        assert_eq!(stored, 0);
        assert_eq!(mem.get_by_kind("identity", 10).unwrap().len(), 1);
    }

    #[test]
    fn starts_with_ascii_ci_is_utf8_safe_and_case_insensitive() {
        // The pre-fix `trimmed[..8]` sliced 8 bytes behind only a length
        // check, so an utterance whose 8th byte fell inside a multi-byte char
        // ("i love ✨…", byte 8 splits the sparkle) panicked. Byte comparison
        // never slices a str, so these are a clean "no match" instead.
        assert!(!starts_with_ascii_ci("i love ✨ sparkles", "remember"));
        assert!(!starts_with_ascii_ci("abcdef😀 note", "remember"));
        assert!(!starts_with_ascii_ci("hi", "remember"));
        // Genuine prefixes still match, case-insensitively.
        assert!(starts_with_ascii_ci("remember the code", "remember"));
        assert!(starts_with_ascii_ci("Remember to buy milk", "remember"));
    }

    #[test]
    fn extract_facts_does_not_panic_on_multibyte_input() {
        // Regression for #634: the "remember" fast-path must not panic on
        // non-ASCII input. Byte 8 lands inside a multi-byte codepoint in each.
        let _ = extract_facts("i love ✨ sparkles everywhere");
        let _ = extract_facts("abcdef😀 is my favorite emoji today");
        let _ = extract_facts("日本語のテキストをここに入力します");
        // A real "remember" request still extracts its content.
        let facts = extract_facts("remember the garage code is 1234");
        assert!(
            facts.iter().any(|f| f.content.contains("garage code")),
            "got: {:?}",
            facts.iter().map(|f| &f.content).collect::<Vec<_>>()
        );
    }
}
