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

/// Compare semver strings. Returns true if `latest` is newer than `current`.
fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let clean = s
            .strip_prefix('v')
            .unwrap_or(s)
            .split('-')
            .next()
            .unwrap_or(s);
        let parts: Vec<u32> = clean.split('.').filter_map(|p| p.parse().ok()).collect();
        (
            parts.first().copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        )
    };

    let l = parse(latest);
    let c = parse(current);
    l > c
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
        // Pre-release suffix is stripped for comparison.
        assert!(version_is_newer("1.1.0-alpha.1", "1.0.0-alpha.1"));
        assert!(!version_is_newer("1.0.0-alpha.2", "1.0.0-alpha.1"));
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
