use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// OTA update system for GeniePod.
///
/// Checks GitHub Releases for new versions, downloads binaries,
/// and triggers a rolling restart via systemd.
///
/// Update flow:
/// 1. Timer fires daily (or user triggers via CLI/API)
/// 2. Check GitHub Releases API for latest version
/// 3. Compare with current version
/// 4. If newer: download aarch64 binaries to staging dir
/// 5. Verify checksums
/// 6. Stop services, replace binaries, restart services
///
/// Safety:
/// - Old binaries backed up before replacement
/// - Rollback if new binary fails health check within 60s
/// - Governor pauses mode switching during update

const GITHUB_REPO: &str = "GeniePod/genie-claw";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub version: String,
    pub published_at: String,
    pub download_url: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub last_check: Option<String>,
}

pub struct OtaManager {
    install_dir: PathBuf,
    staging_dir: PathBuf,
    backup_dir: PathBuf,
}

impl OtaManager {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            install_dir: base_dir.join("bin"),
            staging_dir: base_dir.join("staging"),
            backup_dir: base_dir.join("backup"),
        }
    }

    /// Check GitHub Releases for a newer version.
    pub async fn check_update(&self) -> Result<UpdateStatus> {
        let latest = self.fetch_latest_release().await;

        let (latest_version, update_available) = match &latest {
            Ok(release) => {
                let latest_ver = release.version.clone();
                let is_newer = version_is_newer(&latest_ver, CURRENT_VERSION);
                (Some(latest_ver), is_newer)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to check for updates");
                (None, false)
            }
        };

        Ok(UpdateStatus {
            current_version: CURRENT_VERSION.to_string(),
            latest_version,
            update_available,
            last_check: Some(now_iso()),
        })
    }

    /// Fetch latest release info from GitHub Releases API.
    async fn fetch_latest_release(&self) -> Result<ReleaseInfo> {
        let path = format!("/repos/{}/releases/latest", GITHUB_REPO);
        let body = github_api_get(&path).await?;
        let release: serde_json::Value = serde_json::from_str(&body)?;

        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();

        let published = release
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let body_text = release
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Find aarch64 binary asset.
        let download_url = release
            .get("assets")
            .and_then(|v| v.as_array())
            .and_then(|assets| {
                assets.iter().find_map(|a| {
                    let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.contains("aarch64") || name.contains("arm64") {
                        a.get("browser_download_url")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    } else {
                        None
                    }
                })
            });

        Ok(ReleaseInfo {
            tag_name: tag,
            version,
            published_at: published,
            download_url,
            body: body_text,
        })
    }

    /// Get current version.
    pub fn current_version(&self) -> &str {
        CURRENT_VERSION
    }

    /// Prepare staging directory for update.
    pub async fn prepare_staging(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.staging_dir).await?;
        tokio::fs::create_dir_all(&self.backup_dir).await?;
        Ok(())
    }

    /// Backup current binaries before update.
    pub async fn backup_current(&self) -> Result<()> {
        let binaries = [
            "genie-core",
            "genie-ctl",
            "genie-governor",
            "genie-health",
            "genie-api",
        ];

        for bin in &binaries {
            let src = self.install_dir.join(bin);
            let dst = self.backup_dir.join(bin);
            if src.exists() {
                tokio::fs::copy(&src, &dst).await?;
                tracing::debug!(binary = bin, "backed up");
            }
        }

        Ok(())
    }

    /// Rollback to backed-up binaries.
    pub async fn rollback(&self) -> Result<()> {
        tracing::warn!("rolling back to previous version");
        let binaries = [
            "genie-core",
            "genie-ctl",
            "genie-governor",
            "genie-health",
            "genie-api",
        ];

        for bin in &binaries {
            let src = self.backup_dir.join(bin);
            let dst = self.install_dir.join(bin);
            if src.exists() {
                tokio::fs::copy(&src, &dst).await?;
                tracing::info!(binary = bin, "rolled back");
            }
        }

        Ok(())
    }
}

