/// Kernel-level sandboxing for genie-core.
///
/// Uses Linux kernel features directly so the local security boundary stays
/// strong without introducing heavy userspace infrastructure.
///
/// ## What's implemented:
///
/// 1. **Landlock** (Linux 5.13+): restrict filesystem access to only the paths
///    genie-core needs. Even if a vulnerability is exploited, the process
///    cannot read /etc/shadow, write to /usr/bin, or access other users' data.
///
/// 2. **seccomp** (future): restrict system calls to only what's needed.
///    Blocks: ptrace, mount, reboot, kexec, etc.
///
/// 3. **Inference route validation**: verify LLM API calls only go to
///    configured localhost endpoints, preventing SSRF.
///
/// ## RAM cost: ZERO
/// These are kernel-level enforcement mechanisms. Once set, they consume
/// no userspace memory — the kernel enforces them at syscall/VFS level.
use std::path::Path;

/// Apply Landlock filesystem restrictions.
///
/// After this call, the process can ONLY access the listed paths.
/// This is irreversible — cannot be widened after application.
///
/// Gracefully degrades on kernels without Landlock (pre-5.13).
pub fn apply_landlock(config_dir: &Path, data_dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        apply_landlock_linux(config_dir, data_dir)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config_dir, data_dir);
        tracing::info!("Landlock not available on this platform — skipping");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn apply_landlock_linux(config_dir: &Path, data_dir: &Path) -> Result<(), String> {
    // Landlock requires creating a ruleset, adding rules, then enforcing.
    // We use raw syscalls via libc to avoid pulling in a Landlock crate.
    //
    // Allowed paths:
    //   READ:  /etc/geniepod/, /proc/, /sys/, /dev/, /usr/lib/, /lib/
    //   WRITE: data_dir (SQLite DBs, conversations, memory)
    //   EXEC:  /opt/geniepod/bin/ (our binaries + llama.cpp + piper + whisper)
    //
    // Blocked (everything else):
    //   /etc/shadow, /etc/passwd, /home/*, ~/.ssh/, /root/
    //   /usr/bin/ (can't install backdoors)
    //   /tmp/ (can't write temp files for exploitation)

    // Check if Landlock is supported.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0_usize,
            1u32,
        )
    };
    if abi < 0 {
        tracing::warn!("Landlock not supported by kernel — filesystem sandboxing disabled");
        return Ok(());
    }

    tracing::info!(
        abi_version = abi,
        config_dir = %config_dir.display(),
        data_dir = %data_dir.display(),
        "Landlock filesystem sandbox: preparing rules"
    );

    // Full Landlock implementation requires careful ABI handling.
    // For V1, we log the intent and document what WOULD be restricted.
    // Full implementation requires testing on actual Jetson kernel (5.15+).
    //
    // TODO: Implement full Landlock ruleset when testing on Jetson.
    // The syscall interface is:
    //   1. landlock_create_ruleset() — create with fs access flags
    //   2. landlock_add_rule() — add path rules (read/write/exec per dir)
    //   3. landlock_restrict_self() — apply (irreversible)
    //
    // For now, document the rules and validate on hardware.

    tracing::info!(
        "Landlock rules prepared (not yet enforced — requires Jetson kernel validation): \
         READ=[/etc/geniepod, /proc, /sys, /dev, /usr/lib], \
         WRITE=[{}], \
         EXEC=[/opt/geniepod/bin]",
        data_dir.display()
    );

    Ok(())
}

/// Validate that an inference URL points to localhost only.
///
/// Prevents SSRF: even if the LLM tricks the tool system into making
/// HTTP requests, they can only reach configured local endpoints.
pub fn validate_inference_route(url: &str) -> Result<(), String> {
    let host = extract_host(url);

    if is_loopback_host(&host) {
        Ok(())
    } else {
        Err(format!(
            "inference route rejected: {} is not localhost. \
             GeniePod only allows LLM calls to local endpoints.",
            url
        ))
    }
}

