// Integration tests for genie-core.
// Verify tool dispatch, config loading, and binary properties
// without requiring an LLM, HA, or Jetson hardware.

use std::process::Command;

/// Verify genie-core builds successfully.
#[test]
fn core_binary_builds() {
    let output = Command::new("cargo")
        .args(["build", "--release", "-p", "genie-core"])
        .current_dir(workspace_root())
        .output()
        .expect("failed to run cargo build");

    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify release binary is under 3 MB.
#[test]
fn binary_size_budget() {
    let path = workspace_root().join("target/release/genie-core");
    if path.exists() {
        let size = std::fs::metadata(&path).unwrap().len();
        let size_mb = size as f64 / 1_048_576.0;
        println!("genie-core: {:.2} MB", size_mb);
        assert!(size_mb < 3.0, "{:.1} MB exceeds 3 MB budget", size_mb);
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
