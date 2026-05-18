use genie_common::config::Config;
use genie_common::tegrastats;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;

use crate::http::Response;

/// GET /api/status — current mode, memory, uptime.
pub async fn get_status(_config: &Config) -> Response {
    // Read governor status via its Unix socket.
    let governor_status = query_governor(r#"{"cmd":"status"}"#).await;

    // Augment with live memory reading.
    let mem_avail = tegrastats::mem_available_mb().unwrap_or(0);

    let body = if let Some(mut status) = governor_status {
        // Merge live mem_available into the governor's response.
        if let Some(obj) = status.as_object_mut() {
            obj.insert(
                "mem_available_mb_live".into(),
                serde_json::Value::from(mem_avail),
            );
        }
        serde_json::to_string(&status).unwrap_or_default()
    } else {
        // Governor not running — return basic info.
        serde_json::json!({
            "mode": "unknown",
            "mem_available_mb": mem_avail,
            "governor": "offline"
        })
        .to_string()
    };

    Response {
        status: 200,
        content_type: "application/json",
        body,
    }
}

/// GET /api/tegrastats — recent history from governor's SQLite.
pub async fn get_tegrastats(config: &Config) -> Response {
    let db_path = config.data_dir.join("governor.db");

    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let conn =
            rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare(
                "SELECT ts_ms, ram_used_mb, ram_total_mb, gpu_freq_pct, gpu_temp_c, cpu_temp_c, power_mw
                 FROM tegrastats
                 ORDER BY ts_ms DESC
                 LIMIT 720",
            )
            .map_err(|e| e.to_string())?;

        let rows: Vec<serde_json::Value> = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "ts": row.get::<_, i64>(0)?,
                    "ram_used": row.get::<_, i64>(1)?,
                    "ram_total": row.get::<_, i64>(2)?,
                    "gpu_pct": row.get::<_, i64>(3)?,
                    "gpu_c": row.get::<_, Option<f64>>(4)?,
                    "cpu_c": row.get::<_, Option<f64>>(5)?,
                    "power_mw": row.get::<_, Option<i64>>(6)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        serde_json::to_string(&rows).map_err(|e| e.to_string())
    })
    .await;

    match result {
        Ok(Ok(json)) => Response {
            status: 200,
            content_type: "application/json",
            body: json,
        },
        _ => Response {
            status: 200,
            content_type: "application/json",
            body: "[]".into(),
        },
    }
}

#[derive(Debug, Clone)]
struct ServiceTarget {
    service: String,
    unit: String,
    latency_url: Option<String>,
}

#[derive(Debug, Clone)]
struct HealthRow {
    healthy: bool,
    response_ms: i64,
    error: Option<String>,
    last_check: i64,
}

#[derive(Debug, Clone)]
struct LiveLatencyRow {
    healthy: bool,
    response_ms: i64,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct SystemdRow {
    load_state: String,
    active_state: String,
    sub_state: String,
    unit_file_state: String,
    result: String,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceRow {
    service: String,
    unit: String,
    healthy: bool,
    response_ms: Option<i64>,
    latency_source: &'static str,
    error: Option<String>,
    last_check: Option<i64>,
    source: &'static str,
    load_state: String,
    active_state: String,
    sub_state: String,
    unit_file_state: String,
    result: String,
}

/// GET /api/services — URL health plus systemd state for deployed services.
pub async fn get_services(config: &Config) -> Response {
    let db_path = config.data_dir.join("health.db");
    let targets = dashboard_service_targets(config);
    let health = read_latest_health_rows(db_path).await.unwrap_or_default();
    let live_latency = collect_live_latency_rows(&targets, &health).await;
    let mut systemd = BTreeMap::new();

    for unit in unique_units(&targets) {
        systemd.insert(unit.clone(), query_systemd_unit(&unit).await);
    }

    let rows = merge_service_rows(&targets, &health, &live_latency, &systemd);
    let body = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into());

    Response {
        status: 200,
        content_type: "application/json",
        body,
    }
}

async fn read_latest_health_rows(
    db_path: std::path::PathBuf,
) -> Result<BTreeMap<String, HealthRow>, String> {
    let result =
        tokio::task::spawn_blocking(move || -> Result<BTreeMap<String, HealthRow>, String> {
            let conn = rusqlite::Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| e.to_string())?;

            // Get the latest health check for each URL-polled service.
            let mut stmt = conn
                .prepare(
                    "SELECT h.service, h.healthy, h.response_ms, h.error, h.ts_ms
                 FROM health_log h
                 JOIN (
                     SELECT service, MAX(ts_ms) AS ts_ms
                     FROM health_log
                     GROUP BY service
                 ) latest
                   ON latest.service = h.service AND latest.ts_ms = h.ts_ms
                 ORDER BY h.service",
                )
                .map_err(|e| e.to_string())?;

            let rows: BTreeMap<String, HealthRow> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        HealthRow {
                            healthy: row.get::<_, i32>(1)? == 1,
                            response_ms: row.get::<_, i64>(2)?,
                            error: row.get::<_, Option<String>>(3)?,
                            last_check: row.get::<_, i64>(4)?,
                        },
                    ))
                })
                .map_err(|e| e.to_string())?
                .filter_map(|r| r.ok())
                .collect();

            Ok(rows)
        })
        .await;

    match result {
        Ok(rows) => rows,
        Err(e) => Err(e.to_string()),
    }
}

