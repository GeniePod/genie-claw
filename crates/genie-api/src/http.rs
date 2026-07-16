use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use genie_common::config::Config;
use genie_common::http::{
    GuardRejection, HttpLimits, OriginDecision, RequestGuard, RoutePolicySurface,
    cors_response_headers, read_request, route_requires_local_token,
};
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

    // Cross-origin / DNS-rebinding gate (issue #228), built from the actual
    // bound address so loopback Host/Origin values for the dashboard port are
    // always accepted; the wildcard ACAO is gone.
    let local_addr = listener.local_addr().ok();
    let listen_host = local_addr
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let listen_port = local_addr.map(|addr| addr.port()).unwrap_or(0);
    let guard = Arc::new(RequestGuard::new(
        &listen_host,
        listen_port,
        &config.http.allowed_origins,
        &config.http.allowed_hosts,
        &config.http.local_api_token,
    ));
    if guard.enforces_token() {
        tracing::info!("local API token enforced on mutating dashboard endpoints");
    } else {
        tracing::warn!(
            "no [http].local_api_token set; mutating endpoints rely on the Origin/Host gate only"
        );
    }

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
        let guard = Arc::clone(&guard);

        tokio::spawn(async move {
            // Hold the permit for the lifetime of the request.
            let _permit = permit;
            if let Err(e) = handle_connection(stream, &config, &limits, &guard).await {
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
    guard: &RequestGuard,
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
                let _ = write_response(&mut writer, &error_response(status), None).await;
            }
            tracing::debug!(error = %e, "rejected request");
            return Ok(());
        }
    };

    // Cross-origin / DNS-rebinding gate ahead of routing (issue #228).
    let echo_origin = match guard.check_request(&request) {
        OriginDecision::Allow(origin) => origin,
        OriginDecision::Reject(rejection) => {
            tracing::debug!(reason = rejection.reason(), "request gated out");
            let _ = write_response(&mut writer, &guard_rejection(rejection), None).await;
            return Ok(());
        }
    };

    let method = request.method.as_str();
    let path = request.path.as_str();

    // Mutating/actuating endpoints additionally require the shared local token.
    if route_requires_local_token(RoutePolicySurface::GenieApi, method, path)
        && !guard.token_ok(&request)
    {
        tracing::debug!("mutating request without a valid local API token");
        let response = guard_rejection(GuardRejection::MissingToken);
        return write_response(&mut writer, &response, echo_origin.as_deref()).await;
    }

    let body = request.body.as_deref();

    // Route the request.
    let response = match (method, path) {
        ("OPTIONS", _) => Response {
            status: 204,
            content_type: "text/plain",
            body: String::new(),
        },
        ("GET", "/api/status") => routes::get_status(config).await,
        ("GET", "/api/tegrastats") => routes::get_tegrastats(config).await,
        ("GET", "/api/services") => routes::get_services(config).await,
        ("GET", "/api/security") => routes::get_security(config).await,
        ("GET", "/api/runtime/contract") => routes::get_runtime_contract(config).await,
        ("GET", "/api/actuation/pending") => routes::get_actuation_pending(config).await,
        ("GET", "/api/actuation/actions") => routes::get_actuation_actions(config).await,
        ("GET", "/api/actuation/audit") => routes::get_actuation_audit(config).await,
        ("POST", "/api/actuation/confirm") => routes::post_actuation_confirm(config, body).await,
        ("GET", "/api/memories") => routes::get_memories(config).await,
        ("POST", "/api/memories/update") => routes::post_memory_update(config, body).await,
        ("POST", "/api/memories/delete") => routes::post_memory_delete(config, body).await,
        ("POST", "/api/memories/reorder") => routes::post_memory_reorder(config, body).await,
        ("POST", "/api/mode") => routes::post_mode(body).await,
        ("GET", "/" | "/index.html") => routes::serve_dashboard(&config.http.local_api_token),
        ("GET", "/dashboard.js") => routes::serve_dashboard_js(),
        _ => Response {
            status: 404,
            content_type: "application/json",
            body: r#"{"error":"not found"}"#.into(),
        },
    };

    write_response(&mut writer, &response, echo_origin.as_deref()).await
}

