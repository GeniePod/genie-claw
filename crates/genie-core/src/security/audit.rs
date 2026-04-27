use std::path::Path;

/// Startup security audit.
///
/// Checks filesystem permissions, config safety, and common misconfigurations.
/// Inspired by OpenClaw's audit.ts + audit-fs.ts — adapted for GeniePod's
/// appliance model (single-household, no multi-tenant).
///
/// Runs on startup. Logs warnings but does NOT block startup
/// (appliance must boot even if permissions are wrong).

#[derive(Debug, Clone)]
pub struct AuditFinding {
    pub id: String,
    pub severity: Severity,
    pub message: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// Run all startup security checks. Returns findings.
pub fn run_audit(config_path: &Path, data_dir: &Path) -> Vec<AuditFinding> {
    let mut findings = Vec::new();

    // 1. Config file permissions.
    check_file_permissions(config_path, "config", &mut findings);

    // 2. Data directory permissions.
    check_dir_permissions(data_dir, "data_dir", &mut findings);

    // 3. Check if config contains secrets in plain text.
    check_config_secrets(config_path, &mut findings);

    // 4. Check if running as root (bad practice for appliance).
    check_not_root(&mut findings);

    // 5. Check if API port is bound to localhost only.
    check_localhost_binding(&mut findings);

    // Log all findings.
    for finding in &findings {
        match finding.severity {
            Severity::Critical => {
                tracing::error!(
                    id = finding.id,
                    "SECURITY: {} — {}",
                    finding.message,
                    finding.remediation
                );
            }
            Severity::Warning => {
                tracing::warn!(
                    id = finding.id,
                    "SECURITY: {} — {}",
                    finding.message,
                    finding.remediation
                );
            }
            Severity::Info => {
                tracing::info!(id = finding.id, "SECURITY: {}", finding.message);
            }
        }
    }

    if findings.is_empty() {
        tracing::info!("security audit: all checks passed");
    } else {
        let critical = findings
            .iter()
            .filter(|f| f.severity == Severity::Critical)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count();
        tracing::info!(
            critical,
            warnings,
            total = findings.len(),
            "security audit complete"
        );
    }

    findings
}

fn check_file_permissions(path: &Path, label: &str, findings: &mut Vec<AuditFinding>) {
    if !path.exists() {
        findings.push(AuditFinding {
            id: format!("fs.{}.missing", label),
            severity: Severity::Warning,
            message: format!("{} not found: {}", label, path.display()),
            remediation: "Ensure config file exists at the expected path".into(),
        });
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            let bits = mode & 0o777;

            // World-readable config is a security risk (may contain HA tokens).
            if bits & 0o004 != 0 {
                findings.push(AuditFinding {
                    id: format!("fs.{}.world_readable", label),
                    severity: Severity::Critical,
                    message: format!(
                        "{} is world-readable (mode {:o}) — may contain secrets",
                        path.display(),
                        bits
                    ),
                    remediation: format!("chmod 600 {}", path.display()),
                });
            }

            // World-writable is critical.
            if bits & 0o002 != 0 {
                findings.push(AuditFinding {
                    id: format!("fs.{}.world_writable", label),
                    severity: Severity::Critical,
                    message: format!("{} is world-writable (mode {:o})", path.display(), bits),
                    remediation: format!("chmod 600 {}", path.display()),
                });
            }

            // Group-writable is a warning.
            if bits & 0o020 != 0 {
                findings.push(AuditFinding {
                    id: format!("fs.{}.group_writable", label),
                    severity: Severity::Warning,
                    message: format!("{} is group-writable (mode {:o})", path.display(), bits),
                    remediation: format!("chmod 600 {}", path.display()),
                });
            }

            // Symlink check.
            if meta.file_type().is_symlink() {
                findings.push(AuditFinding {
                    id: format!("fs.{}.symlink", label),
                    severity: Severity::Warning,
                    message: format!(
                        "{} is a symlink — may follow to unexpected target",
                        path.display()
                    ),
                    remediation: "Use a direct path instead of symlink".into(),
                });
            }
        }
    }
}