fn dashboard_service_targets(config: &Config) -> Vec<ServiceTarget> {
    let mut targets = vec![
        ServiceTarget {
            service: "core".into(),
            unit: config.services.core.systemd_unit.clone(),
            latency_url: Some(config.services.core.url.clone()),
        },
        ServiceTarget {
            service: "llm".into(),
            unit: config.services.llm.systemd_unit.clone(),
            latency_url: Some(config.services.llm.url.clone()),
        },
        ServiceTarget {
            service: "api".into(),
            unit: "genie-api.service".into(),
            latency_url: Some("http://127.0.0.1:3080/api/status".into()),
        },
        ServiceTarget {
            service: "health".into(),
            unit: "genie-health.service".into(),
            latency_url: None,
        },
        ServiceTarget {
            service: "governor".into(),
            unit: "genie-governor.service".into(),
            latency_url: None,
        },
        ServiceTarget {
            service: "mqtt".into(),
            unit: "genie-mqtt.service".into(),
            latency_url: None,
        },
        ServiceTarget {
            service: "audio".into(),
            unit: "genie-audio.service".into(),
            latency_url: None,
        },
        ServiceTarget {
            service: "whisper".into(),
            unit: "genie-whisper.service".into(),
            latency_url: None,
        },
        ServiceTarget {
            service: "wakeword".into(),
            unit: "genie-wakeword.service".into(),
            latency_url: None,
        },
        ServiceTarget {
            service: "homeassistant".into(),
            unit: config
                .services
                .homeassistant
                .as_ref()
                .map(|service| service.systemd_unit.clone())
                .unwrap_or_else(|| "homeassistant.service".into()),
            latency_url: config
                .services
                .homeassistant
                .as_ref()
                .map(|service| service.url.clone()),
        },
    ];

    if let Some(nextcloud) = &config.services.nextcloud {
        targets.push(ServiceTarget {
            service: "nextcloud".into(),
            unit: nextcloud.systemd_unit.clone(),
            latency_url: Some(nextcloud.url.clone()),
        });
    }
    if let Some(jellyfin) = &config.services.jellyfin {
        targets.push(ServiceTarget {
            service: "jellyfin".into(),
            unit: jellyfin.systemd_unit.clone(),
            latency_url: Some(jellyfin.url.clone()),
        });
    }

    targets
}

