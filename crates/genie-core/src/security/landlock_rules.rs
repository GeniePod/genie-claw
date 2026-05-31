//! Central Landlock path allowlists for genie-core.
//!
//! Keep filesystem policy in one place so new data directories do not require
//! hunting call sites across the runtime.

use std::path::{Path, PathBuf};

/// Filesystem paths and access class passed to Landlock rule construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccess {
    ReadOnly,
    ReadWrite,
    Execute,
}

#[derive(Debug, Clone)]
pub struct LandlockPathRule {
    pub path: PathBuf,
    pub access: PathAccess,
}

/// Build the genie-core Landlock ruleset from deployment paths.
pub fn core_rules(config_dir: &Path, data_dir: &Path) -> Vec<LandlockPathRule> {
    let mut rules = Vec::new();

    push_dir(&mut rules, config_dir, PathAccess::ReadOnly);
    push_dir(&mut rules, data_dir, PathAccess::ReadWrite);

    for path in [
        "/proc",
        "/sys",
        "/dev",
        "/usr/lib",
        "/lib",
        "/lib64",
        "/opt/geniepod/bin",
    ] {
        push_dir(&mut rules, Path::new(path), PathAccess::ReadOnly);
    }

    push_dir(
        &mut rules,
        Path::new("/opt/geniepod/bin"),
        PathAccess::Execute,
    );

    rules
}

fn push_dir(rules: &mut Vec<LandlockPathRule>, path: &Path, access: PathAccess) {
    let path = normalize_path(path);
    if rules.iter().any(|rule| rule.path == path && rule.access == access) {
        return;
    }
    rules.push(LandlockPathRule { path, access });
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
