use anyhow::Result;
use genie_common::tegrastats;

use crate::ha::HomeAutomationProvider;

/// Get system status: memory, uptime, governor mode.
pub async fn system_info(ha: Option<&dyn HomeAutomationProvider>) -> Result<String> {
    let mut info = Vec::new();
    let governor_status = query_governor_status().await;

    info.push(home_assistant_status(ha).await);

    // Prefer the governor's latest reading when available.
    if let Some(avail) = governor_status
        .as_ref()
        .and_then(governor_mem_available_mb)
        .or_else(|| tegrastats::mem_available_mb().ok())
    {
        info.push(format!("Memory available: {} MB", avail));
    }

    // Uptime.
    if let Ok(contents) = tokio::fs::read_to_string("/proc/uptime").await
        && let Some(secs_str) = contents.split_whitespace().next()
        && let Ok(secs) = secs_str.parse::<f64>()
    {
        info.push(format!("Uptime: {}", format_uptime_secs(secs as u64)));
    }

    // Governor mode (try control socket).
    if let Some(status) = governor_status {
        if let Some(mode) = status.get("mode").and_then(|v| v.as_str()) {
            info.push(format!("Governor mode: {}", mode));
        }
    } else {
        info.push("Governor: not running".to_string());
    }

    // Load average.
    if let Ok(contents) = tokio::fs::read_to_string("/proc/loadavg").await {
        if let Some(load_avg) = format_load_average(&contents) {
            info.push(format!("Load average: {}", load_avg));
        }
    }

    if info.is_empty() {
        Ok("System info unavailable.".into())
    } else {
        Ok(info.join(". ") + ".")
    }
}

fn governor_mem_available_mb(status: &serde_json::Value) -> Option<u64> {
    status
        .get("mem_available_mb_live")
        .and_then(|v| v.as_u64())
        .or_else(|| status.get("mem_available_mb").and_then(|v| v.as_u64()))
}

fn format_uptime_secs(secs: u64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    format!("{}h {}m", hours, mins)
}

fn format_load_average(contents: &str) -> Option<String> {
    let parts: Vec<&str> = contents.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    Some(format!("{} {} {}", parts[0], parts[1], parts[2]))
}

async fn home_assistant_status(ha: Option<&dyn HomeAutomationProvider>) -> String {
    let Some(ha) = ha else {
        return "Home Assistant: integration disabled".to_string();
    };

    let health = ha.health().await;
    if health.connected {
        let message = health.message.trim();
        if message.is_empty()
            || message.eq_ignore_ascii_case("ok")
            || message.eq_ignore_ascii_case("healthy")
        {
            "Home Assistant: connected".to_string()
        } else if let Some(rest) = message.strip_prefix("connected to Home Assistant at ") {
            format!("Home Assistant: connected ({})", rest)
        } else {
            format!("Home Assistant: connected ({})", message)
        }
    } else {
        let message = health.message.trim();
        if message.is_empty() {
            "Home Assistant: unavailable".to_string()
        } else if let Some(rest) = message.strip_prefix("Home Assistant unavailable: ") {
            format!("Home Assistant: unavailable ({})", rest)
        } else {
            format!("Home Assistant: unavailable ({})", message)
        }
    }
}

async fn query_governor_status() -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect("/run/geniepod/governor.sock")
        .await
        .ok()?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(b"{\"cmd\":\"status\"}\n").await.ok()?;

    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .ok()?
        .ok()?;

    line.and_then(|l| serde_json::from_str(&l).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::{
        ActionResult, DeviceRef, HomeAction, HomeAutomationProvider, HomeGraph, HomeState,
        HomeTarget, IntegrationHealth, SceneRef,
    };

    struct ConnectedStub;
    struct DisconnectedStub;

    #[async_trait::async_trait]
    impl HomeAutomationProvider for ConnectedStub {
        async fn health(&self) -> IntegrationHealth {
            IntegrationHealth {
                connected: true,
                cached_graph: true,
                message: "ok".into(),
            }
        }

        async fn sync_structure(&self) -> Result<HomeGraph> {
            anyhow::bail!("unused")
        }

        async fn resolve_target(
            &self,
            _query: &str,
            _action_hint: Option<crate::ha::HomeActionKind>,
        ) -> Result<HomeTarget> {
            anyhow::bail!("unused")
        }

        async fn get_state(&self, _target: &HomeTarget) -> Result<HomeState> {
            anyhow::bail!("unused")
        }

        async fn execute(&self, _action: HomeAction) -> Result<ActionResult> {
            anyhow::bail!("unused")
        }

        async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
            Ok(Vec::new())
        }

        async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl HomeAutomationProvider for DisconnectedStub {
        async fn health(&self) -> IntegrationHealth {
            IntegrationHealth {
                connected: false,
                cached_graph: false,
                message: "Home Assistant unavailable: timeout".into(),
            }
        }

        async fn sync_structure(&self) -> Result<HomeGraph> {
            anyhow::bail!("unused")
        }

        async fn resolve_target(
            &self,
            _query: &str,
            _action_hint: Option<crate::ha::HomeActionKind>,
        ) -> Result<HomeTarget> {
            anyhow::bail!("unused")
        }

        async fn get_state(&self, _target: &HomeTarget) -> Result<HomeState> {
            anyhow::bail!("unused")
        }

        async fn execute(&self, _action: HomeAction) -> Result<ActionResult> {
            anyhow::bail!("unused")
        }

        async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
            Ok(Vec::new())
        }

        async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn prefers_live_governor_memory() {
        let status = serde_json::json!({
            "mem_available_mb": 1024,
            "mem_available_mb_live": 2048,
        });

        assert_eq!(governor_mem_available_mb(&status), Some(2048));
    }

    #[test]
    fn formats_uptime_as_hours_and_minutes() {
        assert_eq!(format_uptime_secs(0), "0h 0m");
        assert_eq!(format_uptime_secs(3661), "1h 1m");
    }

    #[test]
    fn formats_load_average_triplet() {
        assert_eq!(
            format_load_average("0.00 0.01 0.05 1/123 456").as_deref(),
            Some("0.00 0.01 0.05")
        );
        assert_eq!(format_load_average("bad"), None);
    }

    #[tokio::test]
    async fn reports_home_assistant_connected() {
        let status = home_assistant_status(Some(&ConnectedStub)).await;
        assert!(status.contains("Home Assistant: connected"));
    }

    #[tokio::test]
    async fn reports_home_assistant_disabled_when_absent() {
        let status = home_assistant_status(None).await;
        assert_eq!(status, "Home Assistant: integration disabled");
    }

    #[tokio::test]
    async fn reports_home_assistant_unavailable_when_disconnected() {
        let status = home_assistant_status(Some(&DisconnectedStub)).await;
        assert!(status.contains("Home Assistant: unavailable"));
    }
}