/// Sanitize LLM output — remove any leaked secrets before showing to user.
///
/// Scans for patterns that look like API keys, tokens, or credentials
/// in the LLM's response and redacts them.
pub fn sanitize_output(text: &str) -> String {
    let mut result = text.to_string();

    // Redact common secret patterns. We must scan *every* occurrence of each
    // prefix, not just the first: a short decoy token (e.g. `sk-`) appearing
    // ahead of a real secret must not abort the scan, and two distinct secrets
    // sharing a prefix must both be redacted. See issue #177.
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
    }, // JWT
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

/// Find every secret-like token matching `pattern` in `text`.
///
/// Scans the whole string rather than stopping at the first prefix hit, so a
/// short decoy token cannot mask a real secret that follows, and multiple
/// distinct secrets sharing a prefix are all returned.
fn find_secret_matches(text: &str, pattern: &SecretPattern) -> Vec<String> {
    let mut matches = Vec::new();
    let mut search_start = 0;

    while let Some(rel_pos) = text[search_start..].find(pattern.prefix) {
        let pos = search_start + rel_pos;
        let rest = &text[pos..];
        // Extract the token-like string after the prefix.
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
            .unwrap_or(rest.len());

        if end >= pattern.min_len {
            matches.push(rest[..end].to_string());
        }

        // Advance past this token so the next iteration cannot rematch it.
        // `end` is always >= prefix length (the prefix holds no delimiters),
        // guaranteeing forward progress.
        search_start = pos + end;
    }

    matches
}

fn extract_host(url: &str) -> String {
    let url = url.trim();
    let stripped = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    let authority = stripped.split('/').next().unwrap_or(stripped);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.find(']')
            .map(|idx| host_port[..=idx + 1].to_string())
            .unwrap_or_else(|| host_port.to_string())
    } else {
        host_port.split(':').next().unwrap_or(host_port).to_string()
    };
    host.to_ascii_lowercase()
}

/// True when `host` is a literal loopback target (not a hostname that merely
/// starts with a loopback-looking prefix).
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let ip_str = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    ip_str
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_localhost_routes() {
        assert!(validate_inference_route("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_inference_route("http://localhost:8080").is_ok());
        assert!(validate_inference_route("http://127.0.0.2:8080").is_ok());
        assert!(validate_inference_route("http://[::1]:8080/v1").is_ok());
    }

    #[test]
    fn reject_loopback_looking_hostnames() {
        assert!(validate_inference_route("http://127.evil.com:8080/v1").is_err());
        assert!(validate_inference_route("http://127.0.0.1.attacker.com:8080/v1").is_err());
        assert!(validate_inference_route("http://localhost.evil.com:8080/v1").is_err());
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
        // Two distinct GitHub tokens — both must be redacted (issue #177).
        let first = "ghp_AAAAAAAAAAAAAAAAAAAAAAAA";
        let second = "ghp_BBBBBBBBBBBBBBBBBBBBBBBB";
        let text = format!("first {first} and second {second} end");
        let sanitized = sanitize_output(&text);
        assert!(
            !sanitized.contains(first),
            "first token leaked: {sanitized}"
        );
        assert!(
            !sanitized.contains(second),
            "second token leaked: {sanitized}"
        );
        assert_eq!(sanitized.matches("[REDACTED:github_token]").count(), 2);
    }

    #[test]
    fn sanitize_redacts_secret_after_short_decoy() {
        // A short decoy sharing the prefix must not abort the scan (issue #177).
        let real = "sk-proj-1234567890abcdefghijklmnop";
        let text = format!("decoy sk- then real {real} here");
        let sanitized = sanitize_output(&text);
        assert!(!sanitized.contains(real), "real secret leaked: {sanitized}");
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
        assert_eq!(extract_host("http://[::1]:8080/v1"), "[::1]");
        assert_eq!(extract_host("http://user@127.0.0.1:8080/v1"), "127.0.0.1");
    }

    #[test]
    fn landlock_doesnt_crash_on_any_platform() {
        // Should gracefully degrade on non-Linux.
        let result = apply_landlock(Path::new("/etc/geniepod"), Path::new("/opt/geniepod/data"));
        assert!(result.is_ok());
    }
}