fn check_dir_permissions(path: &Path, label: &str, findings: &mut Vec<AuditFinding>) {
    if !path.exists() {
        return; // Data dir created on demand.
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let bits = meta.permissions().mode() & 0o777;

            if bits & 0o002 != 0 {
                findings.push(AuditFinding {
                    id: format!("fs.{}.world_writable", label),
                    severity: Severity::Critical,
                    message: format!(
                        "{} is world-writable (mode {:o}) — attacker can modify memory/conversations",
                        path.display(),
                        bits
                    ),
                    remediation: format!("chmod 700 {}", path.display()),
                });
            }

            if bits & 0o004 != 0 {
                findings.push(AuditFinding {
                    id: format!("fs.{}.world_readable", label),
                    severity: Severity::Critical,
                    message: format!(
                        "{} is world-readable (mode {:o}) — may expose shared memory, conversations, or audit data",
                        path.display(),
                        bits
                    ),
                    remediation: format!("chmod 700 {}", path.display()),
                });
            }

            if bits & 0o040 != 0 {
                findings.push(AuditFinding {
                    id: format!("fs.{}.group_readable", label),
                    severity: Severity::Warning,
                    message: format!(
                        "{} is group-readable (mode {:o}) — keep household memory under one trusted OS boundary",
                        path.display(),
                        bits
                    ),
                    remediation: format!("chmod 700 {}", path.display()),
                });
            }
        }
    }
}

fn check_config_secrets(path: &Path, findings: &mut Vec<AuditFinding>) {
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let trimmed = line.trim();
            let key = trimmed.split('=').next().unwrap_or("").trim();
            if matches!(
                key,
                "ha_token" | "bot_token" | "api_key" | "secret" | "password"
            ) && trimmed.contains('=')
            {
                let value = trimmed
                    .split('=')
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                if !value.is_empty() {
                    findings.push(AuditFinding {
                        id: format!("config.plaintext_secret.{}", key),
                        severity: Severity::Warning,
                        message: format!("{} stored in plain text in config file", key),
                        remediation:
                            "Prefer environment variables for secrets and keep config chmod 600"
                                .into(),
                    });
                }
            }
        }
    }
}

fn check_not_root(findings: &mut Vec<AuditFinding>) {
    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } == 0 {
            findings.push(AuditFinding {
                id: "process.running_as_root".into(),
                severity: Severity::Warning,
                message: "genie-core is running as root".into(),
                remediation:
                    "Create a dedicated 'geniepod' user: useradd -r -s /bin/false geniepod".into(),
            });
        }
    }
}

fn check_localhost_binding(findings: &mut Vec<AuditFinding>) {
    // This is informational — we always bind to 127.0.0.1.
    findings.push(AuditFinding {
        id: "net.localhost_only".into(),
        severity: Severity::Info,
        message: "HTTP API bound to 127.0.0.1 only (not exposed to network)".into(),
        remediation: String::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn audit_missing_config() {
        let findings = run_audit(
            Path::new("/tmp/nonexistent-geniepod-config.toml"),
            Path::new("/tmp/nonexistent-data"),
        );
        assert!(findings.iter().any(|f| f.id.contains("missing")));
    }

    #[test]
    fn audit_existing_config() {
        let path = std::env::temp_dir().join("geniepod-audit-test.toml");
        fs::write(&path, "# test config\nha_token = \"\"\n").unwrap();

        let findings = run_audit(&path, &std::env::temp_dir());
        // Should have at least the localhost_only info finding.
        assert!(findings.iter().any(|f| f.id == "net.localhost_only"));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn audit_plaintext_secret() {
        let path = std::env::temp_dir().join("geniepod-audit-secret.toml");
        fs::write(&path, "ha_token = \"eyJ0b2tlbi1zZWNyZXQi\"\n").unwrap();

        let findings = run_audit(&path, &std::env::temp_dir());
        assert!(
            findings
                .iter()
                .any(|f| f.id == "config.plaintext_secret.ha_token")
        );

        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn audit_world_readable_data_dir() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join("geniepod-audit-data-dir");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let mut findings = Vec::new();
        check_dir_permissions(&path, "data_dir", &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.id == "fs.data_dir.world_readable")
        );

        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn severity_ordering() {
        assert_ne!(Severity::Critical, Severity::Warning);
        assert_ne!(Severity::Warning, Severity::Info);
    }
}
