use std::time::Duration;

use anyhow::Result;
use genie_common::config::{Config, ServiceEndpoint};
use rusqlite::Connection;
use tokio::net::TcpStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::interval;

#[derive(Debug)]
struct ServiceStatus {
    name: String,
    #[allow(dead_code)]
    url: String,
    healthy: bool,
    response_ms: u64,
    error: Option<String>,
}

pub struct HealthMonitor {
    config: Config,
    db: Connection,
    /// Track consecutive failures per service for alert dedup.
    failure_counts: std::collections::HashMap<String, u32>,
}

impl HealthMonitor {
    pub fn new(config: Config) -> Result<Self> {
        let db_path = config.data_dir.join("health.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(&db_path)?;
        db.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS health_log (
                ts_ms       INTEGER NOT NULL,
                service     TEXT NOT NULL,
                healthy     INTEGER NOT NULL,
                response_ms INTEGER NOT NULL,
                error       TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_health_ts ON health_log(ts_ms);
            ",
        )?;

        Ok(Self {
            config,
            db,
            failure_counts: std::collections::HashMap::new(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let interval_secs = self.config.health.interval_secs;
        let mut tick = interval(Duration::from_secs(interval_secs));
        let mut sigterm = signal(SignalKind::terminate())?;

        tracing::info!(interval_secs, "health monitor loop started");
        sd_notify_ready();

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    self.check_all().await;
                    sd_notify_watchdog();
                }
                _ = sigterm.recv() => {
                    tracing::info!("SIGTERM received, shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn check_all(&mut self) {
        let ts_ms = now_ms();

        // Collect endpoints as owned data to avoid borrowing self in the loop.
        let services: Vec<(String, String)> = self
            .collect_endpoints()
            .into_iter()
            .map(|(name, ep)| (name, ep.url.clone()))
            .collect();

        for (name, url) in &services {
            let status = check_http(name, url).await;

            // Log to SQLite.
            let _ = self.db.execute(
                "INSERT INTO health_log (ts_ms, service, healthy, response_ms, error) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    ts_ms,
                    status.name,
                    status.healthy as i32,
                    status.response_ms,
                    status.error,
                ],
            );

            if status.healthy {
                if self.failure_counts.remove(name).is_some() {
                    tracing::info!(service = name, "service recovered");
                }
            } else {
                let count = self.failure_counts.entry(name.clone()).or_insert(0);
                *count += 1;

                tracing::warn!(
                    service = name,
                    consecutive_failures = *count,
                    error = status.error.as_deref().unwrap_or("unknown"),
                    "service unhealthy"
                );

                // Alert on first failure and every 10th consecutive failure.
                if *count == 1 || (*count).is_multiple_of(10) {
                    self.send_alert(&status).await;
                }
            }
        }

        // Prune logs older than 24h every ~120 checks (~1 hour at 30s interval).
        let cutoff = ts_ms.saturating_sub(24 * 3600 * 1000);
        let _ = self
            .db
            .execute("DELETE FROM health_log WHERE ts_ms < ?1", [cutoff]);
    }

    fn collect_endpoints(&self) -> Vec<(String, &ServiceEndpoint)> {
        let mut endpoints = vec![
            ("core".into(), &self.config.services.core),
            ("llm".into(), &self.config.services.llm),
        ];

        if let Some(ref ha) = self.config.services.homeassistant {
            endpoints.push(("homeassistant".into(), ha));
        }

        if let Some(ref nc) = self.config.services.nextcloud {
            endpoints.push(("nextcloud".into(), nc));
        }
        if let Some(ref jf) = self.config.services.jellyfin {
            endpoints.push(("jellyfin".into(), jf));
        }

        endpoints
    }

    async fn send_alert(&self, status: &ServiceStatus) {
        if !self.config.health.alert_enabled || self.config.health.alert_webhook_url.is_empty() {
            return;
        }

        let message = format!(
            "[GeniePod] {} is DOWN: {}",
            status.name,
            status.error.as_deref().unwrap_or("unreachable")
        );

        tracing::info!(service = %status.name, "sending alert to local webhook");

        // POST to an optional local notifier endpoint.
        let url = format!("{}/api/alert", self.config.health.alert_webhook_url);

        let payload = serde_json::json!({
            "message": message,
            "service": status.name,
            "severity": "critical",
        });

        // Use a raw TCP + HTTP/1.1 request to avoid pulling in reqwest/hyper.
        if let Err(e) = send_http_post(&url, &payload.to_string()).await {
            tracing::warn!(error = %e, "failed to send alert to local webhook");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::{
        CoreConfig, GovernorConfig, HealthConfig, PressureConfig, ServicesConfig,
    };
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            data_dir: PathBuf::from("/tmp/geniepod-health-test"),
            core: CoreConfig::default(),
            governor: GovernorConfig {
                poll_interval_ms: 1000,
                night_start_hour: 23,
                day_start_hour: 6,
                night_model_swap: false,
                pressure: PressureConfig::default(),
            },
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
        }
    }

    #[test]
    fn collect_endpoints_skips_unconfigured_homeassistant() {
        let monitor = HealthMonitor::new(test_config()).unwrap();
        let endpoints = monitor.collect_endpoints();
        let names: Vec<&str> = endpoints.iter().map(|(name, _)| name.as_str()).collect();

        assert!(names.contains(&"core"));
        assert!(names.contains(&"llm"));
        assert!(!names.contains(&"homeassistant"));
    }
}

async fn check_http(name: &str, url: &str) -> ServiceStatus {
    let start = std::time::Instant::now();

    // Parse host:port from URL for TCP connect.
    let result = async {
        let url_parsed = url.strip_prefix("http://").unwrap_or(url);
        let (host_port, _path) = url_parsed.split_once('/').unwrap_or((url_parsed, ""));

        let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(host_port))
            .await
            .map_err(|_| anyhow::anyhow!("timeout"))??;

        // Send a minimal HTTP GET.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path = url_parsed
            .find('/')
            .map(|i| &url_parsed[i..])
            .unwrap_or("/");

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, host_port
        );

        let mut stream = stream;
        stream.write_all(request.as_bytes()).await?;

        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
            .await
            .map_err(|_| anyhow::anyhow!("read timeout"))??;

        let response = String::from_utf8_lossy(&buf[..n]);
        if response.starts_with("HTTP/1.") {
            // Extract status code.
            let status_code: u16 = response
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if (200..400).contains(&status_code) {
                Ok(())
            } else {
                Err(anyhow::anyhow!("HTTP {}", status_code))
            }
        } else {
            // Non-HTTP but something responded — treat as alive.
            Ok(())
        }
    }
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(()) => ServiceStatus {
            name: name.into(),
            url: url.into(),
            healthy: true,
            response_ms: elapsed_ms,
            error: None,
        },
        Err(e) => ServiceStatus {
            name: name.into(),
            url: url.into(),
            healthy: false,
            response_ms: elapsed_ms,
            error: Some(e.to_string()),
        },
    }
}

async fn send_http_post(url: &str, body: &str) -> Result<()> {
    let url_parsed = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = url_parsed.split_once('/').unwrap_or((url_parsed, ""));
    let path = format!("/{}", path);

    let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(host_port))
        .await
        .map_err(|_| anyhow::anyhow!("timeout"))??;

    use tokio::io::AsyncWriteExt;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path,
        host_port,
        body.len(),
        body
    );

    let mut stream = stream;
    stream.write_all(request.as_bytes()).await?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sd_notify_ready() {
    if let Ok(addr) = std::env::var("NOTIFY_SOCKET") {
        let _ = std::os::unix::net::UnixDatagram::unbound()
            .and_then(|sock| sock.send_to(b"READY=1", &addr));
    }
}

fn sd_notify_watchdog() {
    if let Ok(addr) = std::env::var("NOTIFY_SOCKET") {
        let _ = std::os::unix::net::UnixDatagram::unbound()
            .and_then(|sock| sock.send_to(b"WATCHDOG=1", &addr));
    }
}