fn unique_units(targets: &[ServiceTarget]) -> Vec<String> {
    targets
        .iter()
        .map(|target| target.unit.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn merge_service_rows(
    targets: &[ServiceTarget],
    health: &BTreeMap<String, HealthRow>,
    live_latency: &BTreeMap<String, LiveLatencyRow>,
    systemd: &BTreeMap<String, SystemdRow>,
) -> Vec<ServiceRow> {
    targets
        .iter()
        .map(|target| {
            let health_row = health.get(&target.service);
            let live_row = live_latency.get(&target.service);
            let systemd_row = systemd
                .get(&target.unit)
                .cloned()
                .unwrap_or_else(|| SystemdRow {
                    error: Some("systemd status unavailable".into()),
                    ..SystemdRow::default()
                });
            let systemd_active = systemd_row.active_state == "active";
            let missing = systemd_row.load_state == "not-found";
            let endpoint_healthy = health_row
                .map(|row| row.healthy)
                .or_else(|| live_row.map(|row| row.healthy))
                .unwrap_or(true);
            let healthy = !missing && systemd_active && endpoint_healthy;
            let endpoint_error = health_row
                .and_then(|row| row.error.clone())
                .or_else(|| live_row.and_then(|row| row.error.clone()));
            let systemd_error = systemd_row
                .error
                .clone()
                .or_else(|| service_state_error(&systemd_row));
            let error = if missing || !systemd_active {
                systemd_error.or(endpoint_error)
            } else {
                endpoint_error.or(systemd_error)
            };
            let (response_ms, latency_source, source) = if let Some(row) = health_row {
                (Some(row.response_ms), "health", "health+systemd")
            } else if let Some(row) = live_row {
                (Some(row.response_ms), "live", "live+systemd")
            } else {
                (None, "not_applicable", "systemd")
            };

            ServiceRow {
                service: target.service.clone(),
                unit: target.unit.clone(),
                healthy,
                response_ms,
                latency_source,
                error,
                last_check: health_row.map(|row| row.last_check),
                source,
                load_state: systemd_row.load_state,
                active_state: systemd_row.active_state,
                sub_state: systemd_row.sub_state,
                unit_file_state: systemd_row.unit_file_state,
                result: systemd_row.result,
            }
        })
        .collect()
}

async fn collect_live_latency_rows(
    targets: &[ServiceTarget],
    health: &BTreeMap<String, HealthRow>,
) -> BTreeMap<String, LiveLatencyRow> {
    let mut rows = BTreeMap::new();

    for target in targets {
        if health.contains_key(&target.service) {
            continue;
        }
        let Some(url) = target.latency_url.as_deref() else {
            continue;
        };
        rows.insert(target.service.clone(), probe_http_latency(url).await);
    }

    rows
}

async fn probe_http_latency(url: &str) -> LiveLatencyRow {
    let start = Instant::now();
    let result = async {
        let url = url.strip_prefix("http://").unwrap_or(url);
        let (host_port, path) = url.split_once('/').unwrap_or((url, ""));
        let path = format!("/{path}");

        let mut stream =
            tokio::time::timeout(Duration::from_millis(750), TcpStream::connect(host_port))
                .await
                .map_err(|_| "connect timeout".to_string())?
                .map_err(|e| e.to_string())?;

        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| e.to_string())?;

        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_millis(750), stream.read(&mut buf))
            .await
            .map_err(|_| "read timeout".to_string())?
            .map_err(|e| e.to_string())?;

        let response = String::from_utf8_lossy(&buf[..n]);
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);

        if (200..400).contains(&status) {
            Ok(())
        } else if status > 0 {
            Err(format!("HTTP {status}"))
        } else {
            Err("invalid HTTP response".into())
        }
    }
    .await;

    LiveLatencyRow {
        healthy: result.is_ok(),
        response_ms: start.elapsed().as_millis() as i64,
        error: result.err(),
    }
}

fn service_state_error(row: &SystemdRow) -> Option<String> {
    match row.load_state.as_str() {
        "not-found" => Some("unit not installed".into()),
        "" => None,
        _ if row.active_state == "active" => None,
        _ if !row.sub_state.is_empty() => Some(row.sub_state.clone()),
        _ if !row.active_state.is_empty() => Some(row.active_state.clone()),
        _ => None,
    }
}

async fn query_systemd_unit(unit: &str) -> SystemdRow {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        Command::new("systemctl")
            .arg("show")
            .arg(unit)
            .arg("--no-page")
            .arg("--property=LoadState")
            .arg("--property=ActiveState")
            .arg("--property=SubState")
            .arg("--property=UnitFileState")
            .arg("--property=Result")
            .output(),
    )
    .await;

    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return SystemdRow {
                error: Some(e.to_string()),
                ..SystemdRow::default()
            };
        }
        Err(_) => {
            return SystemdRow {
                error: Some("systemctl timed out".into()),
                ..SystemdRow::default()
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut row = SystemdRow::default();

    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "LoadState" => row.load_state = value.into(),
                "ActiveState" => row.active_state = value.into(),
                "SubState" => row.sub_state = value.into(),
                "UnitFileState" => row.unit_file_state = value.into(),
                "Result" => row.result = value.into(),
                _ => {}
            }
        }
    }

    if !output.status.success() && !stderr.trim().is_empty() {
        row.error = Some(stderr.trim().into());
    }

    row
}

/// GET /api/security — redacted household security posture.
pub async fn get_security(config: &Config) -> Response {
    Response {
        status: 200,
        content_type: "application/json",
        body: config.household_security_summary().to_string(),
    }
}

/// POST /api/mode — send mode change command to governor.
pub async fn post_mode(body: Option<&str>) -> Response {
    let Some(body) = body else {
        return Response {
            status: 400,
            content_type: "application/json",
            body: r#"{"error":"missing body"}"#.into(),
        };
    };

    // Forward the command to the governor via its control socket.
    let result = query_governor(body).await;

    match result {
        Some(val) => Response {
            status: 200,
            content_type: "application/json",
            body: val.to_string(),
        },
        None => Response {
            status: 500,
            content_type: "application/json",
            body: r#"{"error":"governor unreachable"}"#.into(),
        },
    }
}

