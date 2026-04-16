use anyhow::Result;
use genie_common::tegrastats;

/// Get system status: memory, uptime, governor mode.
pub async fn system_info() -> Result<String> {
    let mut info = Vec::new();
    let governor_status = query_governor_status().await;

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
}
