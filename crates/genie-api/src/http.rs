use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use genie_common::config::Config;
use genie_common::http::{HttpLimits, read_request};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::Semaphore;

use crate::routes;

/// Largest request body genie-api will read into memory. The dashboard only
/// sends tiny JSON control payloads, so this stays well below genie-core's
/// 64 KiB cap. Mirrored into the header phase via `[http].max_header_bytes`.
const API_MAX_BODY_BYTES: usize = 4 * 1024;

/// Minimal HTTP/1.1 server — no framework, no allocator overhead.
///
/// Handles one request per connection (Connection: close).
/// This is intentional: the dashboard polls every 5 seconds,
/// and the API serves <10 concurrent clients on a home appliance.
///
/// The inbound reader is bounded and deadline-guarded (issue #195): oversized
/// request lines/headers are rejected, idle connections are dropped after a
/// read timeout, transient `accept()` errors never terminate the daemon, and
/// concurrent connections are capped by a semaphore.
pub async fn serve(addr: &str, config: Config) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(addr, "listening");
    serve_listener(listener, config).await
}

/// Accept connections from an already-bound `TcpListener`.
///
/// Split out from [`serve`] so tests can pre-bind to port 0 and drive the
/// hardened reader/accept loop directly.
pub(crate) async fn serve_listener(listener: TcpListener, config: Config) -> Result<()> {
    let config = Arc::new(config);
    let limits = HttpLimits::from_config(&config.http, API_MAX_BODY_BYTES);
    let max_connections = config.http.max_connections.max(1);
    // Bound concurrently handled connections so a flood cannot exhaust fds.
    let semaphore = Arc::new(Semaphore::new(max_connections));

    loop {
        // Reserve a slot before accepting; connections beyond the ceiling stay
        // parked in the OS backlog rather than being spawned unbounded.
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break, // semaphore closed — shutting down
        };
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                // A transient accept() error (e.g. EMFILE under a connection
                // flood) must never propagate out and terminate the daemon.
                tracing::warn!(error = %e, "accept failed; continuing");
                drop(permit);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let config = config.clone();

        tokio::spawn(async move {
            // Hold the permit for the lifetime of the request.
            let _permit = permit;
            if let Err(e) = handle_connection(stream, &config, &limits).await {
                tracing::debug!(peer = %peer, error = %e, "connection error");
            }
        });
    }

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    config: &Config,
    limits: &HttpLimits,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // Bounded, deadline-guarded request read (issue #195). Oversized headers
    // get a 431, an oversized body a 413; a stalled or vanished peer is just
    // dropped.
    let request = match read_request(&mut buf_reader, limits).await {
        Ok(request) => request,
        Err(e) => {
            if let Some(status) = e.status_code() {
                let _ = write_response(&mut writer, &error_response(status)).await;
            }
            tracing::debug!(error = %e, "rejected request");
            return Ok(());
        }
    };

    let body = request.body;
    let method = request.method;
    let path = request.path;

    // Route the request.
    let response = match (method.as_str(), path.as_str()) {
        ("GET", "/api/status") => routes::get_status(config).await,
        ("GET", "/api/tegrastats") => routes::get_tegrastats(config).await,
        ("GET", "/api/services") => routes::get_services(config).await,
        ("GET", "/api/security") => routes::get_security(config).await,
        ("GET", "/api/runtime/contract") => routes::get_runtime_contract(config).await,
        ("GET", "/api/actuation/pending") => routes::get_actuation_pending(config).await,
        ("GET", "/api/actuation/actions") => routes::get_actuation_actions(config).await,
        ("GET", "/api/actuation/audit") => routes::get_actuation_audit(config).await,
        ("POST", "/api/actuation/confirm") => {
            routes::post_actuation_confirm(config, body.as_deref()).await
        }
        ("GET", "/api/memories") => routes::get_memories(config).await,
        ("POST", "/api/memories/update") => {
            routes::post_memory_update(config, body.as_deref()).await
        }
        ("POST", "/api/memories/delete") => {
            routes::post_memory_delete(config, body.as_deref()).await
        }
        ("POST", "/api/memories/reorder") => {
            routes::post_memory_reorder(config, body.as_deref()).await
        }
        ("POST", "/api/mode") => routes::post_mode(body.as_deref()).await,
        ("GET", "/" | "/index.html") => routes::serve_dashboard(),
        ("GET", "/dashboard.js") => routes::serve_dashboard_js(),
        _ => Response {
            status: 404,
            content_type: "application/json",
            body: r#"{"error":"not found"}"#.into(),
        },
    };

    write_response(&mut writer, &response).await
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