/// `403` response for a gated-out request, reusing the shared rejection reason.
fn guard_rejection(rejection: GuardRejection) -> Response {
    Response {
        status: rejection.status(),
        content_type: "application/json",
        body: format!(r#"{{"error":"{}"}}"#, rejection.reason()),
    }
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

async fn write_response(
    writer: &mut OwnedWriteHalf,
    response: &Response,
    reflect_origin: Option<&str>,
) -> Result<()> {
    let http_response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
        response.status,
        status_text(response.status),
        response.content_type,
        response.body.len(),
        cors_response_headers(reflect_origin),
    );

    writer.write_all(http_response.as_bytes()).await?;
    writer.write_all(response.body.as_bytes()).await?;
    writer.flush().await?;

    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
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

    // `get_tegrastats` returns 503 on a transient SQLite busy/locked error
    // (classify_sqlite_error, added in #299). `write_response` builds the
    // status line as `HTTP/1.1 {code} {status_text(code)}`, so the reason
    // phrase table must name 503 instead of falling through to "Unknown".
    #[test]
    fn status_text_maps_503_to_service_unavailable() {
        assert_eq!(super::status_text(503), "Service Unavailable");
    }

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
            .write_all(b"GET /does-not-exist HTTP/1.1\r\nHost: localhost\r\n\r\n")
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
            .write_all(b"GET /does-not-exist HTTP/1.1\r\nHost: localhost\r\n\r\n")
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

    // --- Cross-origin request gate (issue #228) ---------------------------

    async fn roundtrip(port: u16, raw: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        read_all(stream, Duration::from_secs(5)).await
    }

    #[tokio::test]
    async fn cross_origin_is_gated_and_wildcard_is_gone() {
        let config = config_with_http("[http]\nread_timeout_secs = 2\nmax_connections = 8\n");
        let port = start_server(config).await;

        // Same-origin read: 200 and never a wildcard ACAO.
        let ok = roundtrip(port, "GET /api/status HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok:?}");
        assert!(
            !ok.contains("Access-Control-Allow-Origin: *"),
            "wildcard ACAO must be gone: {ok:?}"
        );

        // Cross-site Origin → 403, not made readable.
        let evil = roundtrip(
            port,
            "GET /api/status HTTP/1.1\r\nHost: localhost\r\nOrigin: http://evil.example\r\n\r\n",
        )
        .await;
        assert!(evil.starts_with("HTTP/1.1 403"), "{evil:?}");
        assert!(!evil.contains("Access-Control-Allow-Origin"), "{evil:?}");

        // DNS-rebinding: an attacker Host → 403.
        let rebind = roundtrip(
            port,
            &format!("GET /api/status HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n"),
        )
        .await;
        assert!(rebind.starts_with("HTTP/1.1 403"), "{rebind:?}");
    }

    #[tokio::test]
    async fn mutating_dashboard_endpoint_requires_token_when_configured() {
        let config = config_with_http(
            "[http]\nlocal_api_token = \"s3cret\"\nread_timeout_secs = 2\nmax_connections = 8\n",
        );
        let port = start_server(config).await;

        // Mutating route without the token → 403 (rejected before any proxy).
        let no_tok = roundtrip(
            port,
            "POST /api/mode HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        assert!(no_tok.starts_with("HTTP/1.1 403"), "{no_tok:?}");
        assert!(no_tok.contains("local API token"), "{no_tok:?}");

        // Sensitive actuation reads are token-gated.
        for path in [
            "/api/actuation/pending",
            "/api/actuation/actions",
            "/api/actuation/audit",
            "/api/memories",
        ] {
            let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let resp = roundtrip(port, &request).await;
            assert!(
                resp.starts_with("HTTP/1.1 403"),
                "path {path} response: {resp:?}"
            );
        }
        // Sensitive actuation read without the token is also rejected.
        let actuation_no_tok = roundtrip(
            port,
            "GET /api/actuation/audit HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(
            actuation_no_tok.starts_with("HTTP/1.1 403"),
            "{actuation_no_tok:?}"
        );

        // The served dashboard carries the injected token.
        let root = roundtrip(port, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(
            root.contains(r#"content="s3cret""#),
            "token must be injected into the dashboard: {root:?}"
        );
    }
}
