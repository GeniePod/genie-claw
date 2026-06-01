//! Dynamic skill loader — dlopen wrapper for `.so` skill modules.
//!
//! Like Linux's `insmod`, loads a shared library, finds the
//! `genie_skill_init` symbol, and extracts the SkillVTable.

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use genie_common::config::SkillPolicyConfig;
use genie_skill_sdk::{ABI_VERSION, SkillVTable};
use libloading::{Library, Symbol};
use serde::{Deserialize, Serialize};

use crate::skills::signature::TrustedKeys;

/// Optional sidecar metadata for a native skill.
///
/// A skill named `hello.so` can declare metadata in `hello.skill.json`.
/// `signature`/`key_id` are cryptographic material: a detached Ed25519
/// signature over the `.so` bytes, verified by the loader against a trusted
/// public key before the library is loaded.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillManifest {
    /// Expected tool name exposed by the vtable.
    pub name: String,
    /// Expected semantic version exposed by the vtable.
    pub version: String,
    /// Human-readable manifest description.
    pub description: String,
    /// Permission labels requested by the skill, e.g. `network.http`.
    pub permissions: Vec<String>,
    /// Capability labels exposed for operators, e.g. `music.playback`.
    pub capabilities: Vec<String>,
    /// Reviewer identity or process name.
    pub reviewed_by: String,
    /// Base64-encoded detached Ed25519 signature over the `.so` bytes. Verified
    /// against the trusted key named by `key_id`; not trusted by presence.
    pub signature: String,
    /// Id of the trusted public key that produced `signature` (the `.pub`
    /// file stem in the trusted-key directory).
    pub key_id: String,
}

/// Audit view of the manifest state for a loaded skill.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillManifestAudit {
    pub status: String,
    pub path: Option<PathBuf>,
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<String>,
    pub capabilities: Vec<String>,
    pub reviewed_by: String,
    /// Id of the trusted key claimed by the manifest (audit only).
    pub key_id: String,
    /// True only when a trusted key cryptographically verified the signature
    /// over the `.so` bytes — never set by mere presence of a string.
    pub signed: bool,
    pub error: String,
}

/// Runtime load policy for native skills.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkillLoadPolicy {
    pub require_manifest: bool,
    pub require_signature: bool,
    pub denied_permissions: Vec<String>,
    /// Directory of trusted Ed25519 public keys used to verify skill
    /// signatures. Lives outside the (attacker-writable) skills directory.
    pub signature_key_dir: PathBuf,
    /// Deadline for a single skill invocation, in milliseconds. Enforced by
    /// [`LoadedSkill::execute_parsed`] / [`SkillInvocation::run`] so a hung
    /// native call never starves the async executor.
    pub skill_execution_timeout_ms: u64,
}

impl Default for SkillLoadPolicy {
    fn default() -> Self {
        let defaults = SkillPolicyConfig::default();
        Self {
            require_manifest: false,
            require_signature: false,
            denied_permissions: Vec::new(),
            signature_key_dir: defaults.signature_key_dir,
            skill_execution_timeout_ms: defaults.skill_execution_timeout_ms,
        }
    }
}

impl From<&SkillPolicyConfig> for SkillLoadPolicy {
    fn from(config: &SkillPolicyConfig) -> Self {
        Self {
            require_manifest: config.require_manifest,
            require_signature: config.require_signature,
            denied_permissions: config.denied_permissions.clone(),
            signature_key_dir: config.signature_key_dir.clone(),
            skill_execution_timeout_ms: config.skill_execution_timeout_ms,
        }
    }
}

impl SkillManifestAudit {
    fn missing() -> Self {
        Self {
            status: "missing".into(),
            path: None,
            name: String::new(),
            version: String::new(),
            description: String::new(),
            permissions: Vec::new(),
            capabilities: Vec::new(),
            reviewed_by: String::new(),
            key_id: String::new(),
            signed: false,
            error: "no sidecar manifest found".into(),
        }
    }

    fn invalid(path: PathBuf, error: String) -> Self {
        Self {
            status: "invalid".into(),
            path: Some(path),
            name: String::new(),
            version: String::new(),
            description: String::new(),
            permissions: Vec::new(),
            capabilities: Vec::new(),
            reviewed_by: String::new(),
            key_id: String::new(),
            signed: false,
            error,
        }
    }

    /// Build an audit from a parsed manifest. `signed` is the result of
    /// cryptographic verification performed by the loader against the `.so`
    /// bytes — it is decided here, never from the presence of the signature
    /// string.
    fn from_manifest(
        path: PathBuf,
        manifest: SkillManifest,
        signed: bool,
        loaded_name: &str,
        loaded_version: &str,
    ) -> Self {
        let mut problems = Vec::new();

        if manifest.name.trim().is_empty() {
            problems.push("manifest name is empty".to_string());
        } else if manifest.name != loaded_name {
            problems.push(format!(
                "manifest name '{}' does not match loaded skill '{}'",
                manifest.name, loaded_name
            ));
        }

        if manifest.version.trim().is_empty() {
            problems.push("manifest version is empty".to_string());
        } else if manifest.version != loaded_version {
            problems.push(format!(
                "manifest version '{}' does not match loaded skill '{}'",
                manifest.version, loaded_version
            ));
        }

        let status = if problems.is_empty() {
            "ok"
        } else {
            "mismatch"
        };

        Self {
            status: status.into(),
            path: Some(path),
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            permissions: manifest.permissions,
            capabilities: manifest.capabilities,
            reviewed_by: manifest.reviewed_by,
            key_id: manifest.key_id,
            signed,
            error: problems.join("; "),
        }
    }
}

