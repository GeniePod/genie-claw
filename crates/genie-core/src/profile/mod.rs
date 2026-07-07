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

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::memory::{SharedMemory, with_shared_memory};

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
/// Called once at startup. PDF text extraction runs asynchronously **before**
/// taking the `Memory` lock so a hung `pdftotext` cannot wedge boot (#617).
pub async fn load_profile(profile_dir: &Path, memory: &SharedMemory) -> Result<ProfileReport> {
    let mut report = ProfileReport::default();

    if !profile_dir.exists() {
        tracing::debug!(dir = %profile_dir.display(), "profile directory not found — skipping");
        return Ok(report);
    }

    // 1. Load profile.toml (structured data — always re-read).
    let toml_path = profile_dir.join("profile.toml");
    if toml_path.exists() {
        match with_shared_memory(memory, move |mem| {
            toml_profile::load_toml_profile(&toml_path, mem)
        })
        .await
        {
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
    let paths: Vec<PathBuf> = std::fs::read_dir(profile_dir)?
        .flatten()
        .map(|entry| entry.path())
        .collect();

    for path in paths {
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
                let count = with_shared_memory(memory, {
                    let path = path.clone();
                    move |mem| ingest::ingest_text_file(&path, mem)
                })
                .await;
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
                let text = ingest::extract_pdf_text(&path).await;
                let count = if let Some(text) = text {
                    let filename = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    with_shared_memory(memory, move |mem| {
                        ingest::ingest_pdf_text(&text, &filename, mem)
                    })
                    .await
                } else {
                    0
                };
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
