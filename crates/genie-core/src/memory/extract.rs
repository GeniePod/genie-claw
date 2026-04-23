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
    let lower = text.to_lowercase();
    let trimmed = text.trim();

    // Identity patterns.
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
    ) {
        if !job.starts_with("bit ") && !job.starts_with("lot ") && !job.starts_with("fan ") {
            facts.push(ExtractedFact {
                category: "identity".into(),
                content: format!("User is a {}", job),
            });
        }
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

    // Preference patterns.
    if let Some(pref) = extract_pattern(&lower, &["i like ", "i love ", "i enjoy ", "i prefer "]) {
        if pref.split_whitespace().count() <= 8 {
            facts.push(ExtractedFact {
                category: "preference".into(),
                content: format!("User likes {}", pref),
            });
        }
    }

    if let Some(pref) = extract_pattern(
        &lower,
        &["i hate ", "i dislike ", "i don't like ", "i can't stand "],
    ) {
        if pref.split_whitespace().count() <= 8 {
            facts.push(ExtractedFact {
                category: "preference".into(),
                content: format!("User dislikes {}", pref),
            });
        }
    }

    if let Some(fav) = extract_favorite(&lower) {
        facts.push(ExtractedFact {
            category: "preference".into(),
            content: fav,
        });
    }

    // Relationship patterns.
    for (relation, name) in extract_relationships(&lower) {
        facts.push(ExtractedFact {
            category: "relationship".into(),
            content: format!("User's {} is named {}", relation, capitalize(&name)),
        });
    }

    // Explicit "remember" requests.
    if let Some(content) = extract_remember(trimmed) {
        // Only add if not already captured by a more specific pattern above.
        if facts.is_empty() {
            facts.push(ExtractedFact {
                category: "fact".into(),
                content,
            });
        }
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
            Err(_) => continue,
        }

        if let Ok(outcome) = memory.store_resolved(&fact.category, &fact.content) {
            if !outcome.duplicate {
                tracing::debug!(
                    category = %fact.category,
                    content = %fact.content,
                    replaced = outcome.replaced,
                    "auto-captured memory"
                );
                stored += 1;
            }
        }
    }

    stored
}

// --- Pattern helpers ---

fn extract_pattern(text: &str, prefixes: &[&str]) -> Option<String> {
    for prefix in prefixes {
        if let Some(rest) = text.find(prefix).map(|i| &text[i + prefix.len()..]) {
            let value = rest
                .split(|c: char| c == '.' || c == ',' || c == '!' || c == '?')
                .next()
                .unwrap_or("")
                .trim();
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
            if let Ok(age) = num.parse::<u32>() {
                if (1..=120).contains(&age) {
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
    }
    None
}

fn extract_favorite(text: &str) -> Option<String> {
    // "my favorite color is blue" / "my favourite food is pizza"
    let start = text.find("my favo").or_else(|| text.find("my favo"))?;
    let rest = &text[start..];

    // Find "is" after "favorite X"
    let is_pos = rest.find(" is ")?;
    let before_is = &rest[..is_pos]; // "my favorite color"
    let after_is = rest[is_pos + 4..].trim();

    let thing = before_is
        .replace("my favorite ", "")
        .replace("my favourite ", "");

    let value = after_is
        .split(|c: char| c == '.' || c == ',' || c == '!')
        .next()
        .unwrap_or("")
        .trim();

    if !thing.is_empty() && !value.is_empty() {
        Some(format!("User's favorite {} is {}", thing.trim(), value))
    } else {
        None
    }
}

fn extract_relationships(text: &str) -> Vec<(String, String)> {
    let relations = [
        "wife",
        "husband",
        "partner",
        "son",
        "daughter",
        "mom",
        "dad",
        "mother",
        "father",
        "brother",
        "sister",
        "friend",
        "dog",
        "cat",
        "pet",
        "child",
        "baby",
        "boyfriend",
        "girlfriend",
    ];

    let mut results = Vec::new();

    for relation in relations {
        let patterns = [
            format!("my {} is named ", relation),
            format!("my {}'s name is ", relation),
            format!("my {} is called ", relation),
        ];

        for pat in &patterns {
            if let Some(pos) = text.find(pat.as_str()) {
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
    let lower = text.to_lowercase();
    if lower.starts_with("remember") {
        let rest = text["remember".len()..].trim();
        let rest = rest.strip_prefix("that").unwrap_or(rest).trim();
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    None
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

    #[test]
    fn extract_name() {
        let facts = extract_facts("My name is Jared");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "identity");
        assert!(facts[0].content.contains("Jared"));
    }

    #[test]
    fn extract_name_call_me() {
        let facts = extract_facts("Call me Alex");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "identity");
        assert!(facts[0].content.contains("Alex"));
    }

    #[test]
    fn extract_age() {
        let facts = extract_facts("I'm 25 years old");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "identity");
        assert!(facts[0].content.contains("25"));
    }

    #[test]
    fn extract_job() {
        let facts = extract_facts("I work at TrioSpace");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "identity");
        assert!(facts[0].content.to_lowercase().contains("triospace"));
    }

    #[test]
    fn extract_occupation() {
        let facts = extract_facts("I'm a software engineer");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "identity");
        assert!(facts[0].content.contains("software engineer"));
    }

    #[test]
    fn extract_location() {
        let facts = extract_facts("I live in Denver");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "identity");
        assert!(facts[0].content.to_lowercase().contains("denver"));
    }

    #[test]
    fn extract_preference_like() {
        let facts = extract_facts("I love spicy food");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "preference");
        assert!(facts[0].content.contains("spicy food"));
    }

    #[test]
    fn extract_preference_dislike() {
        let facts = extract_facts("I hate cold weather");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "preference");
        assert!(facts[0].content.contains("cold weather"));
    }

    #[test]
    fn extract_favorite() {
        let facts = extract_facts("My favorite color is blue");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "preference");
        assert!(facts[0].content.contains("blue"));
    }

    #[test]
    fn extract_relationship() {
        let facts = extract_facts("My dog is named Rex");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "relationship");
        assert!(facts[0].content.contains("Rex"));
    }

    #[test]
    fn extract_multiple_facts() {
        let facts = extract_facts("My name is Jared and I love coding");
        assert!(facts.len() >= 2);
        assert!(facts.iter().any(|f| f.category == "identity"));
        assert!(facts.iter().any(|f| f.category == "preference"));
    }

    #[test]
    fn extract_nothing() {
        let facts = extract_facts("What time is it?");
        assert!(facts.is_empty());
    }

    #[test]
    fn extract_nothing_from_question() {
        let facts = extract_facts("Can you help me?");
        assert!(facts.is_empty());
    }

    #[test]
    fn explicit_remember() {
        let facts = extract_facts("Remember that I have a meeting tomorrow");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "fact");
        assert!(facts[0].content.contains("meeting tomorrow"));
    }

    #[test]
    fn remember_that_stripped() {
        let facts = extract_facts("Remember I need to buy milk");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].content.contains("buy milk"));
    }

    #[test]
    fn no_false_positive_im_a() {
        // "I'm a bit tired" should NOT extract "bit tired" as occupation
        let facts = extract_facts("I'm a bit tired");
        assert!(facts.iter().all(|f| f.category != "identity"));
    }
}
