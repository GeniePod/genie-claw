// `genie-core --version` must work on a fresh install, before any config file
// exists — it is handled before Config::load(). Regression test for the
// "failed to read config /etc/geniepod/geniepod.toml" error on fresh installs.

use std::process::Command;

#[test]
fn version_flag_succeeds_without_config() {
    let exe = env!("CARGO_BIN_EXE_genie-core");
    let out = Command::new(exe)
        .arg("--version")
        .env_remove("GENIEPOD_CONFIG")
        .output()
        .expect("run genie-core --version");

    assert!(
        out.status.success(),
        "genie-core --version must exit 0 without a config; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("genie-core v"),
        "expected version line, got: {stdout:?}"
    );
}
