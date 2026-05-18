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

/// genie-core release-binary size ceiling. Raised from the alpha-era
/// 5.0 MB budget after legitimate growth from the runtime backend,
/// voice, runtime mode, and concurrent server work. Keep it tight enough
/// that another large dependency or module forces a deliberate decision.
const RELEASE_BINARY_SIZE_BUDGET_MB: f64 = 6.0;

/// Verify release binary stays within the release size budget.
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
        assert!(
            size_mb < RELEASE_BINARY_SIZE_BUDGET_MB,
            "{:.1} MB exceeds {:.1} MB budget",
            size_mb,
            RELEASE_BINARY_SIZE_BUDGET_MB
        );
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
        contents.contains("Downloading prebuilt runtime assets"),
        "setup script should download the default runtime from release assets"
    );
    assert!(
        contents.contains("SHA256SUMS"),
        "setup script should download release checksums"
    );
    assert!(
        contents.contains("sha256sum -c"),
        "setup script should verify downloaded runtime checksums"
    );
    assert!(
        contents.contains("jetson-llm-server-v1.0.0-aarch64-unknown-linux-gnu"),
        "setup script should document the required server release asset"
    );
    assert!(
        !contents.contains("git clone --branch \"$tag\""),
        "setup script should not clone the runtime repo during normal install"
    );
    assert!(
        !contents.contains("cmake --build build"),
        "setup script should not build the runtime from source during setup"
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

/// Verify the Jetson lifecycle helper scripts are syntactically valid.
#[test]
fn jetson_lifecycle_scripts_are_valid_shell() {
    for script in [
        "deploy/scripts/genie-restart-all.sh",
        "deploy/scripts/start_all.sh",
        "deploy/scripts/stop_all.sh",
        "deploy/scripts/genie-model-cache-status.sh",
    ] {
        let path = workspace_root().join(script);
        assert!(path.exists(), "{script} should exist");

        let output = std::process::Command::new("bash")
            .args(["-n", path.to_str().unwrap()])
            .output()
            .expect("failed to run bash -n");

        assert!(
            output.status.success(),
            "{script} has invalid shell syntax: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Verify the deploy pipeline copies the Jetson lifecycle helper scripts.
#[test]
fn makefile_deploys_lifecycle_helpers() {
    let path = workspace_root().join("Makefile");
    let contents = std::fs::read_to_string(&path).unwrap();

    for script in [
        "genie-restart-all.sh",
        "start_all.sh",
        "stop_all.sh",
        "genie-model-cache-status.sh",
    ] {
        assert!(
            contents.contains(&format!("deploy/scripts/{script}")),
            "Makefile should copy {script} during deploy"
        );
        assert!(
            contents.contains(&format!("$(INSTALL_DIR)/bin/{script}")),
            "Makefile should install {script} into /opt/geniepod/bin"
        );
    }
}

/// Verify start_all follows the configured backend instead of starting both LLMs.
#[test]
fn start_all_uses_configured_llm_backend() {
    let path = workspace_root().join("deploy/scripts/start_all.sh");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("Configured LLM unit"),
        "start_all should report the selected LLM unit"
    );
    assert!(
        contents.contains("read_llm_unit"),
        "start_all should read [services.llm].systemd_unit"
    );
    assert!(
        contents.contains("read_wakeword_script")
            && contents.contains("wakeword_script is empty; push-to-talk mode"),
        "start_all should honor empty [core].wakeword_script as push-to-talk mode"
    );
    assert!(
        contents.contains("reset-failed \"$unit\""),
        "start_all should clear stale failed state for disabled optional services"
    );
    assert!(
        contents.contains("other_llm_units_for"),
        "start_all should stop the non-selected LLM backend before starting"
    );
    assert!(
        contents.contains("is_warmup_unit") && contents.contains("start --no-block"),
        "start_all should queue warmup units without blocking the lifecycle script"
    );
    let units = contents
        .split("UNITS=(")
        .nth(1)
        .and_then(|s| s.split(")").next())
        .expect("start_all should declare ordered units");
    let llm_pos = units
        .find("\"$configured_llm_unit\"")
        .expect("start_all should include the configured LLM unit");
    let homeassistant_pos = units
        .find("homeassistant.service")
        .expect("start_all should include Home Assistant");
    let whisper_pos = units
        .find("genie-whisper.service")
        .expect("start_all should include Whisper");
    assert!(
        llm_pos < homeassistant_pos && llm_pos < whisper_pos,
        "start_all should start the configured LLM before memory-heavy services"
    );
}

/// Pin the hard-reset shape of genie-restart-all.sh so future edits can't
/// quietly drop the orphan reap / cache drop / swap free steps.
#[test]
fn genie_restart_all_hard_mode_performs_full_memory_reset() {
    let path = workspace_root().join("deploy/scripts/genie-restart-all.sh");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("stop_all.sh"),
        "genie-restart-all should delegate to stop_all.sh before reset"
    );
    assert!(
        contents.contains("pkill -x"),
        "genie-restart-all should reap orphaned subprocesses by exact basename"
    );
    assert!(
        contents.contains("drop_caches"),
        "genie-restart-all --hard should drop page cache"
    );
    assert!(
        contents.contains("swapoff -a") && contents.contains("swapon -a"),
        "genie-restart-all --hard should free + re-enable swap"
    );
    assert!(
        contents.contains("start_all.sh"),
        "genie-restart-all should delegate to start_all.sh after reset"
    );
    assert!(
        contents.contains("--soft"),
        "genie-restart-all should expose --soft to skip the cache+swap reset (preserves PR #70 warm cache)"
    );
    assert!(
        contents.contains("PR #70") || contents.contains("issue #69"),
        "genie-restart-all should document the warm-cache trade-off relative to PR #70 / issue #69"
    );

    // Ordering invariant against the imperative code section (matched via
    // strings that only appear in the code body, not the doc-comment header):
    //   stop_all → reap orphans → drop page cache → free swap → start_all.
    let stop_pos = contents.find("\"$STOP_ALL\"").expect("STOP_ALL invocation");
    let reap_pos = contents
        .find("Reaping orphaned subprocesses")
        .expect("reap echo line");
    let drop_pos = contents
        .find("Dropping page cache")
        .expect("drop_caches echo line");
    let swap_pos = contents.find("Freeing swap").expect("swap echo line");
    let start_pos = contents
        .find("\"$START_ALL\"")
        .expect("START_ALL invocation");
    assert!(
        stop_pos < reap_pos,
        "stop_all must run before reaping orphans"
    );
    assert!(
        reap_pos < drop_pos,
        "orphan reap must run before drop_caches"
    );
    assert!(
        drop_pos < swap_pos,
        "drop_caches must run before swap reset"
    );
    assert!(swap_pos < start_pos, "swap reset must run before start_all");
}

/// Verify genie-ai-runtime service preserves warm GGUF pages across restarts.
#[test]
fn genie_ai_runtime_service_preserves_model_page_cache() {
    let path = workspace_root().join("deploy/systemd/genie-ai-runtime.service");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        !contents.contains("ExecStartPre="),
        "genie-ai-runtime.service should not force cold model reloads"
    );
    assert!(
        contents.contains("page cache") && contents.contains("issue #69"),
        "service should document why page cache is preserved"
    );
    assert!(
        contents.contains("--int8-kv"),
        "genie-ai-runtime.service should use INT8 KV to fit enough context under memory pressure"
    );
    assert!(
        contents.contains("GENIEPOD_AI_RUNTIME_CONTEXT=8192"),
        "genie-ai-runtime.service should request the Jetson-tested 8k context size"
    );
    assert!(
        contents.contains(
            "Before=genie-whisper.service genie-whisper-warmup.service homeassistant.service genie-core.service"
        ),
        "genie-ai-runtime.service should reserve KV cache before memory-heavy services"
    );
}

/// Verify the chat UI keeps an animated in-flight state before streamed tokens arrive.
#[test]
fn chat_ui_uses_animated_writing_indicator() {
    let path = workspace_root().join("crates/genie-core/src/chat_ui.html");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("Agent writing"),
        "chat UI should show the in-flight agent state"
    );
    assert!(
        contents.contains("writing-dots") && contents.contains("@keyframes writing-pulse"),
        "chat UI should animate the in-flight state instead of rendering static dots"
    );
    assert!(
        contents.contains("aria-busy") && contents.contains("aria-live"),
        "chat UI should expose the in-flight state to assistive technology"
    );
    assert!(
        !contents.contains(".msg.bot.streaming:empty::before"),
        "chat UI should not rely on an empty pseudo-element placeholder"
    );
}

/// Verify the chat UI header reflects `/api/health` status and LLM state.
#[test]
fn chat_ui_reflects_api_health_status() {
    let path = workspace_root().join("crates/genie-core/src/chat_ui.html");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("function applyHealthStatus"),
        "chat UI should map health JSON to header state"
    );
    assert!(
        contents.contains("data.status === 'ok'") && contents.contains("data.llm === 'connected'"),
        "chat UI should require both ok status and connected LLM before showing Connected"
    );
    assert!(
        contents.contains("LLM offline") && contents.contains("dot degraded"),
        "chat UI should show a degraded state when the LLM is offline"
    );
}

/// Verify the model cache helper can inspect GGUF page-cache residency.
#[test]
fn model_cache_status_helper_reports_residency() {
    let path = workspace_root().join("deploy/scripts/genie-model-cache-status.sh");
    let contents = std::fs::read_to_string(&path).unwrap();

    assert!(
        contents.contains("llm_model_path"),
        "helper should default to the configured LLM model path"
    );
    assert!(
        contents.contains("mincore"),
        "helper should use Linux mincore to inspect page residency"
    );
    assert!(
        contents.contains("Resident:"),
        "helper should print resident model bytes"
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
