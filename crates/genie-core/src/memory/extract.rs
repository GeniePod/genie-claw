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
    if trimmed.len() >= 8
        && trimmed[..8].eq_ignore_ascii_case("remember")
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
        // Skip if similar memory already exists.
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
    lower.contains("my name")
        || lower.contains("call me")
        || lower.contains("called")
        || lower.contains("i work")
        || lower.contains("working at")
        || lower.contains("work as")
        || lower.contains("i live")
        || lower.contains("i'm from")
        || lower.contains("i am from")
        || lower.contains("based in")
        || lower.contains("i'm a")
        || lower.contains("i am a")
        || lower.contains("i'm ")
        || lower.contains("i am ")
}

fn needs_preference_scan(lower: &str) -> bool {
    lower.contains("i like")
        || lower.contains("i love")
        || lower.contains("i enjoy")
        || lower.contains("i prefer")
        || lower.contains("i hate")
        || lower.contains("i dislike")
        || lower.contains("i don't like")
        || lower.contains("i can't stand")
        || lower.contains("favo")
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
    if text.len() < PREFIX.len() || !text[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
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