/// JSON error response used to reject oversized requests (431 / 413) before
/// routing.
fn error_response(status: u16) -> Response {
    Response {
        status,
        content_type: "application/json",
        body: format!(r#"{{"error":"{}"}}"#, status_text(status)),
    }
}

async fn write_response(writer: &mut OwnedWriteHalf, response: &Response) -> Result<()> {
    let http_response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        response.status,
        status_text(response.status),
        response.content_type,
        response.body.len(),
    );

    writer.write_all(http_response.as_bytes()).await?;
    writer.write_all(response.body.as_bytes()).await?;
    writer.flush().await?;

    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use genie_common::config::Config;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// Parse a `Config` from a TOML fragment; every field has a serde default,
    /// so the fragment only needs the `[http]` overrides under test.
    fn config_with_http(fragment: &str) -> Config {
        toml::from_str(fragment).expect("config parses")
    }

    /// Bind to an ephemeral port, start the hardened listener, return the port.
    async fn start_server(config: Config) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = super::serve_listener(listener, config).await;
        });
        port
    }

    async fn read_all(mut stream: TcpStream, timeout: Duration) -> String {
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(timeout, stream.read_to_end(&mut buf)).await;
        String::from_utf8_lossy(&buf).to_string()
    }

    #[tokio::test]
    async fn oversized_request_header_is_rejected_and_server_survives() {
        let config = config_with_http(
            "[http]\nmax_header_line_bytes = 256\nread_timeout_secs = 2\nmax_connections = 8\n",
        );
        let port = start_server(config).await;

        // Oversized header line → 431, in bounded memory.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let pad = "A".repeat(2048);
        let req = format!("GET /api/status HTTP/1.1\r\nX-Pad: {pad}\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let resp = read_all(stream, Duration::from_secs(5)).await;
        assert!(
            resp.starts_with("HTTP/1.1 431"),
            "expected 431, got: {resp:?}"
        );

        // The daemon survives: a well-formed request is still routed.
        let mut stream2 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream2
            .write_all(b"GET /does-not-exist HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let resp2 = read_all(stream2, Duration::from_secs(5)).await;
        assert!(
            resp2.starts_with("HTTP/1.1 404"),
            "expected 404 after rejection, got: {resp2:?}"
        );
    }

    #[tokio::test]
    async fn idle_connection_is_dropped_after_read_timeout() {
        let config = config_with_http("[http]\nread_timeout_secs = 1\nmax_connections = 8\n");
        let port = start_server(config).await;

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // Partial request, never terminated.
        stream
            .write_all(b"GET /api/status HTTP/1.1\r\nX-Partial: ")
            .await
            .unwrap();

        let start = std::time::Instant::now();
        let mut buf = Vec::new();
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
            .await
            .expect("server did not drop the idle connection within 5s")
            .unwrap();
        assert_eq!(
            n,
            0,
            "idle connection should close with no response, got: {:?}",
            String::from_utf8_lossy(&buf)
        );
        assert!(start.elapsed() >= Duration::from_millis(500));
    }

    #[tokio::test]
    async fn connection_flood_does_not_wedge_server() {
        let config = config_with_http("[http]\nread_timeout_secs = 1\nmax_connections = 4\n");
        let port = start_server(config).await;

        // More stalled peers than the ceiling, kept open so they don't EOF.
        let mut stalled = Vec::new();
        for _ in 0..8 {
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let _ = s.write_all(b"G").await;
            stalled.push(s);
        }

        // A well-formed request still gets served once stalled peers time out.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /does-not-exist HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut buf))
            .await
            .expect("server wedged: no response within 8s under connection flood")
            .unwrap();
        let resp = String::from_utf8_lossy(&buf);
        assert!(
            resp.starts_with("HTTP/1.1 404"),
            "expected 404 after flood, got: {resp:?}"
        );

        drop(stalled);
    }
}
