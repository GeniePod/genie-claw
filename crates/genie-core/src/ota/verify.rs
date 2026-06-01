//! Ed25519 signature verification for OTA release manifests.
//!
//! Release manifests are `sha256sums`-style files: one `<hex_hash>  <filename>`
//! line per binary asset. The manifest bytes are signed with an Ed25519 key
//! held by the release author; the corresponding public key is stored at
//! `[ota].public_key_path` on the device (out of the attacker-writable update
//! directories).
//!
//! Verification fails closed: a missing key file, an unparseable key, a
//! malformed signature, or a signature that does not validate over the exact
//! manifest bytes all return `Err`. A caller that skips this step before
//! writing files to disk has no integrity guarantee on what it downloaded.
//!
//! The `TrustedKeys` type from `crates/genie-core/src/skills/signature.rs`
//! already implements the correct Ed25519 strict-mode verify; this module
//! re-uses that primitive so the OTA path shares the same cryptographic logic
//! as the skill loader.

use std::path::Path;

use anyhow::{Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};

/// A parsed and validated Ed25519 public key for verifying OTA manifests.
pub struct OtaVerifyingKey {
    key: VerifyingKey,
    /// Key identifier (typically the stem of the `.pub` filename).
    pub key_id: String,
}

impl OtaVerifyingKey {
    /// Load the public key from `path`.
    ///
    /// The file must contain the base64-encoded 32-byte Ed25519 public key,
    /// optionally surrounded by whitespace. Returns `Err` if the file is
    /// missing, unreadable, or not a valid key — never a "trusted" posture with
    /// no actual key material.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            bail!(
                "OTA public key not found at {}; set [ota].public_key_path in geniepod.toml",
                path.display()
            );
        }
        let contents = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("failed to read OTA public key {}: {}", path.display(), e)
        })?;
        let key = decode_verifying_key(contents.trim()).ok_or_else(|| {
            anyhow::anyhow!(
                "OTA public key at {} is not a valid base64-encoded Ed25519 public key (need 32 bytes)",
                path.display()
            )
        })?;
        let key_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("ota")
            .to_string();
        Ok(Self { key, key_id })
    }

    /// Verify that `signature_b64` is a valid Ed25519 signature over `message`.
    ///
    /// `signature_b64` is the base64-encoded 64-byte detached signature,
    /// typically the content of `checksums.sha256.sig` from the release page.
    /// Uses strict verification to reject malleable / non-canonical signatures.
    ///
    /// Returns `Ok(())` on success, `Err` with a descriptive message on any
    /// failure. Callers must propagate the error and must not proceed with the
    /// update if this returns `Err`.
    pub fn verify(&self, message: &[u8], signature_b64: &str) -> Result<()> {
        let sig_bytes = BASE64
            .decode(signature_b64.trim())
            .map_err(|e| anyhow::anyhow!("OTA manifest signature is not valid base64: {e}"))?;
        let signature = Signature::from_slice(&sig_bytes).map_err(|e| {
            anyhow::anyhow!(
                "OTA manifest signature has wrong length ({} bytes, need 64): {e}",
                sig_bytes.len()
            )
        })?;
        self.key
            .verify_strict(message, &signature)
            .map_err(|_| anyhow::anyhow!("OTA manifest signature verification failed — the manifest may have been tampered with or was not signed by the trusted key ({})", self.key_id))
    }
}

/// Parse a `sha256sums`-style manifest into `(filename, hex_sha256)` pairs.
///
/// Each line must be `<64-hex-chars>  <filename>` (two spaces between hash and
/// name, as produced by `sha256sum`). Blank lines and lines starting with `#`
/// are ignored. Returns `Err` if any non-blank, non-comment line is malformed.
pub fn parse_manifest(manifest: &str) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    for (line_no, line) in manifest.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // sha256sum format: "<hash>  <filename>" (two spaces).
        let (hash, filename) = line.split_once("  ").ok_or_else(|| {
            anyhow::anyhow!(
                "manifest line {} is malformed (expected '<hash>  <filename>'): {:?}",
                line_no + 1,
                line
            )
        })?;
        let hash = hash.trim();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!(
                "manifest line {}: hash {:?} is not a 64-hex-character SHA-256 digest",
                line_no + 1,
                hash
            );
        }
        entries.push((filename.trim().to_string(), hash.to_string()));
    }
    Ok(entries)
}

fn decode_verifying_key(b64: &str) -> Option<VerifyingKey> {
    let bytes = BASE64.decode(b64).ok()?;
    let array: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn write_pub_key(path: &Path, key: &VerifyingKey) {
        std::fs::write(path, BASE64.encode(key.to_bytes())).unwrap();
    }

    #[test]
    fn load_and_verify_valid_signature() {
        let dir = std::env::temp_dir().join(format!("geniepod-ota-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sk = signing_key(42);
        let key_path = dir.join("ota.pub");
        write_pub_key(&key_path, &sk.verifying_key());

        let verifier = OtaVerifyingKey::load(&key_path).unwrap();
        let manifest = b"abc123...  genie-core\n";
        let sig = BASE64.encode(sk.sign(manifest).to_bytes());

        assert!(verifier.verify(manifest, &sig).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampered_manifest_fails_verification() {
        let dir = std::env::temp_dir().join(format!("geniepod-ota-tamper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sk = signing_key(42);
        let key_path = dir.join("ota.pub");
        write_pub_key(&key_path, &sk.verifying_key());

        let verifier = OtaVerifyingKey::load(&key_path).unwrap();
        let manifest = b"abc123...  genie-core\n";
        let sig = BASE64.encode(sk.sign(manifest).to_bytes());

        assert!(verifier.verify(b"tampered  genie-core\n", &sig).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_key_file_returns_err() {
        let result = OtaVerifyingKey::load(Path::new("/nonexistent/ota.pub"));
        match result {
            Ok(_) => panic!("expected Err, got Ok"),
            Err(e) => assert!(e.to_string().contains("not found"), "unexpected error: {e}"),
        }
    }

    #[test]
    fn parse_manifest_ok() {
        let manifest = "# comment\n\nabc123def456abc123def456abc123def456abc123def456abc123def456abc1  genie-core\n\
                        fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210  genie-ctl\n";
        let entries = parse_manifest(manifest).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "genie-core");
        assert_eq!(entries[1].0, "genie-ctl");
    }

    #[test]
    fn parse_manifest_rejects_malformed_line() {
        let manifest = "not-a-valid-line\n";
        assert!(parse_manifest(manifest).is_err());
    }

    #[test]
    fn parse_manifest_rejects_short_hash() {
        let manifest = "abc123  genie-core\n";
        assert!(parse_manifest(manifest).is_err());
    }
}
