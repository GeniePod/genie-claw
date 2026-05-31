/// Kernel-level sandboxing for genie-core.
///
/// Uses Linux Landlock (5.13+) to restrict filesystem access to deployment
/// paths. Inference route validation and LLM output sanitization live here too.
use std::path::Path;
use std::sync::OnceLock;

use genie_common::config::{SandboxConfig, SandboxEnforcement};

use super::landlock_rules::{core_rules, PathAccess};

static SANDBOX_REPORT: OnceLock<SandboxReport> = OnceLock::new();

/// Result of attempting to apply the Landlock filesystem sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxReport {
    pub enforced: bool,
    pub abi_version: Option<u64>,
    pub message: String,
}

impl Default for SandboxReport {
    fn default() -> Self {
        Self {
            enforced: false,
            abi_version: None,
            message: "sandbox not initialized".into(),
        }
    }
}

/// Publish the startup sandbox report for `/api/health` and operators.
pub fn publish_sandbox_report(report: SandboxReport) {
    let _ = SANDBOX_REPORT.set(report);
}

/// Latest sandbox report from startup (or default if not yet published).
pub fn sandbox_report() -> SandboxReport {
    SANDBOX_REPORT.get().cloned().unwrap_or_default()
}

/// Apply Landlock filesystem restrictions when configured and supported.
///
/// After a successful restrict call, widening access is not possible for this
/// process. Gracefully degrades on non-Linux hosts and kernels without Landlock.
pub fn apply_landlock(
    config_dir: &Path,
    data_dir: &Path,
    settings: &SandboxConfig,
) -> SandboxReport {
    match settings.enforcement {
        SandboxEnforcement::Off => SandboxReport {
            enforced: false,
            abi_version: None,
            message: "sandbox enforcement disabled in config".into(),
        },
        SandboxEnforcement::Warn | SandboxEnforcement::Enforce => {
            #[cfg(target_os = "linux")]
            {
                apply_landlock_linux(config_dir, data_dir, settings)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (config_dir, data_dir, settings);
                SandboxReport {
                    enforced: false,
                    abi_version: None,
                    message: "Landlock not available on this platform".into(),
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn apply_landlock_linux(
    config_dir: &Path,
    data_dir: &Path,
    settings: &SandboxConfig,
) -> SandboxReport {
    use landlock::{
        path_beneath_rules, Access, AccessFs, ABI, LandlockStatus, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus,
    };

    let rules = core_rules(config_dir, data_dir);
    tracing::info!(
        config_dir = %config_dir.display(),
        data_dir = %data_dir.display(),
        rule_count = rules.len(),
        "Landlock filesystem sandbox: preparing rules"
    );

    let abi = ABI::V7;
    let ruleset = match Ruleset::default().handle_access(AccessFs::from_all(abi)) {
        Ok(ruleset) => ruleset,
        Err(error) => {
            let message = format!("Landlock handle_access failed: {error}");
            return landlock_outcome(settings, false, None, message);
        }
    };

    let mut created = match ruleset.create() {
        Ok(created) => created,
        Err(error) => {
            let message = format!("Landlock ruleset creation failed: {error}");
            return landlock_outcome(settings, false, None, message);
        }
    };

    for rule in &rules {
        let access = landlock_access(rule.access, abi);
        let path_iter = path_beneath_rules(std::iter::once(rule.path.as_path()), access);
        created = match created.add_rules(path_iter) {
            Ok(next) => next,
            Err(error) => {
                let message = format!(
                    "Landlock add_rule failed for {}: {error}",
                    rule.path.display()
                );
                return landlock_outcome(settings, false, None, message);
            }
        };
    }

    let status = match created.restrict_self() {
        Ok(status) => status,
        Err(error) => {
            let message = format!("Landlock restrict_self failed: {error}");
            return landlock_outcome(settings, false, None, message);
        }
    };

    let abi_version = kernel_abi_version_from_status(&status.landlock);
    if status.ruleset != RulesetStatus::FullyEnforced {
        let message = format!(
            "Landlock ruleset not fully enforced: {:?}",
            status.ruleset
        );
        return landlock_outcome(settings, false, abi_version, message);
    }

    if !matches!(status.landlock, LandlockStatus::Available { .. }) {
        let message = format!("Landlock unavailable: {:?}", status.landlock);
        return landlock_outcome(settings, false, abi_version, message);
    }

    landlock_outcome(
        settings,
        true,
        abi_version,
        "Landlock filesystem sandbox enforced".into(),
    )
}

#[cfg(target_os = "linux")]
fn landlock_access(
    access: PathAccess,
    abi: landlock::ABI,
) -> landlock::BitFlags<landlock::AccessFs> {
    use landlock::{Access, AccessFs};

    match access {
        PathAccess::ReadOnly => AccessFs::from_read(abi),
        PathAccess::ReadWrite => AccessFs::from_all(abi),
        PathAccess::Execute => AccessFs::from_read(abi) | AccessFs::Execute,
    }
}

#[cfg(target_os = "linux")]
fn kernel_abi_version_from_status(status: &landlock::LandlockStatus) -> Option<u64> {
    use landlock::LandlockStatus;

    match status {
        LandlockStatus::Available {
            kernel_abi: Some(raw_abi),
            ..
        } => Some(*raw_abi as u64),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn landlock_outcome(
    settings: &SandboxConfig,
    enforced: bool,
    abi_version: Option<u64>,
    message: String,
) -> SandboxReport {
    if enforced {
        tracing::info!(abi_version = ?abi_version, "{message}");
        return SandboxReport {
            enforced: true,
            abi_version,
            message,
        };
    }

    if settings.enforcement == SandboxEnforcement::Enforce {
        tracing::error!(abi_version = ?abi_version, "{message}");
    } else {
        tracing::warn!(abi_version = ?abi_version, "{message}");
    }

    SandboxReport {
        enforced: false,
        abi_version,
        message,
    }
}

/// Validate that an inference URL points to localhost only.
pub fn validate_inference_route(url: &str) -> Result<(), String> {
    let host = extract_host(url);

    match host.as_str() {
        "127.0.0.1" | "localhost" | "::1" | "[::1]" => Ok(()),
        h if h.starts_with("127.") => Ok(()),
        _ => Err(format!(
            "inference route rejected: {} is not localhost. \
             GeniePod only allows LLM calls to local endpoints.",
            url
        )),
    }
}

/// Sanitize LLM output — remove any leaked secrets before showing to user.
pub fn sanitize_output(text: &str) -> String {
    let mut result = text.to_string();

    for pattern in SECRET_PATTERNS {
        for re_match in find_secret_matches(&result, pattern) {
            let redacted = format!("[REDACTED:{}]", pattern.name);
            result = result.replace(&re_match, &redacted);
            tracing::warn!(
                pattern = pattern.name,
                "secret pattern detected and redacted from LLM output"
            );
        }
    }

    result
}

struct SecretPattern {
    name: &'static str,
    prefix: &'static str,
    min_len: usize,
}

const SECRET_PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        name: "api_key",
        prefix: "sk-",
        min_len: 20,
    },
    SecretPattern {
        name: "api_key",
        prefix: "pk-",
        min_len: 20,
    },
    SecretPattern {
        name: "bearer_token",
        prefix: "eyJ",
        min_len: 30,
    },
    SecretPattern {
        name: "aws_key",
        prefix: "AKIA",
        min_len: 16,
    },
    SecretPattern {
        name: "github_token",
        prefix: "ghp_",
        min_len: 20,
    },
    SecretPattern {
        name: "github_token",
        prefix: "gho_",
        min_len: 20,
    },
    SecretPattern {
        name: "github_token",
        prefix: "ghs_",
        min_len: 20,
    },
    SecretPattern {
        name: "slack_token",
        prefix: "xoxb-",
        min_len: 20,
    },
    SecretPattern {
        name: "slack_token",
        prefix: "xoxp-",
        min_len: 20,
    },
];

fn find_secret_matches(text: &str, pattern: &SecretPattern) -> Vec<String> {
    let mut matches = Vec::new();
    let mut search_start = 0;

    while let Some(rel_pos) = text[search_start..].find(pattern.prefix) {
        let pos = search_start + rel_pos;
        let rest = &text[pos..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
            .unwrap_or(rest.len());

        if end >= pattern.min_len {
            matches.push(rest[..end].to_string());
        }

        search_start = pos + end;
    }

    matches
}

fn extract_host(url: &str) -> String {
    let stripped = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    let host_port = stripped.split('/').next().unwrap_or(stripped);
    let host = host_port.split(':').next().unwrap_or(host_port);
    host.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn validate_localhost_routes() {
        assert!(validate_inference_route("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_inference_route("http://localhost:8080").is_ok());
        assert!(validate_inference_route("http://127.0.0.2:8080").is_ok());
    }

    #[test]
    fn reject_remote_routes() {
        assert!(validate_inference_route("http://api.openai.com/v1").is_err());
        assert!(validate_inference_route("http://192.168.1.100:8080").is_err());
        assert!(validate_inference_route("http://10.0.0.1:8080").is_err());
        assert!(validate_inference_route("https://example.com").is_err());
    }

    #[test]
    fn sanitize_api_keys() {
        let text = "The API key is sk-proj-1234567890abcdefghijklmnop in the config.";
        let sanitized = sanitize_output(text);
        assert!(sanitized.contains("[REDACTED:api_key]"));
        assert!(!sanitized.contains("sk-proj-"));
    }

    #[test]
    fn sanitize_jwt_tokens() {
        let text = "Found token: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0 in response.";
        let sanitized = sanitize_output(text);
        assert!(sanitized.contains("[REDACTED:bearer_token]"));
    }

    #[test]
    fn sanitize_github_tokens() {
        let text = "Token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef";
        let sanitized = sanitize_output(text);
        assert!(sanitized.contains("[REDACTED:github_token]"));
    }

    #[test]
    fn sanitize_aws_keys() {
        let text = "AWS key: AKIAIOSFODNN7EXAMPLE";
        let sanitized = sanitize_output(text);
        assert!(sanitized.contains("[REDACTED:aws_key]"));
    }

    #[test]
    fn sanitize_redacts_second_secret_with_same_prefix() {
        let first = "ghp_AAAAAAAAAAAAAAAAAAAAAAAA";
        let second = "ghp_BBBBBBBBBBBBBBBBBBBBBBBB";
        let text = format!("first {first} and second {second} end");
        let sanitized = sanitize_output(&text);
        assert!(!sanitized.contains(first));
        assert!(!sanitized.contains(second));
        assert_eq!(sanitized.matches("[REDACTED:github_token]").count(), 2);
    }

    #[test]
    fn sanitize_redacts_secret_after_short_decoy() {
        let real = "sk-proj-1234567890abcdefghijklmnop";
        let text = format!("decoy sk- then real {real} here");
        let sanitized = sanitize_output(&text);
        assert!(!sanitized.contains(real));
        assert!(sanitized.contains("[REDACTED:api_key]"));
    }

    #[test]
    fn no_false_positives_on_normal_text() {
        let text = "The weather in Denver is 72 degrees. Have a great day!";
        let sanitized = sanitize_output(text);
        assert_eq!(sanitized, text);
    }

    #[test]
    fn extract_host_from_url() {
        assert_eq!(extract_host("http://127.0.0.1:8080/v1"), "127.0.0.1");
        assert_eq!(extract_host("http://localhost:3000"), "localhost");
        assert_eq!(extract_host("https://api.openai.com/v1"), "api.openai.com");
    }

    #[test]
    fn landlock_off_mode_skips_enforcement() {
        let report = apply_landlock(
            Path::new("/etc/geniepod"),
            Path::new("/opt/geniepod/data"),
            &SandboxConfig {
                enforcement: SandboxEnforcement::Off,
                require_landlock: false,
            },
        );
        assert!(!report.enforced);
        assert_eq!(report.message, "sandbox enforcement disabled in config");
    }
}
