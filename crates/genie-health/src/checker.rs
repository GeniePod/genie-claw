use std::time::Duration;

use anyhow::Result;
use genie_common::config::Config;
use genie_common::probe::{ProbeTimeouts, probe_configured_url};
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

/// Materialize `(name, probe_url)` pairs for every service the health monitor
/// owns. Core's probe is derived from `[core].bind_host` and `[core].port` so it
/// tracks where core actually listens even if `[services.core].url` is stale.
fn collect_endpoints(config: &Config) -> Vec<(String, String)> {
    let mut endpoints = vec![
        ("core".into(), config.core_health_url()),
        ("llm".into(), config.services.llm.url.clone()),
    ];

    match config.api_status_url() {
        Ok(url) => endpoints.push(("api".into(), url)),
        Err(e) => tracing::warn!(
            error = %e,
            "skipping genie-api health probe; check [services.api].url"
        ),
    }

    if let Some(ref ha) = config.services.homeassistant {
        endpoints.push(("homeassistant".into(), ha.url.clone()));
    }

    if let Some(ref nc) = config.services.nextcloud {
        endpoints.push(("nextcloud".into(), nc.url.clone()));
    }
    if let Some(ref jf) = config.services.jellyfin {
        endpoints.push(("jellyfin".into(), jf.url.clone()));
    }

    endpoints
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
        let services = collect_endpoints(&self.config);

        for (name, url) in &services {
            let status = check_http(name, url).await;

            insert_health_log(&self.db, ts_ms, &status);

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
        prune_health_log(&self.db, cutoff);
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

fn insert_health_log(db: &Connection, ts_ms: u64, status: &ServiceStatus) {
    if let Err(e) = db.execute(
        "INSERT INTO health_log (ts_ms, service, healthy, response_ms, error) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            ts_ms,
            status.name,
            status.healthy as i32,
            status.response_ms,
            status.error,
        ],
    ) {
        tracing::error!(
            service = %status.name,
            error = %e,
            "failed to insert health_log row"
        );
    }
}

fn prune_health_log(db: &Connection, cutoff_ts_ms: u64) {
    if let Err(e) = db.execute("DELETE FROM health_log WHERE ts_ms < ?1", [cutoff_ts_ms]) {
        tracing::error!(
            cutoff_ts_ms,
            error = %e,
            "failed to prune health_log rows"
        );
    }
}

async fn check_http(name: &str, url: &str) -> ServiceStatus {
    let start = std::time::Instant::now();
    let timeouts = ProbeTimeouts {
        connect: Duration::from_secs(5),
        read: Duration::from_secs(5),
    };

    let result = probe_configured_url(url, timeouts).await;
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

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path,
        host_port,
        body.len(),
        body
    );

    let mut stream = stream;
    stream.write_all(request.as_bytes()).await?;

    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("read timeout"))??;

    let response = String::from_utf8_lossy(&buf[..n]);
    if response.starts_with("HTTP/1.") {
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
        Err(anyhow::anyhow!("invalid HTTP response"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::{
        ConnectivityConfig, CoreConfig, GovernorConfig, HealthConfig, PressureConfig,
        ServicesConfig, TelegramConfig, WebSearchConfig,
    };
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            data_dir: PathBuf::from("/tmp/geniepod-health-test"),
            core: CoreConfig::default(),
            agent: Default::default(),
            optional_ai_provider: Default::default(),
            privacy_proxy: Default::default(),
            governor: GovernorConfig {
                poll_interval_ms: 1000,
                night_start_hour: 23,
                day_start_hour: 6,
                night_model_swap: false,
                pressure: PressureConfig::default(),
            },
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
            telegram: TelegramConfig::default(),
            web_search: WebSearchConfig::default(),
            connectivity: ConnectivityConfig::default(),
            http: Default::default(),
        }
    }

    #[test]
    fn collect_endpoints_skips_unconfigured_homeassistant() {
        let endpoints = collect_endpoints(&test_config());
        let names: Vec<&str> = endpoints.iter().map(|(name, _)| name.as_str()).collect();

        assert!(names.contains(&"core"));
        assert!(names.contains(&"llm"));
        assert!(names.contains(&"api"));
        assert!(!names.contains(&"homeassistant"));
    }

    #[test]
    fn core_endpoint_url_tracks_configured_core_port() {
        let mut config = test_config();
        config.core.port = 3001;
        config.services.core.url = "http://127.0.0.1:3000/api/health".into();

        let endpoints = collect_endpoints(&config);
        let core_url = endpoints
            .iter()
            .find(|(name, _)| name == "core")
            .map(|(_, url)| url.as_str())
            .expect("core endpoint should always be present");

        assert_eq!(core_url, "http://127.0.0.1:3001/api/health");
    }

    #[test]
    fn llm_endpoint_url_still_sources_from_services_config() {
        let mut config = test_config();
        config.services.llm.url = "http://127.0.0.1:9999/v1/health".into();

        let endpoints = collect_endpoints(&config);
        let llm_url = endpoints
            .iter()
            .find(|(name, _)| name == "llm")
            .map(|(_, url)| url.as_str())
            .expect("llm endpoint should always be present");

        assert_eq!(llm_url, "http://127.0.0.1:9999/v1/health");
    }

    #[test]
    fn api_endpoint_url_uses_derived_status_url() {
        let mut config = test_config();
        config.services.api.url = "127.0.0.1:4080/api/status".into();

        let endpoints = collect_endpoints(&config);
        let api_url = endpoints
            .iter()
            .find(|(name, _)| name == "api")
            .map(|(_, url)| url.as_str())
            .expect("api endpoint should always be present when api_status_url parses");

        assert_eq!(api_url, "http://127.0.0.1:4080/api/status");
    }

    #[test]
    fn api_endpoint_omitted_when_status_url_unsupported() {
        let mut config = test_config();
        config.services.api.url = "https://api.example/api/status".into();

        let endpoints = collect_endpoints(&config);
        assert!(
            !endpoints.iter().any(|(name, _)| name == "api"),
            "https api url cannot be probed by plain HTTP client"
        );
    }

    fn open_test_db(dir: &std::path::Path) -> Connection {
        let db_path = dir.join("health.db");
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS health_log (
                ts_ms       INTEGER NOT NULL,
                service     TEXT NOT NULL,
                healthy     INTEGER NOT NULL,
                response_ms INTEGER NOT NULL,
                error       TEXT
            );
            ",
        )
        .unwrap();
        db
    }

    #[test]
    fn health_log_insert_and_prune_on_writable_db() {
        let dir =
            std::env::temp_dir().join(format!("genie-health-writable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db = open_test_db(&dir);
        let status = ServiceStatus {
            name: "core".into(),
            url: "http://127.0.0.1:3000/api/health".into(),
            healthy: true,
            response_ms: 12,
            error: None,
        };

        insert_health_log(&db, 1_000, &status);
        insert_health_log(&db, 2_000, &status);

        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM health_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        prune_health_log(&db, 1_500);
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM health_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn health_log_write_errors_do_not_panic_on_readonly_db() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("genie-health-readonly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db_path = dir.join("health.db");
        {
            let db = open_test_db(&dir);
            drop(db);
        }

        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&db_path, perms).unwrap();

        let db = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();

        let status = ServiceStatus {
            name: "core".into(),
            url: "http://127.0.0.1:3000/api/health".into(),
            healthy: false,
            response_ms: 0,
            error: Some("timeout".into()),
        };

        insert_health_log(&db, 9_000, &status);
        prune_health_log(&db, 0);

        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&db_path, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn send_http_post_rejects_non_http_tcp_banner() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/api/alert");

        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(b"SSH-2.0-OpenSSH_9.0\r\n").await;
            }
        });

        let error = send_http_post(&url, r#"{"message":"test"}"#)
            .await
            .unwrap_err();
        server.abort();

        assert!(
            error.to_string().contains("invalid HTTP response"),
            "expected invalid HTTP error, got: {error}"
        );
    }

    #[tokio::test]
    async fn send_http_post_accepts_valid_http_status_line() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/api/alert");

        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        send_http_post(&url, r#"{"message":"test"}"#)
            .await
            .expect("alert webhook POST should succeed on HTTP 204");
        server.abort();
    }

    #[tokio::test]
    async fn send_http_post_rejects_http_500_status_line() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/api/alert");

        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let error = send_http_post(&url, r#"{"message":"test"}"#)
            .await
            .unwrap_err();
        server.abort();

        assert!(
            error.to_string().contains("HTTP 500"),
            "expected HTTP 500 error, got: {error}"
        );
    }
}
