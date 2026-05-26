use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use genie_common::config::Config;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::http_safety::{
    BODY_READ_TIMEOUT, HEADER_READ_TIMEOUT, LineRead, MAX_CONCURRENT_CONNECTIONS,
    MAX_HEADER_LINE_BYTES, MAX_REQUEST_LINE_BYTES, MAX_TOTAL_HEADER_BYTES, build_error_response,
    read_capped_line,
};
use crate::routes;

/// Minimal HTTP/1.1 server — no framework, no allocator overhead.
///
/// Handles one request per connection (Connection: close).
/// This is intentional: the dashboard polls every 5 seconds,
/// and the API serves <10 concurrent clients on a home appliance.
///
/// The parser is bounded along three axes — see [`crate::http_safety`] for
/// the constants — so an unauthenticated peer cannot exhaust memory, fds, or
/// crash the daemon by sending malformed or stalled requests.
pub async fn serve(addr: &str, config: Config) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let config = std::sync::Arc::new(config);
    let connection_permits = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    tracing::info!(addr, "listening");

    loop {
        let permit = match Arc::clone(&connection_permits).acquire_owned().await {
            Ok(p) => p,
            Err(_) => break Ok(()),
        };

        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                // EMFILE / other transient accept failures must NOT terminate
                // the daemon. Drop the permit, back off, and try again.
                tracing::warn!(error = %e, "accept failed; backing off before retrying");
                drop(permit);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let config = config.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection(stream, &config).await {
                tracing::debug!(peer = %peer, error = %e, "connection error");
            }
        });
    }
}

async fn handle_connection(stream: tokio::net::TcpStream, config: &Config) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // Read the request line under a hard byte cap and timeout. An unbounded
    // read_line() would let any peer pin this task and grow the buffer
    // without limit.
    let mut request_line = String::new();
    match read_capped_line(
        &mut buf_reader,
        &mut request_line,
        MAX_REQUEST_LINE_BYTES,
        HEADER_READ_TIMEOUT,
    )
    .await?
    {
        LineRead::Ok => {}
        LineRead::TooLong => {
            let _ = writer
                .write_all(build_error_response(414, "request line too long").as_bytes())
                .await;
            return Ok(());
        }
        LineRead::Timeout => {
            let _ = writer
                .write_all(build_error_response(408, "request timed out").as_bytes())
                .await;
            return Ok(());
        }
        LineRead::Eof => return Ok(()),
    }

    // Parse method and path.
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    // Drain headers under per-line and total caps. We don't need the values
    // for most endpoints, but the parser must still bound allocation.
    let mut content_length: usize = 0;
    let mut total_header_bytes = request_line.len();
    let mut header_line = String::new();
    loop {
        header_line.clear();
        let outcome = read_capped_line(
            &mut buf_reader,
            &mut header_line,
            MAX_HEADER_LINE_BYTES,
            HEADER_READ_TIMEOUT,
        )
        .await?;
        match outcome {
            LineRead::Ok => {}
            LineRead::TooLong => {
                let _ = writer
                    .write_all(build_error_response(431, "header line too long").as_bytes())
                    .await;
                return Ok(());
            }
            LineRead::Timeout => {
                let _ = writer
                    .write_all(build_error_response(408, "request timed out").as_bytes())
                    .await;
                return Ok(());
            }
            LineRead::Eof => return Ok(()),
        }

        total_header_bytes = total_header_bytes.saturating_add(header_line.len());
        if total_header_bytes > MAX_TOTAL_HEADER_BYTES {
            let _ = writer
                .write_all(build_error_response(431, "headers too large").as_bytes())
                .await;
            return Ok(());
        }

        if header_line.trim().is_empty() {
            break;
        }
        if let Some(val) = header_line.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    // Read body if present, under its own deadline.
    const MAX_BODY_BYTES: usize = 4096;
    if content_length > MAX_BODY_BYTES {
        let _ = writer
            .write_all(build_error_response(413, "request body too large").as_bytes())
            .await;
        return Ok(());
    }
    let body = if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        let read_res = tokio::time::timeout(
            BODY_READ_TIMEOUT,
            tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut buf),
        )
        .await;
        match read_res {
            Ok(Ok(_)) => Some(String::from_utf8_lossy(&buf).to_string()),
            Ok(Err(e)) => return Err(e.into()),
            Err(_elapsed) => {
                let _ = writer
                    .write_all(build_error_response(408, "request body timed out").as_bytes())
                    .await;
                return Ok(());
            }
        }
    } else {
        None
    };

    // Route the request.
    let response = match (method, path) {
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

    // Write HTTP response.
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

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}
