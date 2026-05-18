// Integration tests for genie-core.
// Verify tool dispatch, config loading, and binary properties
// without requiring an LLM, HA, or Jetson hardware.

use std::process::Command;

/// Verify genie-core builds successfully.
#[test]
fn core_binary_builds() {
    let output = build_release_genie_core();

    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify release binary is under 5 MB.
#[test]
fn binary_size_budget() {
    let output = build_release_genie_core();
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let path = workspace_root().join("target/release/genie-core");
    if path.exists() {
        let size = std::fs::metadata(&path).unwrap().len();
        let size_mb = size as f64 / 1_048_576.0;
        println!("genie-core: {:.2} MB", size_mb);
        assert!(size_mb < 5.0, "{:.1} MB exceeds 5 MB budget", size_mb);
    }
}

/// Verify deploy config is valid TOML with expected sections.
#[test]
fn config_parses() {
    let config_path = workspace_root().join("deploy/config/geniepod.toml");
    let contents = std::fs::read_to_string(&config_path).unwrap();
    let config: toml::Value = toml::from_str(&contents).unwrap();

    // Verify expected sections exist.
    let table = config.as_table().unwrap();
    assert!(table.contains_key("core"), "missing [core] section");
    assert!(table.contains_key("governor"), "missing [governor] section");
    assert!(table.contains_key("health"), "missing [health] section");
    assert!(table.contains_key("services"), "missing [services] section");
}

/// Verify all systemd unit files reference correct binary names.
#[test]
fn systemd_units_valid() {
    let systemd_dir = workspace_root().join("deploy/systemd");
    let entries = std::fs::read_dir(&systemd_dir).unwrap();

    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "service") {
            let contents = std::fs::read_to_string(&path).unwrap();
            // No unit should reference "dawn".
            assert!(
                !contents.contains("dawn"),
                "{:?} still references 'dawn'",
                path.file_name().unwrap()
            );
        }
    }
}

/// Verify the aggregate target does not hard-fail when optional audio init is absent.
#[test]
fn geniepod_target_audio_is_optional() {
    let path = workspace_root().join("deploy/systemd/geniepod.target");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("Wants=genie-audio.service"),
        "geniepod.target should softly pull in audio"
    );
    assert!(
        !contents.contains("Requires=genie-audio.service"),
        "geniepod.target should not hard-require audio"
    );
}

/// Verify audio init is skipped cleanly if the helper binary is not deployed.
#[test]
fn genie_audio_service_checks_for_helper() {
    let path = workspace_root().join("deploy/systemd/genie-audio.service");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("ConditionPathExists=/opt/geniepod/bin/genie-audio-init"),
        "genie-audio.service should check for its helper binary"
    );
}

/// Verify Jetson setup warns when the optional audio helper is missing.
#[test]
fn setup_script_warns_about_missing_audio_helper() {
    let path = workspace_root().join("deploy/setup-jetson.sh");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("WARN: genie-audio-init missing"),
        "setup script should detect missing audio init"
    );
    assert!(
        contents.contains("genie-audio.service will be skipped"),
        "setup script should explain the runtime impact"
    );
}

