use anyhow::Result;
use genie_common::config::Config;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::routes;

/// Minimal HTTP/1.1 server — no framework, no allocator overhead.
///
/// Handles one request per connection (Connection: close).
/// This is intentional: the dashboard polls every 5 seconds,
/// and the API serves <10 concurrent clients on a home appliance.
pub async fn serve(addr: &str, config: Config) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let config = std::sync::Arc::new(config);

    tracing::info!(addr, "listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        let config = config.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &config).await {
                tracing::debug!(peer = %peer, error = %e, "connection error");
            }
        });
    }
}

async fn handle_connection(stream: tokio::net::TcpStream, config: &Config) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // Read the request line.
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    // Parse method and path.
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    // Drain headers (we don't need them for our simple API).
    let mut header_line = String::new();
    let mut content_length: usize = 0;
    loop {
        header_line.clear();
        buf_reader.read_line(&mut header_line).await?;
        if header_line.trim().is_empty() {
            break;
        }
        if let Some(val) = header_line.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    // Read body if present.
    let body = if content_length > 0 && content_length < 4096 {
        let mut buf = vec![0u8; content_length];
        tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut buf).await?;
        Some(String::from_utf8_lossy(&buf).to_string())
    } else {
        None
    };

    // Route the request.
    let response = match (method, path) {
        ("GET", "/api/status") => routes::get_status(config).await,
        ("GET", "/api/tegrastats") => routes::get_tegrastats(config).await,
        ("GET", "/api/services") => routes::get_services(config).await,
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
