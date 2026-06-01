pub mod verify;

use anyhow::{Result, bail};
use genie_common::config::OtaConfig;
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
/// 5. Verify SHA-256 digest of each binary against signed manifest
/// 6. Verify Ed25519 signature on manifest using pinned public key
/// 7. Stop services, replace binaries via atomic rename, restart services
///
/// Safety:
/// - Old binaries backed up before replacement
/// - Rollback if new binary fails health check within 60s
/// - Governor pauses mode switching during update
/// - Manifest signature verified before any binary is written to install dir

const GITHUB_REPO: &str = "GeniePod/genie-claw";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Names of the GeniePod binaries managed by OTA.
const MANAGED_BINARIES: &[&str] = &[
    "genie-core",
    "genie-ctl",
    "genie-governor",
    "genie-health",
    "genie-api",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub version: String,
    pub published_at: String,
    /// URL of the aarch64 binary asset (single tarball or executable).
    pub download_url: Option<String>,
    /// URL of the `checksums.sha256` manifest for this release.
    pub checksum_url: Option<String>,
    /// URL of the `checksums.sha256.sig` Ed25519 signature for the manifest.
    pub checksum_sig_url: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub last_check: Option<String>,
    /// True when the release has both a checksum manifest and a signature file
    /// in its assets — i.e. the apply pipeline can run without skipping verify.
    pub signed_release: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub version: String,
    pub binaries_replaced: Vec<String>,
    pub backed_up: bool,
    pub verified: bool,
}

pub struct OtaManager {
    install_dir: PathBuf,
    staging_dir: PathBuf,
    backup_dir: PathBuf,
    config: OtaConfig,
}

impl OtaManager {
    pub fn new(base_dir: &Path, config: OtaConfig) -> Self {
        Self {
            install_dir: base_dir.join("bin"),
            staging_dir: base_dir.join("staging"),
            backup_dir: base_dir.join("backup"),
            config,
        }
    }

    /// Check GitHub Releases for a newer version.
    pub async fn check_update(&self) -> Result<UpdateStatus> {
        let latest = self.fetch_latest_release().await;

        let (latest_version, update_available, signed_release) = match &latest {
            Ok(release) => {
                let latest_ver = release.version.clone();
                let is_newer = version_is_newer(&latest_ver, CURRENT_VERSION);
                let signed = release.checksum_url.is_some() && release.checksum_sig_url.is_some();
                (Some(latest_ver), is_newer, signed)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to check for updates");
                (None, false, false)
            }
        };

        Ok(UpdateStatus {
            current_version: CURRENT_VERSION.to_string(),
            latest_version,
            update_available,
            last_check: Some(now_iso()),
            signed_release,
        })
    }

