//! Personal profile system — ingest user data from files into memory.
//!
//! Supports:
//! - `profile.toml` — structured identity, preferences, relationships
//! - `*.md`, `*.txt` — free-form text, auto-extracted via patterns
//! - `*.pdf` — text extracted via `pdftotext`, then pattern-extracted
//!
//! All data stays local. Files live in `/opt/geniepod/data/profile/`.
//! On startup, genie-core scans this directory and ingests into memory.
//!
//! ## Version Roadmap
//! - V1: Single user, file-based profile
//! - V3: Speaker identification, multi-user, per-user isolation

pub mod ingest;
pub mod toml_profile;

use std::path::Path;

use anyhow::Result;

use crate::memory::Memory;

/// Store an evergreen profile memory only when household write policy allows it.
pub(crate) fn store_evergreen_if_allowed(memory: &Memory, kind: &str, content: &str) -> bool {
    let policy = crate::memory::policy::assess_memory_write(kind, content);
    if !policy.allowed {
        tracing::debug!(
            kind,
            reason = policy.reason,
            "skipping profile memory by write policy"
        );
        return false;
    }
    memory.store_evergreen(kind, content).is_ok()
}

/// Ingest all profile data from the profile directory into memory.
///
/// Called once at startup. Skips files that have already been ingested
/// (tracked via a metadata entry in memory).
pub fn load_profile(profile_dir: &Path, memory: &Memory) -> Result<ProfileReport> {
    let mut report = ProfileReport::default();

    if !profile_dir.exists() {
        tracing::debug!(dir = %profile_dir.display(), "profile directory not found — skipping");
        return Ok(report);
    }

    // 1. Load profile.toml (structured data — always re-read).
    let toml_path = profile_dir.join("profile.toml");
    if toml_path.exists() {
        match toml_profile::load_toml_profile(&toml_path, memory) {
            Ok(count) => {
                tracing::info!(facts = count, "profile.toml loaded");
                report.toml_facts = count;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load profile.toml");
            }
        }
    }

    // 2. Scan for document files (.md, .txt, .pdf).
    let entries = std::fs::read_dir(profile_dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Skip profile.toml (already handled) and non-document files.
        if path.file_name().is_some_and(|n| n == "profile.toml") {
            continue;
        }

        match ext.as_str() {
            "md" | "txt" => {
                let count = ingest::ingest_text_file(&path, memory);
                if count > 0 {
                    tracing::info!(
                        file = %path.display(),
                        facts = count,
                        "ingested text file"
                    );
                }
                report.doc_facts += count;
                report.files_processed += 1;
            }
            "pdf" => {
                let count = ingest::ingest_pdf_file(&path, memory);
                if count > 0 {
                    tracing::info!(
                        file = %path.display(),
                        facts = count,
                        "ingested PDF file"
                    );
                }
                report.doc_facts += count;
                report.files_processed += 1;
            }
            _ => {
                // Skip unsupported file types.
            }
        }
    }

    Ok(report)
}

/// Report from profile loading.
#[derive(Debug, Default)]
pub struct ProfileReport {
    pub toml_facts: usize,
    pub doc_facts: usize,
    pub files_processed: usize,
}

impl ProfileReport {
    pub fn total(&self) -> usize {
        self.toml_facts + self.doc_facts
    }
}