/// A loaded skill module — holds the .so library handle and vtable reference.
pub struct LoadedSkill {
    /// Skill name (from vtable).
    pub name: String,
    /// Skill description (from vtable).
    pub description: String,
    /// Skill version (from vtable).
    pub version: String,
    /// Parameter JSON schema (from vtable).
    pub parameters_json: String,
    /// Path to the .so file.
    pub path: PathBuf,
    /// Optional sidecar manifest audit metadata.
    pub manifest: SkillManifestAudit,
    /// Number of faults (panics/errors). Auto-unloaded after 3.
    pub fault_count: u32,
    /// Per-invocation execution deadline, derived from the load policy.
    execution_timeout: Duration,
    /// The vtable pointer (valid for lifetime of `lib`).
    vtable: *const SkillVTable,
    /// Library handle — must stay alive as long as the vtable (and any function
    /// pointer copied out of it) is used. An `Arc` so an in-flight blocking
    /// call can keep the `.so` mapped even if the skill is unloaded meanwhile.
    lib: Arc<Library>,
}

// Safety: the `vtable` raw pointer makes LoadedSkill `!Send`/`!Sync` by default.
// The pointer and the `Arc<Library>` are valid for the lifetime of the struct,
// and skill invocations only copy out the C function pointers (themselves
// `Send`) plus an `Arc<Library>` clone before crossing a thread boundary — see
// `SkillInvocation`. The struct itself is only mutated from the single-threaded
// runtime while held behind the dispatcher's mutex.
unsafe impl Send for LoadedSkill {}
unsafe impl Sync for LoadedSkill {}

/// C ABI entry points for one skill, copied out of the vtable. Bare function
/// pointers are `Send`, so unlike the raw `*const SkillVTable` they can safely
/// cross to a blocking-pool thread.
type SkillExecuteFn = extern "C" fn(args_json: *const c_char) -> *mut c_char;
type SkillDestroyFn = extern "C" fn(ptr: *mut c_char);

/// A self-contained, `Send` handle that runs a single skill invocation off the
/// async executor.
///
/// It carries an `Arc<Library>` clone so the native code stays mapped for the
/// whole (possibly blocking) call — even if the originating [`LoadedSkill`] is
/// unloaded from the [`SkillLoader`] while the blocking thread is still running.
pub struct SkillInvocation {
    name: String,
    execute_fn: SkillExecuteFn,
    destroy_fn: SkillDestroyFn,
    args: CString,
    timeout: Duration,
    // Keeps the `.so` mapped for the duration of the call. Never dereferenced.
    _lib: Arc<Library>,
}

/// Outcome of a skill invocation: the parsed success/output pair plus whether
/// the call should count against the skill's fault budget.
pub struct SkillOutcome {
    pub success: bool,
    pub output: String,
    pub faulted: bool,
}

impl SkillInvocation {
    /// Run the C ABI skill call on a blocking thread, bounded by `timeout`.
    ///
    /// The blocking offload keeps the single-threaded tokio runtime free to
    /// poll `/api/health`, the voice pipeline, Telegram, and every other
    /// `spawn_local` task while the skill runs. On timeout we stop awaiting and
    /// return a fault to the caller; the abandoned blocking thread cannot be
    /// force-cancelled (it owns a raw C call), but it no longer blocks the
    /// executor and the auto-unload-after-3-faults policy reaps a skill that
    /// keeps timing out.
    pub async fn run(self) -> SkillOutcome {
        let SkillInvocation {
            name,
            execute_fn,
            destroy_fn,
            args,
            timeout,
            _lib,
        } = self;

        let join = tokio::task::spawn_blocking(move || {
            // Hold the library mapped for the entire call.
            let _lib = _lib;
            let result_ptr = execute_fn(args.as_ptr());
            if result_ptr.is_null() {
                return None;
            }
            let result = unsafe { CStr::from_ptr(result_ptr) }
                .to_string_lossy()
                .to_string();
            // Free the C string via the skill's destroy function.
            destroy_fn(result_ptr);
            Some(result)
        });

        match tokio::time::timeout(timeout, join).await {
            Ok(Ok(Some(json))) => Self::parse(json),
            Ok(Ok(None)) => SkillOutcome {
                success: false,
                output: format!("skill '{name}' returned null"),
                faulted: true,
            },
            Ok(Err(join_err)) => SkillOutcome {
                success: false,
                output: format!("skill '{name}' panicked: {join_err}"),
                faulted: true,
            },
            Err(_elapsed) => SkillOutcome {
                success: false,
                output: format!(
                    "skill '{name}' exceeded execution timeout of {} ms",
                    timeout.as_millis()
                ),
                faulted: true,
            },
        }
    }

    fn parse(json: String) -> SkillOutcome {
        match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(parsed) => {
                let success = parsed
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let output = parsed
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&json)
                    .to_string();
                SkillOutcome {
                    success,
                    output,
                    faulted: !success,
                }
            }
            // Non-JSON output is treated as opaque success, matching prior behavior.
            Err(_) => SkillOutcome {
                success: true,
                output: json,
                faulted: false,
            },
        }
    }
}

