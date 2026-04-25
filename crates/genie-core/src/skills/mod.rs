//! Loadable Skill Modules (LSM) — Linux-inspired dynamic skill loading.
//!
//! Scans `/opt/geniepod/skills/` for `.so` files, loads them via `dlopen`,
//! and registers their tools with the dispatcher.

use std::path::PathBuf;

pub mod loader;

pub use loader::{
    LoadedSkill, SkillLoader, SkillManifest, SkillManifestAudit, find_manifest_sidecar,
    manifest_sidecar_candidates,
};

pub const DEFAULT_SKILLS_DIR: &str = "/opt/geniepod/skills";

/// Resolve the skills directory. Production defaults to `/opt/geniepod/skills`,
/// while development/tests may override it with `GENIEPOD_SKILLS_DIR`.
pub fn skills_dir() -> PathBuf {
    std::env::var("GENIEPOD_SKILLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SKILLS_DIR))
}

/// Load all skills from the configured skills directory.
pub fn load_all() -> SkillLoader {
    let skills_dir = skills_dir();
    let mut loader = SkillLoader::new(&skills_dir);
    let loaded_names = loader.load_all();

    if loaded_names.is_empty() {
        tracing::debug!(dir = %skills_dir.display(), "no loadable skills found");
    } else {
        tracing::info!(
            dir = %skills_dir.display(),
            count = loaded_names.len(),
            skills = ?loaded_names,
            "loaded dynamic skills"
        );
    }

    loader
}