/// A parsed semantic version: the numeric `major.minor.patch` core plus any
/// pre-release identifiers.
#[derive(Debug, PartialEq, Eq)]
struct SemVer {
    core: (u32, u32, u32),
    /// Dot-separated pre-release identifiers (the part after `-`). Empty for a
    /// normal release. A release outranks any pre-release of the same core.
    pre: Vec<String>,
}

/// Parse a version string into a [`SemVer`].
///
/// Tolerant of a leading `v`. Build metadata (`+...`) is ignored for
/// precedence, per SemVer §10. Missing core components default to 0.
fn parse_semver(s: &str) -> SemVer {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    // Build metadata never affects precedence — drop anything from '+' on.
    let s = s.split('+').next().unwrap_or(s);
    // Core is everything before the first '-'; the rest is the pre-release.
    let (core_str, pre_str) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };

    // Positional parse: an unparseable component becomes 0 without shifting the
    // ones after it (so "1.x.2" is (1, 0, 2), not (1, 2, 0)).
    let mut nums = core_str
        .split('.')
        .map(|p| p.trim().parse::<u32>().unwrap_or(0));
    let core = (
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
    );

    let pre = pre_str
        .filter(|p| !p.is_empty())
        .map(|p| p.split('.').map(|id| id.to_string()).collect())
        .unwrap_or_default();

    SemVer { core, pre }
}

/// Compare a single pair of pre-release identifiers per SemVer §11:
/// numeric identifiers compare numerically and always rank below alphanumeric
/// ones; alphanumeric identifiers compare in ASCII order.
fn compare_pre_identifier(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(an), Ok(bn)) => an.cmp(&bn),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// Full SemVer §11 precedence ordering between two parsed versions.
fn compare_semver(a: &SemVer, b: &SemVer) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    // 1. Numeric core dominates.
    match a.core.cmp(&b.core) {
        Ordering::Equal => {}
        non_eq => return non_eq,
    }

    // 2. A release (no pre-release) outranks any pre-release of the same core.
    match (a.pre.is_empty(), b.pre.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    // 3. Compare pre-release identifiers left to right.
    for (ai, bi) in a.pre.iter().zip(b.pre.iter()) {
        match compare_pre_identifier(ai, bi) {
            Ordering::Equal => {}
            non_eq => return non_eq,
        }
    }

    // 4. When all shared identifiers match, the longer set has higher precedence.
    a.pre.len().cmp(&b.pre.len())
}

/// Compare semver strings. Returns true if `latest` is strictly newer than
/// `current`, with full SemVer §11 pre-release precedence — so
/// `1.0.0-alpha.12` is newer than `1.0.0-alpha.11`, and `1.0.0` is newer than
/// any `1.0.0-alpha.N`.
fn version_is_newer(latest: &str, current: &str) -> bool {
    compare_semver(&parse_semver(latest), &parse_semver(current)) == std::cmp::Ordering::Greater
}

