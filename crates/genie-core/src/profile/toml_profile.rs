//! Structured profile loader from profile.toml.
//!
//! Maps TOML sections to memory categories:
//! - [identity] → "identity" memories (evergreen)
//! - [preferences] → "preference" memories (evergreen)
//! - [family] / [relationships] → "relationship" memories (evergreen)
//! - [routines] → "context" memories (evergreen)

use std::path::Path;

use anyhow::Result;

use crate::memory::Memory;

/// Load profile.toml and store entries as evergreen memories.
///
/// Returns the number of facts stored.
pub fn load_toml_profile(path: &Path, memory: &Memory) -> Result<usize> {
    let content = std::fs::read_to_string(path)?;
    let doc: toml::Value = content.parse()?;
    let table = doc
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("profile.toml is not a table"))?;

    let mut stored = 0;

    // [identity] section.
    if let Some(identity) = table.get("identity").and_then(|v| v.as_table()) {
        for (key, value) in identity {
            let text = value_to_string(value);
            if text.is_empty() {
                continue;
            }
            let fact = format!("User's {} is {}", key, text);
            if !memory.has_similar(&fact).unwrap_or(false)
                && super::store_evergreen_if_allowed(memory, "identity", &fact)
            {
                stored += 1;
            }
        }
    }

    // [preferences] section.
    if let Some(prefs) = table.get("preferences").and_then(|v| v.as_table()) {
        for (key, value) in prefs {
            let text = value_to_string(value);
            if text.is_empty() {
                continue;
            }
            let fact = match value {
                toml::Value::Array(_) => format!("User's {} preferences: {}", key, text),
                _ => format!("User prefers {} {}", key, text),
            };
            if !memory.has_similar(&fact).unwrap_or(false)
                && super::store_evergreen_if_allowed(memory, "preference", &fact)
            {
                stored += 1;
            }
        }
    }

    // [family] or [relationships] section.
    for section_name in ["family", "relationships"] {
        if let Some(family) = table.get(section_name).and_then(|v| v.as_table()) {
            for (relation, name) in family {
                let text = value_to_string(name);
                if text.is_empty() {
                    continue;
                }
                let fact = format!("User's {} is {}", relation, text);
                if !memory.has_similar(&fact).unwrap_or(false)
                    && super::store_evergreen_if_allowed(memory, "relationship", &fact)
                {
                    stored += 1;
                }
            }
        }
    }

    // [routines] section.
    if let Some(routines) = table.get("routines").and_then(|v| v.as_table()) {
        for (name, steps) in routines {
            let text = value_to_string(steps);
            if text.is_empty() {
                continue;
            }
            let fact = format!("{} routine: {}", name, text);
            if !memory.has_similar(&fact).unwrap_or(false)
                && super::store_evergreen_if_allowed(memory, "context", &fact)
            {
                stored += 1;
            }
        }
    }

    // [work] section.
    if let Some(work) = table.get("work").and_then(|v| v.as_table()) {
        for (key, value) in work {
            let text = value_to_string(value);
            if text.is_empty() {
                continue;
            }
            let fact = format!("User's work {}: {}", key, text);
            if !memory.has_similar(&fact).unwrap_or(false)
                && super::store_evergreen_if_allowed(memory, "identity", &fact)
            {
                stored += 1;
            }
        }
    }

    // [about] section — free text.
    if let Some(about) = table.get("about").and_then(|v| v.as_str()) {
        // Split into sentences and extract facts from each.
        for sentence in about.split('.') {
            let sentence = sentence.trim();
            if sentence.len() > 10 {
                let facts = crate::memory::extract::extract_facts(sentence);
                for fact in facts {
                    if !memory.has_similar(&fact.content).unwrap_or(false)
                        && super::store_evergreen_if_allowed(memory, &fact.category, &fact.content)
                    {
                        stored += 1;
                    }
                }
            }
        }
    }

    Ok(stored)
}

