use anyhow::Result;
use tokio::process::Command;

/// Control systemd and Docker services.
///
/// All operations are idempotent — starting an already-running service
/// or stopping an already-stopped service is a no-op (systemd handles this).
pub struct ServiceCtl;

impl ServiceCtl {
    /// Start a systemd unit.
    pub async fn start(unit: &str) -> Result<()> {
        tracing::info!(unit, "starting service");
        let output = Command::new("systemctl")
            .args(["start", unit])
            .output()
            .await?;

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
        let output = Command::new("systemctl")
            .args(["stop", unit])
            .output()
            .await?;

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
        Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Stop a Docker container by name.
    pub async fn docker_stop(container: &str) -> Result<()> {
        tracing::info!(container, "stopping Docker container");
        let output = Command::new("docker")
            .args(["stop", "-t", "10", container])
            .output()
            .await?;

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
        let output = Command::new("docker")
            .args(["start", container])
            .output()
            .await?;

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
        let output = Command::new("systemctl")
            .args(["daemon-reload"])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(unit = %unit, %stderr, "systemctl daemon-reload failed");
            anyhow::bail!("systemctl daemon-reload failed: {}", stderr);
        }

        let output = Command::new("systemctl")
            .args(["restart", &unit])
            .output()
            .await?;
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
        let output = Command::new("sh").args(["-c", script]).output().await?;
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
}
