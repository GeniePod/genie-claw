//! Document ingestion — extract personal facts from MD, TXT, and PDF files.
//!
//! Reads documents from the profile directory, extracts text, and runs
//! pattern-based fact extraction to populate memory.
//!
//! PDF support requires `pdftotext` (from poppler-utils) installed on the system.

use std::path::Path;

use crate::memory::Memory;
use crate::memory::extract;

/// Ingest a text/markdown file into memory.
///
/// Reads the file, splits into paragraphs, extracts facts from each.
/// Returns the number of new facts stored.
pub fn ingest_text_file(path: &Path, memory: &Memory) -> usize {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read text file");
            return 0;
        }
    };

    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    ingest_text(&content, &filename, memory)
}

/// Ingest a PDF file into memory via `pdftotext`.
///
/// Shells out to `pdftotext` (poppler-utils) to extract text,
/// then runs pattern extraction on the result.
/// Returns the number of new facts stored.
pub fn ingest_pdf_file(path: &Path, memory: &Memory) -> usize {
    let output = match std::process::Command::new("pdftotext")
        .args([
            "-layout",
            &path.to_string_lossy(),
            "-", // output to stdout
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "pdftotext not found — install poppler-utils: sudo apt install poppler-utils"
            );
            return 0;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(path = %path.display(), error = %stderr, "pdftotext failed");
        return 0;
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    ingest_text(&text, &filename, memory)
}

/// Core text ingestion: split into chunks, extract facts, deduplicate, store.
fn ingest_text(text: &str, source: &str, memory: &Memory) -> usize {
    let mut total_stored = 0;

    // Strip markdown formatting for cleaner extraction.
    let clean = strip_markdown_light(text);

    // Process line by line — each line or sentence may contain facts.
    for line in clean.lines() {
        let line = line.trim();

        // Skip short lines, headers, and non-content.
        if line.len() < 10 || line.starts_with('#') || line.starts_with("---") {
            continue;
        }

        // Split long lines into sentences.
        for sentence in split_sentences(line) {
            let sentence = sentence.trim();
            if sentence.len() < 10 {
                continue;
            }

            // Extract facts using the auto-capture patterns.
            let facts = extract::extract_facts(sentence);
            for fact in facts {
                // Append source for traceability.
                let content_with_source = format!("{} (source: {})", fact.content, source);

                // Deduplicate against existing memories.
                match memory.has_similar(&fact.content) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(_) => continue,
                }

                if super::store_evergreen_if_allowed(memory, &fact.category, &content_with_source) {
                    total_stored += 1;
                }
            }
        }

        // Also extract key-value patterns common in resumes/profiles.
        let kv_facts = extract_key_value_facts(line, source);
        for (category, content) in kv_facts {
            match memory.has_similar(&content) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) => continue,
            }
            if super::store_evergreen_if_allowed(memory, &category, &content) {
                total_stored += 1;
            }
        }
    }

    total_stored
}

/// Extract key-value facts from structured document lines.
///
/// Handles patterns common in resumes and profiles:
/// - "Name: Jared Smith"
/// - "Email: jared@example.com"
/// - "Location: Denver, CO"
/// - "Skills: Rust, Python, CUDA"
/// - "Education: MS Computer Science, MIT"
fn extract_key_value_facts(line: &str, source: &str) -> Vec<(String, String)> {
    let mut facts = Vec::new();

    // Look for "Key: Value" pattern.
    if let Some(colon_pos) = line.find(':') {
        let key = line[..colon_pos].trim().to_lowercase();
        let value = line[colon_pos + 1..].trim();

        if value.is_empty() || value.len() > 200 {
            return facts;
        }

        let (category, content) = match key.as_str() {
            "name" | "full name" => (
                "identity",
                format!("User's name is {} (source: {})", value, source),
            ),
            "email" | "e-mail" => (
                "identity",
                format!("User's email is {} (source: {})", value, source),
            ),
            "phone" | "telephone" | "mobile" => (
                "identity",
                format!("User's phone is {} (source: {})", value, source),
            ),
            "location" | "address" | "city" => (
                "identity",
                format!("User lives in {} (source: {})", value, source),
            ),
            "occupation" | "title" | "role" | "position" => (
                "identity",
                format!("User works as {} (source: {})", value, source),
            ),
            "company" | "employer" | "organization" => (
                "identity",
                format!("User works at {} (source: {})", value, source),
            ),
            "education" | "degree" | "university" | "school" => (
                "identity",
                format!("User's education: {} (source: {})", value, source),
            ),
            "skills" | "technologies" | "expertise" | "languages" => (
                "identity",
                format!("User's skills: {} (source: {})", value, source),
            ),
            "interests" | "hobbies" => (
                "preference",
                format!("User's interests: {} (source: {})", value, source),
            ),
            "bio" | "summary" | "about" | "objective" => (
                "identity",
                format!("User bio: {} (source: {})", value, source),
            ),
            _ => return facts,
        };

        facts.push((category.to_string(), content));
    }

    facts
}