/// Convert a TOML value to a display string.
///
/// Every scalar TOML can hold renders, including dates and times — a profile
/// realistically carries `birthday = 1990-05-15` or `wake = 06:30:00`. Arrays
/// render element-wise with these same rules, so `[7, 13]` renders the way the
/// `7` and `13` scalars do. Only a nested table has no display form and yields
/// an empty string, which callers treat as "skip this key". The match is
/// exhaustive on purpose: a catch-all previously swallowed `Datetime`, so those
/// facts were dropped without a trace.
fn value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(arr) => arr
            .iter()
            .map(value_to_string)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        toml::Value::Table(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Render the single value in a one-key TOML document.
    fn render(src: &str) -> String {
        let doc: toml::Value = src.parse().unwrap();
        let table = doc.as_table().unwrap();
        let (_, value) = table.iter().next().unwrap();
        value_to_string(value)
    }

    #[test]
    fn value_to_string_renders_datetimes_and_non_string_arrays() {
        // Dates/times are scalars a personal profile holds (birthday, wake
        // time). A catch-all arm used to render them "", so load_toml_profile
        // silently skipped the fact. Arrays render element-wise like scalars.
        assert_eq!(render("birthday = 1990-05-15"), "1990-05-15");
        assert_eq!(render("wake = 06:30:00"), "06:30:00");
        assert_eq!(render("lucky = [7, 13]"), "7, 13");
        assert_eq!(render("mixed = [\"jazz\", 5]"), "jazz, 5");
        // Unchanged for the scalars that already worked.
        assert_eq!(render("age = 32"), "32");
        assert_eq!(render("music = [\"jazz\", \"lo-fi\"]"), "jazz, lo-fi");
    }

    fn temp_memory() -> Memory {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-profile-test-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        Memory::open(&path).unwrap()
    }

    #[test]
    fn load_identity() {
        let mem = temp_memory();
        let dir = std::env::temp_dir().join(format!("geniepod-profile-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("profile.toml"),
            r#"
[identity]
name = "Jared"
age = 32
occupation = "CTO"
location = "Denver, CO"
"#,
        )
        .unwrap();

        let count = load_toml_profile(&dir.join("profile.toml"), &mem).unwrap();
        assert!(count >= 4, "expected >= 4 identity facts, got {}", count);

        let results = mem.search("Jared", 10).unwrap();
        assert!(!results.is_empty(), "should find Jared in memory");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_preferences() {
        let mem = temp_memory();
        let dir = std::env::temp_dir().join(format!("geniepod-pref-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("profile.toml"),
            r#"
[preferences]
music = ["jazz", "lo-fi", "classical"]
temperature_unit = "fahrenheit"
"#,
        )
        .unwrap();

        let count = load_toml_profile(&dir.join("profile.toml"), &mem).unwrap();
        assert!(count >= 2);

        let results = mem.search("jazz", 10).unwrap();
        assert!(!results.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_family() {
        let mem = temp_memory();
        let dir = std::env::temp_dir().join(format!("geniepod-fam-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("profile.toml"),
            r#"
[family]
wife = "Sarah"
dog = "Rex"
"#,
        )
        .unwrap();

        let count = load_toml_profile(&dir.join("profile.toml"), &mem).unwrap();
        assert_eq!(count, 2);

        let results = mem.search("Sarah", 10).unwrap();
        assert!(!results.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deduplication() {
        let mem = temp_memory();
        let dir = std::env::temp_dir().join(format!("geniepod-dedup-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("profile.toml"),
            r#"
[identity]
name = "Jared"
"#,
        )
        .unwrap();

        // Load twice — should not duplicate.
        let count1 = load_toml_profile(&dir.join("profile.toml"), &mem).unwrap();
        let count2 = load_toml_profile(&dir.join("profile.toml"), &mem).unwrap();
        assert_eq!(count1, 1);
        assert_eq!(count2, 0, "second load should be deduplicated");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_restricted_secret_content() {
        let mem = temp_memory();
        let dir = std::env::temp_dir().join(format!("geniepod-secret-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("profile.toml"),
            r#"about = "The gate code is 5829.""#,
        )
        .unwrap();

        let count = load_toml_profile(&dir.join("profile.toml"), &mem).unwrap();
        assert_eq!(
            count, 0,
            "restricted secrets must not be stored from profile.toml"
        );

        let results = mem.search("5829", 10).unwrap();
        assert!(results.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