impl LoadedSkill {
    /// Build a `Send` invocation handle for this skill. Synchronous and cheap:
    /// callers can hold the skill-loader lock across this, then drop it before
    /// `await`-ing [`SkillInvocation::run`] so the lock is never held across the
    /// blocking call.
    pub fn prepare(&self, args_json: &str) -> SkillInvocation {
        let vtable = unsafe { &*self.vtable };
        SkillInvocation {
            name: self.name.clone(),
            execute_fn: vtable.execute,
            destroy_fn: vtable.destroy,
            args: CString::new(args_json).unwrap_or_default(),
            timeout: self.execution_timeout,
            _lib: Arc::clone(&self.lib),
        }
    }

    /// Execute the skill and parse the JSON result into success/output.
    ///
    /// The C ABI call runs on a blocking thread under the configured deadline
    /// (see [`SkillInvocation::run`]), so it never freezes the async executor.
    pub async fn execute_parsed(&mut self, args_json: &str) -> (bool, String) {
        let outcome = self.prepare(args_json).run().await;
        if outcome.faulted {
            self.fault_count += 1;
        }
        (outcome.success, outcome.output)
    }

    /// Check if the skill should be auto-unloaded due to repeated faults.
    pub fn should_unload(&self) -> bool {
        self.fault_count >= 3
    }
}

/// Read a C string pointer from the vtable. Returns empty string if null.
unsafe fn read_c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
    }
}

/// Return supported sidecar manifest candidates for a skill shared library.
pub fn manifest_sidecar_candidates(skill_path: &Path) -> Vec<PathBuf> {
    vec![
        skill_path.with_extension("skill.json"),
        skill_path.with_extension("manifest.json"),
        skill_path.with_extension("json"),
    ]
}

/// Find the first sidecar manifest that exists for a skill shared library.
pub fn find_manifest_sidecar(skill_path: &Path) -> Option<PathBuf> {
    manifest_sidecar_candidates(skill_path)
        .into_iter()
        .find(|path| path.exists())
}

/// Parsed state of a skill's sidecar manifest, before it is matched against
/// the loaded vtable. Kept separate from [`SkillManifestAudit`] so signature
/// verification can run on the raw manifest *before* the `.so` is loaded.
enum ManifestSource {
    Missing,
    Invalid {
        path: PathBuf,
        error: String,
    },
    Present {
        path: PathBuf,
        manifest: SkillManifest,
    },
}

fn read_manifest_source(skill_path: &Path) -> ManifestSource {
    let Some(path) = find_manifest_sidecar(skill_path) else {
        return ManifestSource::Missing;
    };

    match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|content| {
            serde_json::from_str::<SkillManifest>(&content).map_err(|e| e.to_string())
        }) {
        Ok(manifest) => ManifestSource::Present { path, manifest },
        Err(error) => ManifestSource::Invalid { path, error },
    }
}

/// Verify the sidecar's detached signature over the exact bytes of the `.so`
/// that will be loaded. Returns `true` only when a trusted key validates the
/// signature over the supplied bytes; fails closed on any verification error.
/// The caller is responsible for reading the file once and passing the same
/// buffer to both this function and the library loader, so that the verified
/// bytes are identical to the bytes that execute.
fn verify_skill_signature(
    so_bytes: &[u8],
    manifest: &SkillManifest,
    trusted_keys: &TrustedKeys,
) -> bool {
    trusted_keys.verify_detached(&manifest.key_id, so_bytes, &manifest.signature)
}

/// Build the audit view once the vtable's name/version are known. `signed`
/// carries the verification result computed before load.
fn build_manifest_audit(
    source: ManifestSource,
    signed: bool,
    loaded_name: &str,
    loaded_version: &str,
) -> SkillManifestAudit {
    match source {
        ManifestSource::Missing => SkillManifestAudit::missing(),
        ManifestSource::Invalid { path, error } => SkillManifestAudit::invalid(path, error),
        ManifestSource::Present { path, manifest } => {
            SkillManifestAudit::from_manifest(path, manifest, signed, loaded_name, loaded_version)
        }
    }
}

/// Post-load policy checks that depend on the loaded vtable (manifest
/// name/version match and requested permissions).
///
/// The signature gate is enforced *before* `dlopen` in [`SkillLoader::load_skill`]
/// — never here — because by the time this runs the native code has already
/// been loaded.
fn enforce_skill_policy(manifest: &SkillManifestAudit, policy: &SkillLoadPolicy) -> Result<()> {
    if policy.require_manifest && manifest.status != "ok" {
        anyhow::bail!(
            "skill manifest required but status is '{}': {}",
            manifest.status,
            manifest.error
        );
    }

    let denied = manifest
        .permissions
        .iter()
        .filter(|permission| policy.denied_permissions.contains(permission))
        .cloned()
        .collect::<Vec<_>>();
    if !denied.is_empty() {
        anyhow::bail!("skill requests denied permission(s): {}", denied.join(", "));
    }

    Ok(())
}