/// GET request to GitHub API (api.github.com).
/// Uses curl for TLS — available on all Jetson images.
async fn github_api_get(path: &str) -> Result<String> {
    let url = format!("https://api.github.com{}", path);
    let output = tokio::process::Command::new("curl")
        .args([
            "-sS",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: GeniePod-OTA",
            &url,
        ])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "GitHub API request failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple ISO-ish timestamp without chrono.
    #[cfg(unix)]
    {
        let time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&time_t, &mut tm) };
        if !result.is_null() {
            return format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            );
        }
    }

    format!("{}", secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_basic() {
        assert!(version_is_newer("1.1.0", "1.0.0"));
        assert!(version_is_newer("2.0.0", "1.9.9"));
        assert!(version_is_newer("1.0.1", "1.0.0"));
        assert!(!version_is_newer("1.0.0", "1.0.0"));
        assert!(!version_is_newer("0.9.0", "1.0.0"));
    }

    #[test]
    fn version_comparison_with_prefix() {
        assert!(version_is_newer("v1.1.0", "v1.0.0"));
        assert!(version_is_newer("v2.0.0", "1.0.0"));
    }

    #[test]
    fn version_comparison_with_prerelease() {
        // A higher numeric core wins regardless of pre-release tags.
        assert!(version_is_newer("1.1.0-alpha.1", "1.0.0-alpha.1"));
        // Pre-releases of the SAME core order by their identifiers (SemVer §11),
        // so a later alpha IS newer than an earlier one. This is the case the
        // OTA checker depends on during the whole `1.0.0-alpha.N` release line —
        // the previous "strip the suffix" logic made every alpha compare equal,
        // so the device never saw a new alpha.
        assert!(version_is_newer("1.0.0-alpha.2", "1.0.0-alpha.1"));
        assert!(version_is_newer("1.0.0-alpha.11", "1.0.0-alpha.9"));
        assert!(!version_is_newer("1.0.0-alpha.1", "1.0.0-alpha.2"));
    }

    #[test]
    fn prerelease_numeric_identifiers_compare_numerically() {
        // The exact regression: alpha.11 must beat alpha.9 (string compare would
        // say "11" < "9"; numeric compare says 11 > 9).
        assert!(version_is_newer("1.0.0-alpha.12", "1.0.0-alpha.11"));
        assert!(version_is_newer("1.0.0-alpha.100", "1.0.0-alpha.99"));
        assert!(!version_is_newer("1.0.0-alpha.9", "1.0.0-alpha.11"));
    }

    #[test]
    fn release_outranks_its_prerelease() {
        // 1.0.0 final is newer than any 1.0.0 pre-release...
        assert!(version_is_newer("1.0.0", "1.0.0-alpha.11"));
        assert!(version_is_newer("1.0.0", "1.0.0-rc.1"));
        // ...and a pre-release is NOT newer than the matching final release.
        assert!(!version_is_newer("1.0.0-alpha.1", "1.0.0"));
        assert!(!version_is_newer("1.0.0-rc.1", "1.0.0"));
    }

    #[test]
    fn prerelease_stage_ordering() {
        // alpha < beta < rc (ASCII lexical for alphanumeric identifiers).
        assert!(version_is_newer("1.0.0-beta.1", "1.0.0-alpha.11"));
        assert!(version_is_newer("1.0.0-rc.1", "1.0.0-beta.9"));
        assert!(!version_is_newer("1.0.0-alpha.99", "1.0.0-beta.1"));
    }

    #[test]
    fn numeric_identifier_ranks_below_alphanumeric() {
        // SemVer §11: a numeric identifier has lower precedence than an
        // alphanumeric one in the same position.
        assert!(version_is_newer("1.0.0-alpha", "1.0.0-1"));
        assert!(!version_is_newer("1.0.0-1", "1.0.0-alpha"));
    }

    #[test]
    fn longer_prerelease_set_wins_when_prefixes_match() {
        // SemVer §11: a larger set of pre-release fields outranks a smaller one
        // when all preceding identifiers are equal.
        assert!(version_is_newer("1.0.0-alpha.1.1", "1.0.0-alpha.1"));
        assert!(!version_is_newer("1.0.0-alpha.1", "1.0.0-alpha.1.1"));
    }

    #[test]
    fn build_metadata_is_ignored() {
        // Build metadata (`+...`) does not affect precedence (SemVer §10).
        assert!(!version_is_newer("1.0.0+build.5", "1.0.0+build.1"));
        assert!(version_is_newer("1.0.1+build.1", "1.0.0+build.9"));
    }

    #[test]
    fn current_version_valid() {
        assert!(CURRENT_VERSION.len() > 3); // e.g. "1.0.0"
        assert!(CURRENT_VERSION.contains('.'));
    }

    #[test]
    fn ota_manager_paths() {
        let mgr = OtaManager::new(Path::new("/opt/geniepod"));
        assert_eq!(mgr.install_dir, PathBuf::from("/opt/geniepod/bin"));
        assert_eq!(mgr.staging_dir, PathBuf::from("/opt/geniepod/staging"));
        assert_eq!(mgr.backup_dir, PathBuf::from("/opt/geniepod/backup"));
    }
}
