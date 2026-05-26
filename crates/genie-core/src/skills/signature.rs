//! Detached Ed25519 verification for native skill `.so` modules.
//!
//! Authenticity of a skill must be derived from a signature over the bytes
//! that will actually be `dlopen`'d — never from the presence of a string the
//! same party supplied. A trusted public key (shipped with the distribution,
//! out of the attacker-writable skills directory) signs the `.so`; the loader
//! verifies that signature against the exact file bytes before loading.
//!
//! Key files: `<key_id>.pub` in the trusted-key directory, each containing the
//! base64-encoded 32-byte Ed25519 public key. The file stem is the key id.
//! Manifests carry the base64-encoded 64-byte detached signature plus the
//! `key_id` selecting which trusted key produced it.

use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};

/// A set of trusted Ed25519 public keys, indexed by key id.
///
/// Verification fails closed: an unknown key id, a malformed signature, or a
/// signature that does not validate over the supplied bytes all return
/// `false`. An empty set rejects everything.
#[derive(Debug, Default)]
pub struct TrustedKeys {
    keys: HashMap<String, VerifyingKey>,
}

impl TrustedKeys {
    /// An empty key set. Verifies nothing (fail closed).
    pub fn empty() -> Self {
        Self {
            keys: HashMap::new(),
        }
    }

    /// Load every `*.pub` file in `dir` as a trusted key.
    ///
    /// The file stem is the key id; the file contents are the base64-encoded
    /// 32-byte Ed25519 public key. A missing directory yields an empty set.
    /// Malformed key files are skipped with a warning rather than aborting,
    /// so one bad file cannot block every other trusted key.
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut keys = HashMap::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => {
                tracing::debug!(dir = %dir.display(), "skill key directory not found");
                return Self { keys };
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("pub") {
                continue;
            }
            let Some(key_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|contents| decode_verifying_key(contents.trim()))
            {
                Some(key) => {
                    keys.insert(key_id.to_string(), key);
                }
                None => {
                    tracing::warn!(
                        path = %path.display(),
                        "ignoring malformed skill public key"
                    );
                }
            }
        }

        Self { keys }
    }

    /// Number of trusted keys loaded.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True if no trusted keys are loaded — every verification fails closed.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Verify a base64-encoded detached signature over `message` using the
    /// trusted key named `key_id`.
    ///
    /// Returns `true` only when `key_id` names a trusted key, the signature
    /// decodes, and it validates over `message`. Any failure — empty inputs,
    /// unknown key, malformed signature, or invalid signature — returns
    /// `false`. Uses strict verification to reject malleable / non-canonical
    /// signatures.
    pub fn verify_detached(&self, key_id: &str, message: &[u8], signature_b64: &str) -> bool {
        let key_id = key_id.trim();
        let signature_b64 = signature_b64.trim();
        if key_id.is_empty() || signature_b64.is_empty() {
            return false;
        }

        let Some(key) = self.keys.get(key_id) else {
            return false;
        };
        let Ok(sig_bytes) = BASE64.decode(signature_b64) else {
            return false;
        };
        let Ok(signature) = Signature::from_slice(&sig_bytes) else {
            return false;
        };

        key.verify_strict(message, &signature).is_ok()
    }
}