/// Load a shared library from an in-memory buffer via a sealed Linux `memfd`.
///
/// The bytes are written into an anonymous file descriptor, which is then
/// sealed with `F_SEAL_WRITE | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`.
/// Once sealed, no process — including this one — can alter the mapping.
/// `dlopen` is then called on `/proc/self/fd/<n>`, so the kernel loads exactly
/// the bytes that were cryptographically verified, not whatever happens to be
/// on disk at the time.  This closes the TOCTOU window that existed between
/// the path-based `verify_skill_signature` and the previous `Library::new(path)`.
#[cfg(target_os = "linux")]
fn load_library_from_memfd(bytes: &[u8], label: &str) -> Result<Library> {
    use std::io::Write as _;
    use std::os::unix::io::FromRawFd as _;

    let c_label = std::ffi::CString::new(label).unwrap_or_default();
    // SAFETY: memfd_create is a simple syscall; no memory-safety preconditions.
    let fd = unsafe {
        libc::memfd_create(
            c_label.as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        anyhow::bail!("memfd_create failed: {}", std::io::Error::last_os_error());
    }

    // Write the verified bytes.  ManuallyDrop keeps fd open after the File is
    // "dropped" — we need it alive for sealing and dlopen below.
    let write_result = {
        // SAFETY: fd is valid and we own it exclusively at this point.
        let mut file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        file.write_all(bytes)
    };
    if let Err(e) = write_result {
        // SAFETY: fd is still open; close to avoid leaking.
        unsafe { libc::close(fd) };
        return Err(e.into());
    }

    // Seal: prevent any future writes or size changes.  F_SEAL_SEAL prevents
    // further seals from being added after this call.
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    // SAFETY: fd is open; F_ADD_SEALS takes a seals bitmask as its third arg.
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!("memfd sealing failed: {}", std::io::Error::last_os_error());
    }

    // dlopen via /proc/self/fd/<n>.  The dynamic linker mmaps from offset 0
    // regardless of file position; file-position state is irrelevant here.
    // dlopen internally dups the fd, so closing ours after the call is safe.
    let fd_path = format!("/proc/self/fd/{fd}");
    // SAFETY: loading a .so is inherently unsafe; bytes have been verified.
    let lib = unsafe { Library::new(&fd_path) };
    // SAFETY: dlopen has its own reference; close ours to avoid leaking.
    unsafe { libc::close(fd) };

    lib.map_err(|e| anyhow::anyhow!("dlopen from sealed memfd failed: {e}"))
}

/// Skill loader — scans a directory for `.so` files and loads them.
pub struct SkillLoader {
    skills_dir: PathBuf,
    policy: SkillLoadPolicy,
    trusted_keys: TrustedKeys,
    loaded: Vec<LoadedSkill>,
}

impl SkillLoader {
    pub fn new(skills_dir: &Path) -> Self {
        Self::new_with_policy(skills_dir, SkillLoadPolicy::default())
    }

    pub fn new_with_policy(skills_dir: &Path, policy: SkillLoadPolicy) -> Self {
        let trusted_keys = TrustedKeys::load_from_dir(&policy.signature_key_dir);
        if policy.require_signature && trusted_keys.is_empty() {
            tracing::warn!(
                key_dir = %policy.signature_key_dir.display(),
                "require_signature is enabled but no trusted skill keys were found; \
                 every skill will be rejected (fail closed)"
            );
        }
        Self {
            skills_dir: skills_dir.to_path_buf(),
            policy,
            trusted_keys,
            loaded: Vec::new(),
        }
    }

