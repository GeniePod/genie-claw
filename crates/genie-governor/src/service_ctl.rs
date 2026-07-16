use anyhow::Result;
use std::time::Duration;
use tokio::process::Command;

/// Bound every systemctl/docker invocation in this file so a hung d-bus call
/// or wedged docker daemon can't block the governor's supervisor loop forever
/// (issue #617). systemctl/docker are normally near-instant; 10s is generous
/// headroom while still catching a genuine hang.
const SERVICE_CTL_TIMEOUT: Duration = Duration::from_secs(10);

/// Control systemd and Docker services.
///
/// All operations are idempotent — starting an already-running service
/// or stopping an already-stopped service is a no-op (systemd handles this).
pub struct ServiceCtl;

impl ServiceCtl {
    /// Start a systemd unit.
    pub async fn start(unit: &str) -> Result<()> {
        tracing::info!(unit, "starting service");
        let mut cmd = Command::new("systemctl");
        cmd.args(["start", unit]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                anyhow::bail!("systemctl start {unit} timed out after {SERVICE_CTL_TIMEOUT:?}")
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(unit, %stderr, "failed to start service");
            anyhow::bail!("systemctl start {} failed: {}", unit, stderr);
        }
        Ok(())
    }

    /// Stop a systemd unit.
    pub async fn stop(unit: &str) -> Result<()> {
        tracing::info!(unit, "stopping service");
        let mut cmd = Command::new("systemctl");
        cmd.args(["stop", unit]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                anyhow::bail!("systemctl stop {unit} timed out after {SERVICE_CTL_TIMEOUT:?}")
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't fail on "not loaded" — service may not be enabled.
            if stderr.contains("not loaded") {
                return Ok(());
            }
            tracing::warn!(unit, %stderr, "failed to stop service");
            anyhow::bail!("systemctl stop {} failed: {}", unit, stderr);
        }
        Ok(())
    }

    /// Check if a systemd unit is active.
    pub async fn is_active(unit: &str) -> bool {
        let mut cmd = Command::new("systemctl");
        cmd.args(["is-active", "--quiet", unit]).kill_on_drop(true);
        match tokio::time::timeout(SERVICE_CTL_TIMEOUT, cmd.status()).await {
            Ok(Ok(status)) => status.success(),
            _ => false,
        }
    }

    /// Stop a Docker container by name.
    pub async fn docker_stop(container: &str) -> Result<()> {
        tracing::info!(container, "stopping Docker container");
        let mut cmd = Command::new("docker");
        cmd.args(["stop", "-t", "10", container]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::warn!(
                    container,
                    timeout = ?SERVICE_CTL_TIMEOUT,
                    "docker stop timed out"
                );
                return Ok(());
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("No such container") {
                tracing::warn!(container, %stderr, "failed to stop container");
            }
        }
        Ok(())
    }

    /// Start a Docker container by name (must already exist / be created).
    #[allow(dead_code)]
    pub async fn docker_start(container: &str) -> Result<()> {
        tracing::info!(container, "starting Docker container");
        let mut cmd = Command::new("docker");
        cmd.args(["start", container]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                anyhow::bail!("docker start {container} timed out after {SERVICE_CTL_TIMEOUT:?}")
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(container, %stderr, "failed to start container");
            anyhow::bail!("docker start {} failed: {}", container, stderr);
        }
        Ok(())
    }

    /// Reload the configured LLM service with a different model.
    /// Sends SIGHUP or restarts the service with new args.
    pub async fn swap_llm_model(unit: &str, model_path: &str) -> Result<()> {
        let unit = normalize_systemd_unit(unit);
        tracing::info!(unit = %unit, model = model_path, "swapping LLM model");

        // Write the model path to the override config, then restart.
        let override_dir = llm_override_dir_for_unit(&unit);
        tokio::fs::create_dir_all(&override_dir).await?;

        let override_content = format!(
            "[Service]\nEnvironment=\"GENIEPOD_LLM_MODEL={}\"\n",
            model_path
        );
        tokio::fs::write(
            format!("{}/model-override.conf", override_dir),
            override_content,
        )
        .await?;

        // Reload systemd and restart the LLM service. Both commands can fail
        // silently (polkit denial, masked unit, rejected override) — a swallowed
        // failure here leaves the heavier model resident and risks OOM, so we
        // check the exit status and bail like the other state-changing methods.
        let mut reload_cmd = Command::new("systemctl");
        reload_cmd.args(["daemon-reload"]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, reload_cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                anyhow::bail!("systemctl daemon-reload timed out after {SERVICE_CTL_TIMEOUT:?}")
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(unit = %unit, %stderr, "systemctl daemon-reload failed");
            anyhow::bail!("systemctl daemon-reload failed: {}", stderr);
        }

        let mut restart_cmd = Command::new("systemctl");
        restart_cmd.args(["restart", &unit]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, restart_cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                anyhow::bail!("systemctl restart {unit} timed out after {SERVICE_CTL_TIMEOUT:?}")
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(unit = %unit, %stderr, "failed to restart LLM service");
            anyhow::bail!("systemctl restart {} failed: {}", unit, stderr);
        }

        Ok(())
    }

    /// Enable zram swap (2 GB compressed).
    pub async fn enable_zram() -> Result<()> {
        tracing::warn!("enabling zram swap — memory critically low");
        let script = r#"
            if [ ! -e /dev/zram0 ]; then
                modprobe zram num_devices=1
            fi
            echo lz4 > /sys/block/zram0/comp_algorithm
            echo 2G > /sys/block/zram0/disksize
            mkswap /dev/zram0 2>/dev/null
            swapon -p 5 /dev/zram0 2>/dev/null
        "#;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]).kill_on_drop(true);
        let output = match tokio::time::timeout(SERVICE_CTL_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::warn!(
                    timeout = ?SERVICE_CTL_TIMEOUT,
                    "zram setup timed out — memory pressure response degraded"
                );
                return Ok(());
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(%stderr, "zram setup may have partially failed");
        }
        Ok(())
    }
}

fn normalize_systemd_unit(unit: &str) -> String {
    if unit.contains('.') {
        unit.to_string()
    } else {
        format!("{unit}.service")
    }
}

fn llm_override_dir_for_unit(unit: &str) -> String {
    format!("/etc/systemd/system/{}.d", normalize_systemd_unit(unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_systemd_unit_names() {
        assert_eq!(normalize_systemd_unit("genie-llm"), "genie-llm.service");
        assert_eq!(
            normalize_systemd_unit("genie-ai-runtime.service"),
            "genie-ai-runtime.service"
        );
    }

    #[test]
    fn llm_override_dir_uses_configured_unit() {
        assert_eq!(
            llm_override_dir_for_unit("genie-ai-runtime.service"),
            "/etc/systemd/system/genie-ai-runtime.service.d"
        );
    }

    /// Regression for #617: every `ServiceCtl` call site wraps its command in
    /// `tokio::time::timeout` + `kill_on_drop(true)`. `systemctl`/`docker`
    /// aren't guaranteed to be present or functional in a test sandbox, so
    /// this exercises the exact same wrap-and-kill idiom directly against
    /// `sleep` (always present on the Linux targets this project supports)
    /// to prove a hung child is bounded and does not block the caller.
    #[tokio::test]
    async fn timeout_wrap_bounds_a_hung_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").kill_on_drop(true);

        let start = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_millis(200), cmd.output()).await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "200ms timeout must fire before `sleep 30` exits"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must fail fast on the timeout, not wait for the child: took {elapsed:?}"
        );
    }
}