/// Query the governor via its Unix control socket.
async fn query_governor(json_cmd: &str) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect("/run/geniepod/governor.sock")
        .await
        .ok()?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(json_cmd.as_bytes()).await.ok()?;
    writer.write_all(b"\n").await.ok()?;

    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .ok()?
        .ok()?;

    line.and_then(|l| serde_json::from_str(&l).ok())
}

/// GET / — serve the dashboard HTML.
pub fn serve_dashboard() -> Response {
    Response {
        status: 200,
        content_type: "text/html; charset=utf-8",
        body: include_str!("../../dashboard/index.html").into(),
    }
}

/// GET /dashboard.js — serve the dashboard JavaScript.
pub fn serve_dashboard_js() -> Response {
    Response {
        status: 200,
        content_type: "application/javascript; charset=utf-8",
        body: include_str!("../../dashboard/dashboard.js").into(),
    }
}

struct CoreProxyResponse {
    status: u16,
    body: String,
}

pub async fn get_actuation_pending(_config: &Config) -> Response {
    match proxy_core_json("GET", "/api/actuation/pending", None).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn get_runtime_contract(_config: &Config) -> Response {
    match proxy_core_json("GET", "/api/runtime/contract", None).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn get_actuation_actions(_config: &Config) -> Response {
    match proxy_core_json("GET", "/api/actuation/actions", None).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn get_actuation_audit(config: &Config) -> Response {
    let path = config.data_dir.join("safety/actuation-audit.jsonl");
    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        if !path.exists() {
            return Ok("[]".into());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let items = text
            .lines()
            .rev()
            .take(50)
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect::<Vec<_>>();
        serde_json::to_string(&items).map_err(|e| e.to_string())
    })
    .await;

    match result {
        Ok(Ok(body)) => Response {
            status: 200,
            content_type: "application/json",
            body,
        },
        Ok(Err(e)) => Response {
            status: 500,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
        Err(e) => Response {
            status: 500,
            content_type: "application/json",
            body: serde_json::json!({ "error": e.to_string() }).to_string(),
        },
    }
}

pub async fn post_actuation_confirm(_config: &Config, body: Option<&str>) -> Response {
    let Some(body) = body else {
        return Response {
            status: 400,
            content_type: "application/json",
            body: r#"{"error":"missing body"}"#.into(),
        };
    };

    match proxy_core_json("POST", "/api/actuation/confirm", Some(body)).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn get_memories(_config: &Config) -> Response {
    match proxy_core_json("GET", "/api/memories", None).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn post_memory_update(_config: &Config, body: Option<&str>) -> Response {
    let Some(body) = body else {
        return Response {
            status: 400,
            content_type: "application/json",
            body: r#"{"error":"missing body"}"#.into(),
        };
    };
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(req) => req,
        Err(e) => {
            return Response {
                status: 400,
                content_type: "application/json",
                body: serde_json::json!({ "error": e.to_string() }).to_string(),
            };
        }
    };
    let payload = serde_json::to_string(&parsed).unwrap_or_else(|_| body.to_string());
    match proxy_core_json("POST", "/api/memories/update", Some(&payload)).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn post_memory_delete(_config: &Config, body: Option<&str>) -> Response {
    let Some(body) = body else {
        return Response {
            status: 400,
            content_type: "application/json",
            body: r#"{"error":"missing body"}"#.into(),
        };
    };
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(req) => req,
        Err(e) => {
            return Response {
                status: 400,
                content_type: "application/json",
                body: serde_json::json!({ "error": e.to_string() }).to_string(),
            };
        }
    };
    let payload = serde_json::to_string(&parsed).unwrap_or_else(|_| body.to_string());
    match proxy_core_json("POST", "/api/memories/delete", Some(&payload)).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

pub async fn post_memory_reorder(_config: &Config, body: Option<&str>) -> Response {
    let Some(body) = body else {
        return Response {
            status: 400,
            content_type: "application/json",
            body: r#"{"error":"missing body"}"#.into(),
        };
    };
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(req) => req,
        Err(e) => {
            return Response {
                status: 400,
                content_type: "application/json",
                body: serde_json::json!({ "error": e.to_string() }).to_string(),
            };
        }
    };
    let payload = serde_json::to_string(&parsed).unwrap_or_else(|_| body.to_string());
    match proxy_core_json("POST", "/api/memories/reorder", Some(&payload)).await {
        Ok(proxy) => Response {
            status: proxy.status,
            content_type: "application/json",
            body: proxy.body,
        },
        Err(e) => Response {
            status: 502,
            content_type: "application/json",
            body: serde_json::json!({ "error": e }).to_string(),
        },
    }
}

async fn proxy_core_json(
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<CoreProxyResponse, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let mut stream = TcpStream::connect("127.0.0.1:3000")
        .await
        .map_err(|e| e.to_string())?;
    let body_str = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| e.to_string())?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "invalid core response".to_string())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "invalid core status".to_string())?;
    Ok(CoreProxyResponse {
        status,
        body: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::{
        ConnectivityConfig, CoreConfig, GovernorConfig, HealthConfig, ServicesConfig,
        TelegramConfig, WebSearchConfig,
    };
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            data_dir: PathBuf::from("/tmp/geniepod-api-test"),
            core: CoreConfig::default(),
            governor: GovernorConfig::default(),
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
            telegram: TelegramConfig::default(),
            web_search: WebSearchConfig::default(),
            connectivity: ConnectivityConfig::default(),
        }
    }

    #[test]
    fn dashboard_targets_include_deployed_stack_services() {
        let config = test_config();
        let targets = dashboard_service_targets(&config);
        let names: Vec<&str> = targets
            .iter()
            .map(|target| target.service.as_str())
            .collect();

        for expected in [
            "core",
            "llm",
            "api",
            "health",
            "governor",
            "mqtt",
            "audio",
            "whisper",
            "wakeword",
            "homeassistant",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn service_rows_merge_health_and_systemd_status() {
        let targets = vec![
            ServiceTarget {
                service: "core".into(),
                unit: "genie-core.service".into(),
                latency_url: Some("http://127.0.0.1:3000/api/health".into()),
            },
            ServiceTarget {
                service: "api".into(),
                unit: "genie-api.service".into(),
                latency_url: Some("http://127.0.0.1:3080/api/status".into()),
            },
        ];
        let health = BTreeMap::from([(
            "core".into(),
            HealthRow {
                healthy: true,
                response_ms: 42,
                error: None,
                last_check: 123,
            },
        )]);
        let systemd = BTreeMap::from([
            (
                "genie-core.service".into(),
                SystemdRow {
                    load_state: "loaded".into(),
                    active_state: "active".into(),
                    sub_state: "running".into(),
                    unit_file_state: "enabled".into(),
                    result: "success".into(),
                    error: None,
                },
            ),
            (
                "genie-api.service".into(),
                SystemdRow {
                    load_state: "loaded".into(),
                    active_state: "active".into(),
                    sub_state: "running".into(),
                    unit_file_state: "enabled".into(),
                    result: "success".into(),
                    error: None,
                },
            ),
        ]);
        let live_latency = BTreeMap::from([(
            "api".into(),
            LiveLatencyRow {
                healthy: false,
                response_ms: 17,
                error: Some("HTTP 503".into()),
            },
        )]);

        let rows = merge_service_rows(&targets, &health, &live_latency, &systemd);
        let core = rows.iter().find(|row| row.service == "core").unwrap();
        let api = rows.iter().find(|row| row.service == "api").unwrap();

        assert!(core.healthy);
        assert_eq!(core.response_ms, Some(42));
        assert_eq!(core.latency_source, "health");
        assert_eq!(core.source, "health+systemd");
        assert!(!api.healthy);
        assert_eq!(api.response_ms, Some(17));
        assert_eq!(api.latency_source, "live");
        assert_eq!(api.error.as_deref(), Some("HTTP 503"));
        assert_eq!(api.source, "live+systemd");
    }

    #[test]
    fn systemd_only_service_rows_mark_latency_not_applicable() {
        let targets = vec![ServiceTarget {
            service: "wakeword".into(),
            unit: "genie-wakeword.service".into(),
            latency_url: None,
        }];
        let systemd = BTreeMap::from([(
            "genie-wakeword.service".into(),
            SystemdRow {
                load_state: "loaded".into(),
                active_state: "failed".into(),
                sub_state: "failed".into(),
                unit_file_state: "disabled".into(),
                result: "exit-code".into(),
                error: None,
            },
        )]);

        let rows = merge_service_rows(&targets, &BTreeMap::new(), &BTreeMap::new(), &systemd);
        let wakeword = rows.first().unwrap();

        assert!(!wakeword.healthy);
        assert_eq!(wakeword.response_ms, None);
        assert_eq!(wakeword.latency_source, "not_applicable");
        assert_eq!(wakeword.error.as_deref(), Some("failed"));
        assert_eq!(wakeword.source, "systemd");
    }
}