/// Verify LLM backend auto-fallback can patch a root-owned config and fails loudly.
#[test]
fn setup_script_privileged_llm_backend_patch_is_checked() {
    let path = workspace_root().join("deploy/setup-jetson.sh");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("CONFIGURED_BACKEND=\"$(sudo awk"),
        "setup script should read the configured LLM backend through sudo"
    );
    assert!(
        contents.contains("if ! sudo awk -v nb=\"$new_backend\" -v nu=\"$new_unit\""),
        "setup script should read the chmod 600 root-owned config through sudo"
    );
    assert!(
        contents.contains("sudo mktemp /tmp/geniepod.toml."),
        "setup script should create a root-owned temp file for the patched config"
    );
    assert!(
        contents.contains("ERROR: failed to rewrite $cfg for patching"),
        "setup script should report failed config rewrites"
    );
    assert!(
        contents.contains("| sudo tee \"$tmp\" > /dev/null"),
        "setup script should write the patched temp file through sudo tee"
    );
    assert!(
        contents.contains("ERROR: failed to install patched $cfg"),
        "setup script should report failed config installs"
    );
    assert!(
        contents.contains("sudo rm -f \"$tmp\""),
        "setup script should clean up the root-owned temp file through sudo"
    );
    assert!(
        contents.contains("Installing genie-ai-runtime now; this is the default backend"),
        "setup script should install the default runtime during normal setup"
    );
    assert!(
        contents.contains("find_cuda_compiler()"),
        "setup script should resolve nvcc before building the default runtime"
    );
    assert!(
        contents.contains("-DCMAKE_CUDA_COMPILER=\"$cuda_compiler\""),
        "setup script should pass the resolved nvcc path to CMake"
    );
    assert!(
        contents.contains("-DJLLM_BUILD_SERVER=ON"),
        "setup script should build the jetson-llm-server binary required by systemd"
    );
    assert!(
        contents.contains("ERROR: CUDA compiler nvcc not found."),
        "setup script should fail clearly when nvcc is missing"
    );
    assert!(
        !contents.contains("Auto-falling back to llama.cpp"),
        "setup script should not silently downgrade the default backend to llama.cpp"
    );
    assert!(
        contents.contains(
            "if ! patch_services_llm_backend \"genie_ai_runtime\" \"genie-ai-runtime.service\""
        ),
        "genie-ai-runtime selection should check patch failure"
    );
    assert!(
        contents
            .contains("auto-fallback could not patch $CONFIG_DIR/geniepod.toml; aborting setup"),
        "setup should abort instead of enabling services against an unpatched config"
    );
}

/// Verify the Jetson restart helper script is syntactically valid.
#[test]
fn jetson_restart_script_is_valid_shell() {
    let path = workspace_root().join("deploy/scripts/genie-restart-all.sh");
    assert!(path.exists(), "restart helper script should exist");

    let output = std::process::Command::new("bash")
        .args(["-n", path.to_str().unwrap()])
        .output()
        .expect("failed to run bash -n");

    assert!(
        output.status.success(),
        "restart helper script has invalid shell syntax: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify the deploy pipeline copies the Jetson restart helper script.
#[test]
fn makefile_deploys_restart_helper() {
    let path = workspace_root().join("Makefile");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("deploy/scripts/genie-restart-all.sh"),
        "Makefile should copy the restart helper script during deploy"
    );
    assert!(
        contents.contains("$(INSTALL_DIR)/bin/genie-restart-all.sh"),
        "Makefile should install the restart helper into /opt/geniepod/bin"
    );
}

/// Verify systemd deploy replaces stale or masked unit-file symlinks.
#[test]
fn makefile_installs_systemd_units_instead_of_copying_through_symlinks() {
    let path = workspace_root().join("Makefile");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("sudo install -m 0644 \"$$unit\""),
        "Makefile should replace stale/masked unit files instead of copying through symlinks"
    );
    assert!(
        !contents.contains("sudo cp /tmp/genie-*.service"),
        "Makefile should not use cp for systemd units; cp follows masked-unit symlinks"
    );
}

/// Verify the restart helper does not bounce llama.cpp on routine app updates.
#[test]
fn restart_helper_skips_llm_service() {
    let path = workspace_root().join("deploy/scripts/genie-restart-all.sh");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        !contents.contains("genie-llm.service"),
        "restart helper should not restart genie-llm.service"
    );
}

fn workspace_root() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

fn build_release_genie_core() -> std::process::Output {
    Command::new("cargo")
        .args(["build", "--release", "-p", "genie-core"])
        .current_dir(workspace_root())
        .output()
        .expect("failed to run cargo build")
}