    /// Scan the skills directory and load all `.so` files.
    pub fn load_all(&mut self) -> Vec<String> {
        let mut loaded_names = Vec::new();

        if !self.skills_dir.exists() {
            tracing::debug!(dir = %self.skills_dir.display(), "skills directory not found");
            return loaded_names;
        }

        let entries = match std::fs::read_dir(&self.skills_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read skills directory");
                return loaded_names;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "so") {
                match self.load_skill(&path) {
                    Ok(name) => {
                        tracing::info!(skill = %name, path = %path.display(), "skill loaded");
                        loaded_names.push(name);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to load skill"
                        );
                    }
                }
            }
        }

        loaded_names
    }

    /// Load a single skill from a `.so` file.
    pub fn load_skill(&mut self, path: &Path) -> Result<String> {
        // Read the sidecar manifest and the .so bytes exactly once.
        // Both signature verification and library loading consume the same
        // buffer — eliminating the TOCTOU window that previously existed
        // between a path-based verify and a separate dlopen.
        let manifest_source = read_manifest_source(path);
        let so_bytes = std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;

        let signed = match &manifest_source {
            ManifestSource::Present { manifest, .. } => {
                verify_skill_signature(&so_bytes, manifest, &self.trusted_keys)
            }
            ManifestSource::Missing | ManifestSource::Invalid { .. } => false,
        };

        if self.policy.require_signature && !signed {
            anyhow::bail!(
                "skill signature required but not cryptographically verified for {} \
                 (need a trusted key in {} and a matching detached signature)",
                path.display(),
                self.policy.signature_key_dir.display()
            );
        }

        // Load the library from the exact bytes already read and verified above.
        // On Linux a sealed memfd is used so the kernel loads those bytes and
        // not whatever is on disk when dlopen runs — closing the TOCTOU window.
        // On other platforms we fall back to path-based dlopen (no TOCTOU
        // mitigation; require_signature is a Linux-first production boundary).
        #[cfg(target_os = "linux")]
        let lib = {
            let label = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("genie-skill");
            load_library_from_memfd(&so_bytes, label)?
        };
        #[cfg(not(target_os = "linux"))]
        let lib = unsafe { Library::new(path) }
            .map_err(|e| anyhow::anyhow!("dlopen failed for {}: {}", path.display(), e))?;

        // The .so bytes are no longer needed; free them before vtable extraction.
        drop(so_bytes);

        // Find the entry point.
        let init_fn: Symbol<extern "C" fn() -> *const SkillVTable> =
            unsafe { lib.get(b"genie_skill_init\0") }.map_err(|e| {
                anyhow::anyhow!(
                    "symbol 'genie_skill_init' not found in {}: {}",
                    path.display(),
                    e
                )
            })?;

        let vtable_ptr = init_fn();
        if vtable_ptr.is_null() {
            anyhow::bail!("genie_skill_init returned null for {}", path.display());
        }

        let vtable = unsafe { &*vtable_ptr };

        // Check ABI version.
        if vtable.abi_version != ABI_VERSION {
            anyhow::bail!(
                "ABI version mismatch: skill has {}, core expects {}",
                vtable.abi_version,
                ABI_VERSION
            );
        }

        let name = unsafe { read_c_str(vtable.name) };
        let description = unsafe { read_c_str(vtable.description) };
        let version = unsafe { read_c_str(vtable.version) };
        let parameters_json = unsafe { read_c_str(vtable.parameters_json) };

        if name.is_empty() {
            anyhow::bail!("skill in {} has empty name", path.display());
        }
        if description.is_empty() {
            anyhow::bail!("skill '{}' has empty description", name);
        }
        if serde_json::from_str::<serde_json::Value>(&parameters_json).is_err() {
            anyhow::bail!("skill '{}' has invalid parameters_json", name);
        }

        // Check for duplicate skill name.
        if self.loaded.iter().any(|s| s.name == name) {
            anyhow::bail!("skill '{}' already loaded", name);
        }

        let manifest = build_manifest_audit(manifest_source, signed, &name, &version);
        if manifest.status != "ok" {
            tracing::warn!(
                skill = %name,
                status = %manifest.status,
                error = %manifest.error,
                "skill manifest is not verified"
            );
        }
        enforce_skill_policy(&manifest, &self.policy)?;

        let skill = LoadedSkill {
            name: name.clone(),
            description,
            version,
            parameters_json,
            path: path.to_path_buf(),
            manifest,
            fault_count: 0,
            execution_timeout: Duration::from_millis(self.policy.skill_execution_timeout_ms),
            vtable: vtable_ptr,
            lib: Arc::new(lib),
        };

        self.loaded.push(skill);
        Ok(name)
    }

    /// Get all loaded skills (immutable).
    pub fn loaded(&self) -> &[LoadedSkill] {
        &self.loaded
    }

    /// Active load policy.
    pub fn policy(&self) -> &SkillLoadPolicy {
        &self.policy
    }

    /// Get a mutable reference to a loaded skill by name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut LoadedSkill> {
        self.loaded.iter_mut().find(|s| s.name == name)
    }

    /// Unload a skill by name. Returns true if found and unloaded.
    pub fn unload(&mut self, name: &str) -> bool {
        if let Some(idx) = self.loaded.iter().position(|s| s.name == name) {
            let skill = self.loaded.remove(idx);
            tracing::info!(skill = %skill.name, "skill unloaded");
            // Library is dropped here, calling dlclose.
            true
        } else {
            false
        }
    }

    /// Remove skills that have faulted too many times.
    pub fn prune_faulted(&mut self) -> Vec<String> {
        let mut pruned = Vec::new();
        self.loaded.retain(|s| {
            if s.should_unload() {
                tracing::warn!(
                    skill = %s.name,
                    faults = s.fault_count,
                    "auto-unloading faulted skill"
                );
                pruned.push(s.name.clone());
                false
            } else {
                true
            }
        });
        pruned
    }

    /// Number of loaded skills.
    pub fn count(&self) -> usize {
        self.loaded.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

    fn workspace_root() -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().parent().unwrap().to_path_buf()
    }

    /// A skill `execute` that blocks well past any reasonable test deadline,
    /// standing in for a hung native skill.
    extern "C" fn slow_execute(_args: *const c_char) -> *mut c_char {
        std::thread::sleep(Duration::from_millis(200));
        CString::new(r#"{"success":true,"output":"late"}"#)
            .unwrap()
            .into_raw()
    }

    extern "C" fn noop_destroy(ptr: *mut c_char) {
        if !ptr.is_null() {
            unsafe { drop(CString::from_raw(ptr)) };
        }
    }

    /// Load the sample skill purely to obtain a valid `Library` to keep an
    /// invocation's `_lib` Arc populated in tests that drive synthetic
    /// function pointers.
    fn load_self_as_library() -> Library {
        unsafe { Library::new(sample_skill_path()) }.expect("load sample skill as library")
    }

    fn sample_skill_path() -> &'static Path {
        static SAMPLE_SKILL_PATH: OnceLock<PathBuf> = OnceLock::new();
        SAMPLE_SKILL_PATH.get_or_init(|| {
            let root = workspace_root();
            let build_dir = std::env::temp_dir().join(format!(
                "geniepod-sample-skill-build-loader-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&build_dir);
            std::fs::create_dir_all(&build_dir).unwrap();
            let output = Command::new("cargo")
                .args(["build", "-p", "genie-skill-hello", "--target-dir"])
                .arg(&build_dir)
                .current_dir(&root)
                .output()
                .expect("failed to build sample skill");

            assert!(
                output.status.success(),
                "sample skill build failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            let candidates = [
                build_dir.join("debug/libgenie_skill_hello.so"),
                build_dir.join("debug/libgenie_skill_hello.dylib"),
                build_dir.join("debug/genie_skill_hello.dll"),
            ];

            candidates
                .into_iter()
                .find(|path| path.exists())
                .expect("sample skill artifact not found")
        })
    }

    #[test]
    fn loader_empty_dir() {
        let dir = std::env::temp_dir().join("geniepod-skills-test-empty");
        let _ = std::fs::create_dir_all(&dir);
        let mut loader = SkillLoader::new(&dir);
        let names = loader.load_all();
        assert!(names.is_empty());
        assert_eq!(loader.count(), 0);
    }

    #[test]
    fn loader_nonexistent_dir() {
        let mut loader = SkillLoader::new(Path::new("/tmp/nonexistent-skills-dir"));
        let names = loader.load_all();
        assert!(names.is_empty());
    }

    #[test]
    fn loader_invalid_so() {
        let dir = std::env::temp_dir().join("geniepod-skills-test-invalid");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("bad.so"), b"not a real shared library").unwrap();
        let mut loader = SkillLoader::new(&dir);
        let names = loader.load_all();
        assert!(names.is_empty()); // Should fail gracefully
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loader_loads_and_executes_real_skill() {
        let skill_path = sample_skill_path();
        let dir = std::env::temp_dir().join("geniepod-skills-test-real");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installed_path = dir.join(skill_path.file_name().unwrap());
        std::fs::copy(skill_path, &installed_path).unwrap();

        let mut loader = SkillLoader::new(&dir);
        let name = loader.load_skill(&installed_path).unwrap();
        assert_eq!(name, "hello_world");
        assert_eq!(loader.count(), 1);

        let skill = loader.get_mut("hello_world").unwrap();
        assert_eq!(skill.manifest.status, "missing");
        let (success, output) = skill.execute_parsed(r#"{"name":"Jared"}"#).await;
        assert!(success);
        assert!(output.contains("Jared"));
        assert!(output.contains("loadable skill module"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A skill that runs longer than the configured deadline is reported as a
    /// fault rather than freezing the caller. We drive this through a real
    /// invocation whose C call sleeps past a deliberately tiny timeout.
    #[tokio::test]
    async fn execution_times_out_and_counts_as_fault() {
        let invocation = SkillInvocation {
            name: "slow".into(),
            execute_fn: slow_execute,
            destroy_fn: noop_destroy,
            args: CString::new("{}").unwrap(),
            timeout: Duration::from_millis(20),
            _lib: Arc::new(load_self_as_library()),
        };

        let outcome = invocation.run().await;
        assert!(!outcome.success);
        assert!(outcome.faulted);
        assert!(
            outcome.output.contains("execution timeout"),
            "unexpected output: {}",
            outcome.output
        );
    }

    /// The timeout deadline is sourced from the load policy and stamped onto
    /// each loaded skill, so an operator's `skill_execution_timeout_ms` actually
    /// reaches the invocation path.
    #[test]
    fn policy_timeout_is_applied_to_loaded_skill() {
        let skill_path = sample_skill_path();
        let dir = std::env::temp_dir().join(format!(
            "geniepod-skills-test-timeout-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installed_path = dir.join("hello.so");
        std::fs::copy(skill_path, &installed_path).unwrap();

        let mut loader = SkillLoader::new_with_policy(
            &dir,
            SkillLoadPolicy {
                skill_execution_timeout_ms: 1234,
                ..SkillLoadPolicy::default()
            },
        );
        loader.load_skill(&installed_path).unwrap();
        let skill = loader.loaded().first().unwrap();
        assert_eq!(skill.execution_timeout, Duration::from_millis(1234));
        assert_eq!(skill.prepare("{}").timeout, Duration::from_millis(1234));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loader_reads_skill_manifest_sidecar() {
        let skill_path = sample_skill_path();
        let dir = std::env::temp_dir().join(format!(
            "geniepod-skills-test-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installed_path = dir.join("hello.so");
        std::fs::copy(skill_path, &installed_path).unwrap();
        std::fs::write(
            dir.join("hello.skill.json"),
            r#"{
                "name": "hello_world",
                "version": "0.1.0",
                "description": "Sample hello skill",
                "permissions": ["speech.output"],
                "capabilities": ["demo.greeting"],
                "reviewed_by": "test",
                "signature": "test-signature"
            }"#,
        )
        .unwrap();

        let mut loader = SkillLoader::new(&dir);
        let name = loader.load_skill(&installed_path).unwrap();
        assert_eq!(name, "hello_world");

        let skill = loader.loaded().first().unwrap();
        assert_eq!(skill.manifest.status, "ok");
        assert_eq!(skill.manifest.permissions, vec!["speech.output"]);
        assert_eq!(skill.manifest.capabilities, vec!["demo.greeting"]);
        // An arbitrary "signature" string is NOT signed: `signed` is decided by
        // cryptographic verification, not by the field being non-empty.
        assert!(!skill.manifest.signed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loader_policy_can_require_manifest() {
        let skill_path = sample_skill_path();
        let dir = std::env::temp_dir().join(format!(
            "geniepod-skills-test-require-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installed_path = dir.join("hello.so");
        std::fs::copy(skill_path, &installed_path).unwrap();

        let mut loader = SkillLoader::new_with_policy(
            &dir,
            SkillLoadPolicy {
                require_manifest: true,
                ..SkillLoadPolicy::default()
            },
        );
        let err = loader.load_skill(&installed_path).unwrap_err();
        assert!(err.to_string().contains("manifest required"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loader_policy_blocks_denied_manifest_permissions() {
        let skill_path = sample_skill_path();
        let dir = std::env::temp_dir().join(format!(
            "geniepod-skills-test-denied-permission-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let installed_path = dir.join("hello.so");
        std::fs::copy(skill_path, &installed_path).unwrap();
        std::fs::write(
            dir.join("hello.skill.json"),
            r#"{
                "name": "hello_world",
                "version": "0.1.0",
                "permissions": ["network.raw"]
            }"#,
        )
        .unwrap();

        let mut loader = SkillLoader::new_with_policy(
            &dir,
            SkillLoadPolicy {
                denied_permissions: vec!["network.raw".into()],
                ..SkillLoadPolicy::default()
            },
        );
        let err = loader.load_skill(&installed_path).unwrap_err();
        assert!(err.to_string().contains("denied permission"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Signature enforcement (issue #175) -------------------------------

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use ed25519_dalek::{Signer, SigningKey};

    /// Skills dir + a sibling trusted-key dir with one installed key, plus the
    /// sample `.so` copied in. Returns the directories, the installed `.so`
    /// path, and the signing key whose public half is trusted under `key_id`.
    fn signed_skill_dirs(case: &str, key_id: &str) -> (PathBuf, PathBuf, PathBuf, SigningKey) {
        let dir =
            std::env::temp_dir().join(format!("geniepod-skills-sig-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let keys_dir = dir.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();

        let so_path = dir.join("hello.so");
        std::fs::copy(sample_skill_path(), &so_path).unwrap();

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        std::fs::write(
            keys_dir.join(format!("{key_id}.pub")),
            BASE64.encode(signing_key.verifying_key().to_bytes()),
        )
        .unwrap();

        (dir, keys_dir, so_path, signing_key)
    }

    /// Detached base64 Ed25519 signature over the current bytes of `so_path`.
    fn sign_file(key: &SigningKey, so_path: &Path) -> String {
        let bytes = std::fs::read(so_path).unwrap();
        BASE64.encode(key.sign(&bytes).to_bytes())
    }

    fn write_signed_manifest(dir: &Path, signature: &str, key_id: &str) {
        std::fs::write(
            dir.join("hello.skill.json"),
            format!(
                r#"{{
                    "name": "hello_world",
                    "version": "0.1.0",
                    "description": "Sample hello skill",
                    "reviewed_by": "test",
                    "signature": "{signature}",
                    "key_id": "{key_id}"
                }}"#
            ),
        )
        .unwrap();
    }

    fn require_signature_policy(keys_dir: &Path) -> SkillLoadPolicy {
        SkillLoadPolicy {
            require_signature: true,
            signature_key_dir: keys_dir.to_path_buf(),
            ..SkillLoadPolicy::default()
        }
    }

    /// The reported bug: `require_signature = true` + a junk `"signature"`
    /// string must NOT load the skill.
    #[test]
    fn loader_rejects_required_signature_with_junk_string() {
        let (dir, keys_dir, so_path, _key) = signed_skill_dirs("junk", "geniepod");
        write_signed_manifest(&dir, "x", "geniepod");

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let err = loader.load_skill(&so_path).unwrap_err();
        assert!(
            err.to_string().contains("signature required"),
            "unexpected error: {err}"
        );
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A valid detached signature by a trusted key over the exact `.so` loads
    /// and is reported as verified.
    #[test]
    fn loader_accepts_valid_signature() {
        let (dir, keys_dir, so_path, key) = signed_skill_dirs("valid", "geniepod");
        let signature = sign_file(&key, &so_path);
        write_signed_manifest(&dir, &signature, "geniepod");

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let name = loader.load_skill(&so_path).unwrap();
        assert_eq!(name, "hello_world");

        let skill = loader.loaded().first().unwrap();
        assert!(skill.manifest.signed);
        assert_eq!(skill.manifest.status, "ok");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Flipping one byte of the `.so` after signing invalidates the signature:
    /// tamper detection, load rejected.
    #[test]
    fn loader_rejects_tampered_so() {
        let (dir, keys_dir, so_path, key) = signed_skill_dirs("tamper", "geniepod");
        let signature = sign_file(&key, &so_path);
        write_signed_manifest(&dir, &signature, "geniepod");

        // Corrupt one byte after signing.
        let mut bytes = std::fs::read(&so_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&so_path, &bytes).unwrap();

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        assert!(loader.load_skill(&so_path).is_err());
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A correct signature that names a key id we do not trust fails closed.
    #[test]
    fn loader_rejects_unknown_key_id() {
        let (dir, keys_dir, so_path, key) = signed_skill_dirs("unknown", "geniepod");
        let signature = sign_file(&key, &so_path);
        // Signature is valid for the trusted key, but the manifest claims a
        // different (untrusted) key id.
        write_signed_manifest(&dir, &signature, "attacker");

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        assert!(loader.load_skill(&so_path).is_err());
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no trusted keys installed, require_signature fails closed even for
    /// a present manifest.
    #[test]
    fn loader_fails_closed_without_trusted_keys() {
        let (dir, _keys_dir, so_path, _key) = signed_skill_dirs("nokeys", "geniepod");
        write_signed_manifest(&dir, "x", "geniepod");

        let empty_keys = dir.join("empty-keys");
        std::fs::create_dir_all(&empty_keys).unwrap();
        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&empty_keys));
        assert!(loader.load_skill(&so_path).is_err());
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unsigned native skill — no sidecar manifest at all — must be rejected
    /// when a signature is required. The rejection is the signature gate, not a
    /// later manifest/ABI failure.
    #[test]
    fn loader_rejects_unsigned_skill_with_no_manifest() {
        let (dir, keys_dir, so_path, _key) = signed_skill_dirs("nomanifest", "geniepod");
        // Deliberately write no manifest sidecar.

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let err = loader.load_skill(&so_path).unwrap_err();
        assert!(
            err.to_string().contains("signature required"),
            "unexpected error: {err}"
        );
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A manifest present but carrying an empty `signature` field is unsigned:
    /// it must be rejected, never treated as signed by the field's existence.
    #[test]
    fn loader_rejects_empty_signature_field() {
        let (dir, keys_dir, so_path, _key) = signed_skill_dirs("emptysig", "geniepod");
        write_signed_manifest(&dir, "", "geniepod");

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let err = loader.load_skill(&so_path).unwrap_err();
        assert!(
            err.to_string().contains("signature required"),
            "unexpected error: {err}"
        );
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The signature gate runs *before* `dlopen`: an unsigned file whose bytes
    /// are not even a loadable library is rejected with the signature error,
    /// not a "dlopen failed" error. That proves the native code is never loaded
    /// when the signature check fails — the whole point of verifying first.
    #[test]
    fn loader_rejects_before_dlopen() {
        let dir = std::env::temp_dir().join(format!(
            "geniepod-skills-sig-gatefirst-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let keys_dir = dir.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();

        // Install a trusted key so the gate is genuinely active.
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        std::fs::write(
            keys_dir.join("geniepod.pub"),
            BASE64.encode(signing_key.verifying_key().to_bytes()),
        )
        .unwrap();

        // A bogus, unsigned ".so" that dlopen would reject outright.
        let so_path = dir.join("bogus.so");
        std::fs::write(&so_path, b"this is not a loadable shared library").unwrap();

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let err = loader.load_skill(&so_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("signature required"),
            "expected signature rejection before dlopen, got: {msg}"
        );
        assert!(
            !msg.contains("dlopen"),
            "dlopen must not be attempted before the signature gate, got: {msg}"
        );
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- TOCTOU mitigation (issue #280) -------------------------------------

    /// On Linux, `load_library_from_memfd` loads from the supplied byte buffer,
    /// not from the original path.  Passing valid `.so` bytes must produce a
    /// working library; the source file path is never consulted.
    #[test]
    #[cfg(target_os = "linux")]
    fn memfd_loads_library_from_bytes_not_path() {
        let so_bytes = std::fs::read(sample_skill_path()).unwrap();

        // load_library_from_memfd takes bytes — the source file is irrelevant.
        let lib = load_library_from_memfd(&so_bytes, "genie-skill-toctou-test")
            .expect("load_library_from_memfd failed");

        // Confirm the right code was loaded by finding the entry-point symbol.
        let init_fn: Symbol<unsafe extern "C" fn() -> *const SkillVTable> =
            unsafe { lib.get(b"genie_skill_init\0") }
                .expect("genie_skill_init symbol not found in memfd-loaded library");
        let vtable_ptr = unsafe { init_fn() };
        assert!(
            !vtable_ptr.is_null(),
            "genie_skill_init returned null from memfd-loaded library"
        );
    }

    /// End-to-end TOCTOU regression: a signed skill is loaded via the memfd
    /// path; replacing the .so on disk afterwards has no effect on the already-
    /// loaded library because the kernel loaded the sealed in-memory copy.
    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn loader_toctou_disk_swap_after_load_has_no_effect() {
        let (dir, keys_dir, so_path, key) = signed_skill_dirs("toctou", "geniepod");
        let signature = sign_file(&key, &so_path);
        write_signed_manifest(&dir, &signature, "geniepod");

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let name = loader.load_skill(&so_path).unwrap();
        assert_eq!(name, "hello_world");
        assert!(
            loader.loaded().first().unwrap().manifest.signed,
            "skill must be reported as cryptographically verified"
        );

        // Simulate what an attacker does after the TOCTOU window: replace the
        // .so with arbitrary bytes.  The sealed memfd is already loaded; this
        // disk write must have no effect on execution.
        std::fs::write(&so_path, b"attacker payload swapped after verification").unwrap();

        let skill = loader.get_mut("hello_world").unwrap();
        let (success, output) = skill.execute_parsed(r#"{"name":"TOCTOU"}"#).await;
        assert!(success, "skill execution failed after disk swap: {output}");
        assert!(
            output.contains("TOCTOU"),
            "expected legitimate skill output after disk swap, got: {output}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A malformed trusted key file must not produce a trusted posture: when the
    /// key id the manifest names fails to parse, it never becomes a trust
    /// anchor, so even a well-formed signature is rejected (fail closed). The
    /// skill is neither loaded nor reported as signed.
    #[test]
    fn loader_malformed_trusted_key_is_not_trusted() {
        let dir =
            std::env::temp_dir().join(format!("geniepod-skills-sig-badkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let keys_dir = dir.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();

        let so_path = dir.join("hello.so");
        std::fs::copy(sample_skill_path(), &so_path).unwrap();

        // The key the manifest references exists on disk but is garbage, so it
        // is skipped at load time and never trusted.
        std::fs::write(keys_dir.join("geniepod.pub"), "not a valid ed25519 key").unwrap();

        // The signature itself is well-formed (real key over the real bytes);
        // only the trust anchor is broken — which must still fail closed.
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let signature = BASE64.encode(
            signing_key
                .sign(&std::fs::read(&so_path).unwrap())
                .to_bytes(),
        );
        write_signed_manifest(&dir, &signature, "geniepod");

        let mut loader = SkillLoader::new_with_policy(&dir, require_signature_policy(&keys_dir));
        let err = loader.load_skill(&so_path).unwrap_err();
        assert!(
            err.to_string().contains("signature required"),
            "unexpected error: {err}"
        );
        assert_eq!(loader.count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