    /// Full apply pipeline: download → verify manifest → verify signatures →
    /// backup current → atomic rename.
    ///
    /// Returns `Err` if:
    /// - OTA is disabled in config (`[ota].enabled = false`)
    /// - No update is available
    /// - The release has no checksum manifest or signature asset
    /// - SHA-256 verification fails for any binary
    /// - Ed25519 signature verification fails
    /// - Any filesystem operation fails
    ///
    /// On any failure after backup, call `rollback()` to restore the previous
    /// binaries. This function does NOT call rollback itself because the caller
    /// may want to log additional context before deciding to roll back.
    pub async fn apply_update(&self) -> Result<ApplyResult> {
        if !self.config.enabled {
            bail!(
                "OTA apply is disabled ([ota].enabled = false in geniepod.toml); \
                 set enabled = true and configure a public key to unlock apply"
            );
        }

        let status = self.check_update().await?;
        if !status.update_available {
            bail!("no update available (current version is already latest)");
        }

        let release = self.fetch_latest_release().await?;

        let checksum_url = release.checksum_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "release {} has no checksums.sha256 asset; cannot verify binaries before install",
                release.tag_name
            )
        })?;
        let checksum_sig_url = release.checksum_sig_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "release {} has no checksums.sha256.sig asset; cannot verify manifest signature",
                release.tag_name
            )
        })?;

        tracing::info!(version = %release.version, "OTA apply started");

        // Step 1: prepare staging / backup directories.
        self.prepare_staging().await?;

        // Step 2: download and verify the manifest and its signature before
        // downloading any binary. If the manifest is not authentic, we stop
        // immediately without touching disk.
        let manifest_bytes = self
            .download_to_bytes(checksum_url, "checksums.sha256")
            .await?;
        let sig_bytes = self
            .download_to_bytes(checksum_sig_url, "checksums.sha256.sig")
            .await?;
        let sig_b64 = String::from_utf8(sig_bytes)
            .map_err(|_| anyhow::anyhow!("checksums.sha256.sig contains non-UTF-8 bytes"))?;

        let verifier = verify::OtaVerifyingKey::load(&self.config.public_key_path)?;
        verifier.verify(&manifest_bytes, sig_b64.trim())?;
        tracing::info!(key_id = %verifier.key_id, "manifest signature verified");

        // Step 3: parse the manifest to discover which binaries are available
        // and what their expected SHA-256 digests are.
        let manifest_str = String::from_utf8(manifest_bytes.clone())
            .map_err(|_| anyhow::anyhow!("checksums.sha256 contains non-UTF-8 bytes"))?;
        let manifest_entries = verify::parse_manifest(&manifest_str)?;

        // Step 4: download each managed binary that appears in the manifest and
        // verify its digest. Binaries absent from the manifest are skipped.
        let mut verified_binaries: Vec<String> = Vec::new();
        for binary_name in MANAGED_BINARIES {
            let Some((_, expected_hash)) = manifest_entries
                .iter()
                .find(|(name, _)| name.contains(binary_name))
            else {
                tracing::debug!(binary = binary_name, "not in manifest; skipping");
                continue;
            };

            let download_url = release.download_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!("release has no download_url for binary {binary_name}")
            })?;

            // The download_url points to a tarball. Derive the individual
            // binary URL by replacing the tarball name with the binary name.
            // If the release ships individual binaries, the url is used directly.
            let binary_url = derive_binary_url(download_url, binary_name);
            let staging_path = self.staging_dir.join(binary_name);
            self.download_file(&binary_url, &staging_path, binary_name)
                .await?;
            self.verify_sha256(&staging_path, expected_hash, binary_name)
                .await?;
            verified_binaries.push(binary_name.to_string());
        }

        if verified_binaries.is_empty() {
            bail!(
                "manifest contained no entries matching the managed binary names {:?}; \
                 cannot proceed",
                MANAGED_BINARIES
            );
        }

        // Step 5: backup current binaries so rollback can restore them.
        self.backup_current().await?;
        tracing::info!("current binaries backed up");

        // Step 6: atomically replace each verified binary via rename(2).
        // On Linux this is atomic when staging and install are on the same
        // filesystem — the rename either fully happens or does not.
        let mut replaced: Vec<String> = Vec::new();
        for binary_name in &verified_binaries {
            let src = self.staging_dir.join(binary_name);
            let dst = self.install_dir.join(binary_name);
            tokio::fs::rename(&src, &dst).await.map_err(|e| {
                anyhow::anyhow!(
                    "atomic rename failed for {}: {} -> {}: {e}",
                    binary_name,
                    src.display(),
                    dst.display()
                )
            })?;
            tracing::info!(binary = binary_name, "replaced");
            replaced.push(binary_name.clone());
        }

        tracing::info!(
            version = %release.version,
            replaced = replaced.len(),
            "OTA apply complete"
        );

        Ok(ApplyResult {
            version: release.version,
            binaries_replaced: replaced,
            backed_up: true,
            verified: true,
        })
    }

    /// Fetch latest release info from GitHub Releases API.
    async fn fetch_latest_release(&self) -> Result<ReleaseInfo> {
        let path = format!("/repos/{}/releases/latest", GITHUB_REPO);
        let body = github_api_get(&path, self.config.network_timeout_secs).await?;
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

        // Scan the assets array once; collect relevant URLs by filename pattern.
        let assets = release
            .get("assets")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut download_url: Option<String> = None;
        let mut checksum_url: Option<String> = None;
        let mut checksum_sig_url: Option<String> = None;

        for asset in &assets {
            let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url = asset
                .get("browser_download_url")
                .and_then(|v| v.as_str())
                .map(String::from);

            if name == "checksums.sha256" {
                checksum_url = url;
            } else if name == "checksums.sha256.sig" {
                checksum_sig_url = url;
            } else if (name.contains("aarch64") || name.contains("arm64")) && download_url.is_none()
            {
                // First aarch64 asset wins as the primary binary download.
                download_url = url;
            }
        }

        Ok(ReleaseInfo {
            tag_name: tag,
            version,
            published_at: published,
            download_url,
            checksum_url,
            checksum_sig_url,
            body: body_text,
        })
    }

    /// Get current version.
    pub fn current_version(&self) -> &str {
        CURRENT_VERSION
    }

    /// Create staging and backup directories.
    pub async fn prepare_staging(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.staging_dir).await?;
        tokio::fs::create_dir_all(&self.backup_dir).await?;
        Ok(())
    }

    /// Backup current binaries before update.
    ///
    /// Requires `prepare_staging()` to have been called first so the backup
    /// directory exists. Logs a warning for binaries that are absent from the
    /// install directory rather than failing, so a partial install does not
    /// block the backup of the binaries that are present.
    pub async fn backup_current(&self) -> Result<()> {
        // Guard: the backup dir must exist. If prepare_staging() was skipped the
        // copy calls below would fail silently because tokio::fs::copy returns
        // Ok(0) on a missing *destination* directory on some platforms.
        if !self.backup_dir.exists() {
            bail!(
                "backup directory {} does not exist; call prepare_staging() first",
                self.backup_dir.display()
            );
        }

        for bin in MANAGED_BINARIES {
            let src = self.install_dir.join(bin);
            let dst = self.backup_dir.join(bin);
            if src.exists() {
                tokio::fs::copy(&src, &dst)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to back up {}: {e}", src.display()))?;
                tracing::debug!(binary = bin, "backed up");
            } else {
                tracing::warn!(
                    binary = bin,
                    path = %src.display(),
                    "binary not found in install dir; skipping backup"
                );
            }
        }

        Ok(())
    }

    /// Rollback to backed-up binaries.
    ///
    /// Called automatically by the caller when `apply_update()` fails after
    /// `backup_current()` has run. Non-fatal per binary: logs and continues if
    /// a backup binary is missing, so a partial rollback is still attempted.
    pub async fn rollback(&self) -> Result<()> {
        tracing::warn!("OTA rollback: restoring previous binaries");
        let mut errors: Vec<String> = Vec::new();

        for bin in MANAGED_BINARIES {
            let src = self.backup_dir.join(bin);
            let dst = self.install_dir.join(bin);
            if src.exists() {
                if let Err(e) = tokio::fs::copy(&src, &dst).await {
                    errors.push(format!("{bin}: {e}"));
                } else {
                    tracing::info!(binary = bin, "rolled back");
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            bail!("rollback partially failed: {}", errors.join("; "))
        }
    }

    /// Download `url` to `dest_path` using curl with a bounded timeout.
    async fn download_file(&self, url: &str, dest_path: &Path, label: &str) -> Result<()> {
        tracing::info!(url = %url, dest = %dest_path.display(), "downloading {}", label);
        let output = tokio::process::Command::new("curl")
            .args([
                "-fsSL",
                "--max-time",
                &self.config.network_timeout_secs.to_string(),
                "--output",
                dest_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("dest path is not valid UTF-8"))?,
                url,
            ])
            .output()
            .await?;

        if !output.status.success() {
            bail!(
                "curl download of {} failed: {}",
                label,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    /// Download `url` into an in-memory `Vec<u8>` — used for small manifest and
    /// signature files where streaming to disk is unnecessary.
    async fn download_to_bytes(&self, url: &str, label: &str) -> Result<Vec<u8>> {
        tracing::debug!(url = %url, "downloading {}", label);
        let output = tokio::process::Command::new("curl")
            .args([
                "-fsSL",
                "--max-time",
                &self.config.network_timeout_secs.to_string(),
                url,
            ])
            .output()
            .await?;

        if !output.status.success() {
            bail!(
                "curl download of {} failed: {}",
                label,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(output.stdout)
    }

    /// Verify the SHA-256 digest of `path` against `expected_hex`.
    async fn verify_sha256(&self, path: &Path, expected_hex: &str, label: &str) -> Result<()> {
        let data = tokio::fs::read(path).await.map_err(|e| {
            anyhow::anyhow!(
                "failed to read {} for hash verification: {e}",
                path.display()
            )
        })?;
        let actual = crate::prompt_sha::sha256_hex_bytes(&data);
        if actual != expected_hex {
            bail!(
                "SHA-256 mismatch for {label}: expected {expected_hex}, got {actual}; \
                 the downloaded binary may be corrupted or have been tampered with"
            );
        }
        tracing::debug!(binary = label, hash = %actual, "SHA-256 verified");
        Ok(())
    }
}

/// Derive the individual binary asset URL from the primary download URL.
///
/// When the release ships a single tarball, this replaces the tarball filename
/// in the URL with the binary name so the correct asset is fetched. When the
/// release already ships the binary by name, the URL is returned unchanged.
fn derive_binary_url(download_url: &str, binary_name: &str) -> String {
    // If the URL path already ends with the binary name, use it as-is.
    if download_url.split('/').next_back() == Some(binary_name) {
        return download_url.to_string();
    }
    // Otherwise replace the last path segment with the binary name.
    // This handles the common pattern of a base URL shared between assets.
    let base = download_url
        .rsplit_once('/')
        .map(|(b, _)| b)
        .unwrap_or(download_url);
    format!("{base}/{binary_name}")
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

/// GET request to GitHub API (api.github.com) with a bounded timeout.
///
/// `timeout_secs` is threaded from `[ota].network_timeout_secs` so every call
/// honours the operator's configured ceiling rather than blocking indefinitely
/// (the bug documented in the issue for the original line 219).
async fn github_api_get(path: &str, timeout_secs: u64) -> Result<String> {
    let url = format!("https://api.github.com{}", path);
    let output = tokio::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            &timeout_secs.to_string(),
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: GeniePod-OTA",
            &url,
        ])
        .output()
        .await?;

    if !output.status.success() {
        bail!(
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
        assert!(CURRENT_VERSION.len() > 3);
        assert!(CURRENT_VERSION.contains('.'));
    }

    #[test]
    fn ota_manager_paths() {
        let mgr = OtaManager::new(Path::new("/opt/geniepod"), OtaConfig::default());
        assert_eq!(mgr.install_dir, PathBuf::from("/opt/geniepod/bin"));
        assert_eq!(mgr.staging_dir, PathBuf::from("/opt/geniepod/staging"));
        assert_eq!(mgr.backup_dir, PathBuf::from("/opt/geniepod/backup"));
    }

    #[test]
    fn derive_binary_url_same_name() {
        let url = "https://example.com/releases/genie-core";
        assert_eq!(derive_binary_url(url, "genie-core"), url);
    }

    #[test]
    fn derive_binary_url_replaces_tarball() {
        let url = "https://example.com/releases/genie-claw-1.0.0-aarch64.tar.gz";
        let result = derive_binary_url(url, "genie-ctl");
        assert_eq!(result, "https://example.com/releases/genie-ctl");
    }

    #[test]
    fn derive_binary_url_replaces_last_segment() {
        let base = "https://github.com/GeniePod/genie-claw/releases/download/v1.2.0";
        let url = format!("{base}/genie-claw-aarch64.tar.gz");
        let result = derive_binary_url(&url, "genie-core");
        assert_eq!(result, format!("{base}/genie-core"));
    }
}
