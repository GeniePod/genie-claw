//! Dynamic skill loader — dlopen wrapper for `.so` skill modules.
//!
//! Like Linux's `insmod`, loads a shared library, finds the
//! `genie_skill_init` symbol, and extracts the SkillVTable.

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};

use anyhow::Result;
use genie_skill_sdk::{ABI_VERSION, SkillVTable};
use libloading::{Library, Symbol};
use serde::{Deserialize, Serialize};

/// Optional sidecar metadata for a native skill.
///
/// A skill named `hello.so` can declare metadata in `hello.skill.json`.
/// The loader treats this as audit metadata today, not as a signature check.
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
    /// Signature material or signature reference. Presence only is reported.
    pub signature: String,
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
    pub signed: bool,
    pub error: String,
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
            signed: false,
            error,
        }
    }

    fn from_manifest(
        path: PathBuf,
        manifest: SkillManifest,
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
        let signed = !manifest.signature.trim().is_empty();

        Self {
            status: status.into(),
            path: Some(path),
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            permissions: manifest.permissions,
            capabilities: manifest.capabilities,
            reviewed_by: manifest.reviewed_by,
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
    /// The vtable pointer (valid for lifetime of `_lib`).
    vtable: *const SkillVTable,
    /// Library handle — must stay alive as long as vtable is used.
    _lib: Library,
}

// Safety: LoadedSkill is only accessed from the single-threaded tokio runtime.
// The Library and vtable pointer are valid for the lifetime of the LoadedSkill.
unsafe impl Send for LoadedSkill {}
unsafe impl Sync for LoadedSkill {}

impl LoadedSkill {
    /// Execute the skill with JSON arguments.
    ///
    /// Wraps the C ABI call and handles string lifecycle.
    /// Returns the result as a Rust String.
    pub fn execute(&mut self, args_json: &str) -> Result<String> {
        let vtable = unsafe { &*self.vtable };

        let c_args = CString::new(args_json).unwrap_or_default();
        let result_ptr = (vtable.execute)(c_args.as_ptr());

        if result_ptr.is_null() {
            self.fault_count += 1;
            anyhow::bail!("skill '{}' returned null", self.name);
        }

        let result_str = unsafe { CStr::from_ptr(result_ptr) }
            .to_string_lossy()
            .to_string();

        // Free the C string via the skill's destroy function.
        (vtable.destroy)(result_ptr);

        Ok(result_str)
    }

    /// Execute and parse the JSON result into success/output.
    pub fn execute_parsed(&mut self, args_json: &str) -> (bool, String) {
        match self.execute(args_json) {
            Ok(json) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) {
                    let success = parsed
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let output = parsed
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&json)
                        .to_string();
                    if !success {
                        self.fault_count += 1;
                    }
                    (success, output)
                } else {
                    (true, json)
                }
            }
            Err(e) => {
                self.fault_count += 1;
                (false, e.to_string())
            }
        }
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

fn read_manifest_audit(
    skill_path: &Path,
    loaded_name: &str,
    loaded_version: &str,
) -> SkillManifestAudit {
    let Some(path) = find_manifest_sidecar(skill_path) else {
        return SkillManifestAudit::missing();
    };

    match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|content| {
            serde_json::from_str::<SkillManifest>(&content).map_err(|e| e.to_string())
        }) {
        Ok(manifest) => {
            SkillManifestAudit::from_manifest(path, manifest, loaded_name, loaded_version)
        }
        Err(error) => SkillManifestAudit::invalid(path, error),
    }
}

/// Skill loader — scans a directory for `.so` files and loads them.
pub struct SkillLoader {
    skills_dir: PathBuf,
    loaded: Vec<LoadedSkill>,
}

impl SkillLoader {
    pub fn new(skills_dir: &Path) -> Self {
        Self {
            skills_dir: skills_dir.to_path_buf(),
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
        // Safety: loading a .so is inherently unsafe. We trust skills from
        // the skills directory (like Linux trusts kernel modules from /lib/modules).
        let lib = unsafe { Library::new(path) }
            .map_err(|e| anyhow::anyhow!("dlopen failed for {}: {}", path.display(), e))?;

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

        let manifest = read_manifest_audit(path, &name, &version);
        if manifest.status != "ok" {
            tracing::warn!(
                skill = %name,
                status = %manifest.status,
                error = %manifest.error,
                "skill manifest is not verified"
            );
        }

        let skill = LoadedSkill {
            name: name.clone(),
            description,
            version,
            parameters_json,
            path: path.to_path_buf(),
            manifest,
            fault_count: 0,
            vtable: vtable_ptr,
            _lib: lib,
        };

        self.loaded.push(skill);
        Ok(name)
    }

    /// Get all loaded skills (immutable).
    pub fn loaded(&self) -> &[LoadedSkill] {
        &self.loaded
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

    #[test]
    fn loader_loads_and_executes_real_skill() {
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
        let (success, output) = skill.execute_parsed(r#"{"name":"Jared"}"#);
        assert!(success);
        assert!(output.contains("Jared"));
        assert!(output.contains("loadable skill module"));

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
        assert!(skill.manifest.signed);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
