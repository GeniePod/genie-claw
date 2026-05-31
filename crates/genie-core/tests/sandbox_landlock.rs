//! Landlock integration checks (issue #347).
//!
//! Verifies that enforced sandboxes deny reads outside the allowlist.

use genie_common::config::{SandboxConfig, SandboxEnforcement};
use genie_core::security::apply_landlock;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

#[test]
fn landlock_denies_sensitive_paths_when_enforced() {
    let report = apply_landlock(
        Path::new("/etc/geniepod"),
        Path::new("/opt/geniepod/data"),
        &SandboxConfig {
            enforcement: SandboxEnforcement::Enforce,
            require_landlock: false,
        },
    );
    if !report.enforced {
        eprintln!(
            "skipping landlock enforcement test: {}",
            report.message
        );
        return;
    }

    let denied = OpenOptions::new().read(true).open("/etc/shadow");
    match &denied {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        other => panic!("expected EACCES opening /etc/shadow, got {other:?}"),
    }
}