/// Light markdown stripping — removes formatting but keeps content.
fn strip_markdown_light(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        // Strip header markers but keep text.
        let line = trimmed.trim_start_matches('#').trim();

        // Strip bullet/list markers.
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
            .unwrap_or(line);

        // Strip bold/italic.
        let line = line.replace("**", "").replace("__", "");

        if !line.is_empty() {
            result.push_str(&line);
            result.push('\n');
        }
    }

    result
}

/// Split text into sentences.
fn split_sentences(text: &str) -> Vec<&str> {
    // Simple split on sentence-ending punctuation.
    let mut sentences = Vec::new();
    let mut start = 0;

    for (i, c) in text.char_indices() {
        if (c == '.' || c == '!' || c == '?') && i > start + 5 {
            sentences.push(&text[start..=i]);
            start = i + 1;
        }
    }

    // Include trailing fragment.
    let remainder = text[start..].trim();
    if !remainder.is_empty() {
        sentences.push(remainder);
    }

    sentences
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_memory() -> Memory {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-ingest-test-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        Memory::open(&path).unwrap()
    }

    #[test]
    fn ingest_markdown_with_facts() {
        let mem = temp_memory();
        let text =
            "# About Me\n\nMy name is Jared. I live in Denver. I love hiking and jazz music.\n";
        let count = ingest_text(text, "test.md", &mem);
        assert!(count >= 2, "expected >= 2 facts, got {}", count);
    }

    #[test]
    fn ingest_resume_key_value() {
        let mem = temp_memory();
        let text = "Name: Jared Smith\nLocation: Denver, CO\nSkills: Rust, Python, CUDA\n";
        let count = ingest_text(text, "resume.txt", &mem);
        assert!(count >= 3, "expected >= 3 facts from resume, got {}", count);

        let results = mem.search("Jared", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn ingest_strips_markdown() {
        let mem = temp_memory();
        let text = "## Skills\n\n**Rust**, **Python**, CUDA\n\n```code\nfn main() {}\n```\n\nI live in Denver.\n";
        let count = ingest_text(text, "test.md", &mem);
        assert!(count >= 1);
    }

    #[test]
    fn ingest_text_file_real() {
        let mem = temp_memory();
        let dir = std::env::temp_dir().join(format!("geniepod-ingest-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("about.md");
        std::fs::write(
            &path,
            "My name is Alice. I work at Google. I love machine learning.\n",
        )
        .unwrap();

        let count = ingest_text_file(&path, &mem);
        assert!(count >= 2, "expected >= 2 facts, got {}", count);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ingest_deduplicates() {
        let mem = temp_memory();
        let text = "My name is Jared.\n";
        let count1 = ingest_text(text, "file1.md", &mem);
        let count2 = ingest_text(text, "file2.md", &mem);
        assert_eq!(count1, 1);
        assert_eq!(count2, 0, "second ingest should be deduplicated");
    }

    #[test]
    fn ingest_rejects_restricted_secret_content() {
        let mem = temp_memory();
        let count = ingest_text("The gate code is 5829.", "notes.txt", &mem);
        assert_eq!(
            count, 0,
            "restricted secrets must not be stored from profile ingest"
        );

        let results = mem.search("5829", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn extract_kv_facts() {
        let facts = extract_key_value_facts("Skills: Rust, Python, CUDA, C++", "resume.pdf");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].1.contains("Rust"));
    }

    #[test]
    fn split_sentences_basic() {
        let sentences = split_sentences("Hello world. How are you? I'm fine!");
        assert_eq!(sentences.len(), 3);
    }
}