/// Decode a base64-encoded 32-byte Ed25519 public key into a [`VerifyingKey`].
fn decode_verifying_key(b64: &str) -> Option<VerifyingKey> {
    let bytes = BASE64.decode(b64).ok()?;
    let array: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Deterministic signing key from a fixed seed — no RNG needed in tests.
    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn write_pub_key(dir: &Path, key_id: &str, key: &VerifyingKey) {
        std::fs::write(
            dir.join(format!("{key_id}.pub")),
            BASE64.encode(key.to_bytes()),
        )
        .unwrap();
    }

    #[test]
    fn empty_keys_reject_everything() {
        let keys = TrustedKeys::empty();
        assert!(keys.is_empty());
        assert!(!keys.verify_detached("any", b"payload", "c2ln"));
    }

    #[test]
    fn valid_signature_verifies() {
        let dir = std::env::temp_dir().join(format!("geniepod-keys-valid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sk = signing_key(7);
        write_pub_key(&dir, "trusted", &sk.verifying_key());

        let keys = TrustedKeys::load_from_dir(&dir);
        assert_eq!(keys.len(), 1);

        let message = b"the exact bytes that will run";
        let sig = BASE64.encode(sk.sign(message).to_bytes());
        assert!(keys.verify_detached("trusted", message, &sig));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn junk_signature_is_rejected() {
        let dir = std::env::temp_dir().join(format!("geniepod-keys-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sk = signing_key(7);
        write_pub_key(&dir, "trusted", &sk.verifying_key());

        let keys = TrustedKeys::load_from_dir(&dir);
        // The pre-fix bug: any non-empty string counted as "signed". A trusted
        // key must reject an arbitrary string.
        assert!(!keys.verify_detached("trusted", b"payload", "x"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampered_message_is_rejected() {
        let dir = std::env::temp_dir().join(format!("geniepod-keys-tamper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sk = signing_key(7);
        write_pub_key(&dir, "trusted", &sk.verifying_key());
        let keys = TrustedKeys::load_from_dir(&dir);

        let sig = BASE64.encode(sk.sign(b"original bytes").to_bytes());
        assert!(!keys.verify_detached("trusted", b"original bytez", &sig));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_key_id_is_rejected() {
        let dir =
            std::env::temp_dir().join(format!("geniepod-keys-unknown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sk = signing_key(7);
        write_pub_key(&dir, "trusted", &sk.verifying_key());
        let keys = TrustedKeys::load_from_dir(&dir);

        let message = b"payload";
        let sig = BASE64.encode(sk.sign(message).to_bytes());
        // Correct signature, but the manifest names a key we do not trust.
        assert!(!keys.verify_detached("attacker", message, &sig));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_trusted_key_is_rejected() {
        let dir = std::env::temp_dir().join(format!("geniepod-keys-wrong-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Trust key A, but sign with key B and claim key id "a".
        write_pub_key(&dir, "a", &signing_key(1).verifying_key());
        let keys = TrustedKeys::load_from_dir(&dir);

        let message = b"payload";
        let sig = BASE64.encode(signing_key(2).sign(message).to_bytes());
        assert!(!keys.verify_detached("a", message, &sig));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_key_file_is_skipped() {
        let dir = std::env::temp_dir().join(format!("geniepod-keys-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("broken.pub"), "not base64 !!!").unwrap();
        let sk = signing_key(3);
        write_pub_key(&dir, "good", &sk.verifying_key());

        let keys = TrustedKeys::load_from_dir(&dir);
        // Broken file skipped; the good key still loads and verifies.
        assert_eq!(keys.len(), 1);
        let message = b"payload";
        let sig = BASE64.encode(sk.sign(message).to_bytes());
        assert!(keys.verify_detached("good", message, &sig));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_keys_do_not_yield_a_trusted_posture() {
        let dir = std::env::temp_dir().join(format!("geniepod-keys-allbad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Every key file in the directory is unparseable: not base64, wrong
        // length, and empty. None becomes a trust anchor.
        std::fs::write(dir.join("garbage.pub"), "not base64 !!!").unwrap();
        std::fs::write(dir.join("shortkey.pub"), BASE64.encode([0u8; 16])).unwrap();
        std::fs::write(dir.join("empty.pub"), "").unwrap();

        let keys = TrustedKeys::load_from_dir(&dir);
        // A directory full of malformed keys must read as zero trust anchors —
        // never as a trusted posture that happens to verify nothing safely.
        assert!(keys.is_empty());
        // Even a syntactically valid signature cannot verify: no usable key.
        let sk = signing_key(4);
        let message = b"payload";
        let sig = BASE64.encode(sk.sign(message).to_bytes());
        assert!(!keys.verify_detached("garbage", message, &sig));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
