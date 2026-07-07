use anyhow::{Context, Result};
use genie_common::http::{HttpReadError, HttpResponseLimits, read_response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// TCP-connect cap. Same value the file had hard-coded since scaffolding;
/// surfaced as a named constant so tests can assert against it.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Full-request cap covering write + status-line + headers + body. Without
/// this, a Home Assistant that pauses mid-response (Python GC, integration
/// reload, Supervisor self-update, stalled add-on) leaves the dispatching
/// chat / voice / dashboard task hung forever — there was no timeout at all
/// on the read side before this constant existed.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard upper bound on a single response body. Prevents the read-to-EOF
/// fallback (no `Content-Length`, no `Transfer-Encoding: chunked`) from
/// accumulating unbounded bytes in RSS, and caps the cumulative payload of
/// a malformed chunked stream. 8 MiB comfortably fits the largest realistic
/// `/api/states` dump for a household.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Home Assistant REST API client.
///
/// Uses the local or LAN Home Assistant HTTP API for:
/// - state reads
/// - service calls
/// - lightweight template rendering for area discovery
#[derive(Debug)]
pub struct HaClient {
    host: String,
    port: u16,
    token: Zeroizing<String>,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_response_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_id: String,
    pub state: String,
    #[serde(default)]
    pub attributes: serde_json::Value,
}

impl Entity {
    /// Friendly name from attributes, or entity_id as fallback.
    pub fn friendly_name(&self) -> &str {
        self.attributes
            .get("friendly_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.entity_id)
    }
}

#[derive(Debug)]
struct HttpResponse {
    status_code: u16,
    body: String,
}

impl HaClient {
    pub fn new(host: &str, port: u16, token: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            token: Zeroizing::new(token.to_string()),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    /// Build from a configured Home Assistant HTTP URL such as
    /// `http://127.0.0.1:8123/api/` or `http://homeassistant.local:8123/`.
    pub fn from_url(url: &str, token: &str) -> Result<Self> {
        let (host, port) = parse_http_url(url)?;
        Ok(Self::new(&host, port, token))
    }

    /// Override the default timeouts and the max response size. Intended for
    /// `mod tests`, which need to assert that a hung server is surfaced as
    /// an `Err` in millisecond-scale rather than waiting for the production
    /// 30 s deadline. Production callers should leave the defaults alone.
    #[cfg(test)]
    pub(crate) fn with_test_limits(
        mut self,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Self {
        self.connect_timeout = connect_timeout;
        self.request_timeout = request_timeout;
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Simple connectivity check.
    pub async fn test_connection(&self) -> Result<()> {
        self.http_get("/api/").await.map(|_| ())
    }

    /// Get all entity states.
    pub async fn get_states(&self) -> Result<Vec<Entity>> {
        let body = self.http_get("/api/states").await?;
        let entities: Vec<Entity> = serde_json::from_str(&body)?;
        Ok(entities)
    }

    /// Get a single entity state.
    pub async fn get_state(&self, entity_id: &str) -> Result<Entity> {
        let path = format!("/api/states/{}", entity_id);
        let body = self.http_get(&path).await?;
        let entity: Entity = serde_json::from_str(&body)?;
        Ok(entity)
    }

    /// Call a Home Assistant service (e.g., `light.turn_on`).
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        data: &serde_json::Value,
    ) -> Result<Vec<Entity>> {
        let path = format!("/api/services/{}/{}", domain, service);
        let body = serde_json::to_string(data)?;
        let body = self.http_post_json(&path, &body).await?;

        if body.trim().is_empty() {
            return Ok(Vec::new());
        }

        serde_json::from_str(&body).or_else(|_| Ok(Vec::new()))
    }

    /// Render a Home Assistant template and return its plain-text output.
    pub async fn render_template(&self, template: &str) -> Result<String> {
        let body = serde_json::json!({ "template": template }).to_string();
        self.http_post_json("/api/template", &body).await
    }

    /// Render a template expected to serialize JSON and decode it to `T`.
    pub async fn render_template_json<T: DeserializeOwned>(&self, template: &str) -> Result<T> {
        let body = self.render_template(template).await?;
        serde_json::from_str(body.trim())
            .with_context(|| format!("failed to decode Home Assistant template output: {}", body))
    }

    async fn http_get(&self, path: &str) -> Result<String> {
        let response = self.http_request("GET", path, None, None).await?;
        Ok(response.body)
    }

    async fn http_post_json(&self, path: &str, body: &str) -> Result<String> {
        let response = self
            .http_request("POST", path, Some("application/json"), Some(body))
            .await?;
        Ok(response.body)
    }

    async fn http_request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Option<&str>,
    ) -> Result<HttpResponse> {
        let addr = format!("{}:{}", self.host, self.port);
        let connect_timeout = self.connect_timeout;
        let request_timeout = self.request_timeout;
        let max_response_bytes = self.max_response_bytes;

        let stream = match tokio::time::timeout(connect_timeout, TcpStream::connect(&addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                return Err(e)
                    .with_context(|| format!("Home Assistant connect {} {} failed", method, path));
            }
            Err(_) => {
                anyhow::bail!(
                    "Home Assistant connect {} {} timed out after {:?}",
                    method,
                    path,
                    connect_timeout
                );
            }
        };

        let (reader, mut writer) = stream.into_split();

        // Build the request in two parts around the token so it is never
        // embedded in a single heap allocation alongside the rest of the
        // headers. The token is written as a separate `write_all` from a
        // Zeroizing<String> that is wiped when the async block returns.
        let head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer ",);
        let tail = if let Some(body) = body {
            format!(
                "\r\nContent-Type: {ct}\r\nContent-Length: {cl}\r\nConnection: close\r\n\r\n{body}",
                ct = content_type.unwrap_or("application/json"),
                cl = body.len(),
                body = body
            )
        } else {
            "\r\nConnection: close\r\n\r\n".to_string()
        };
        let token = self.token.clone();

        // Enforce a single deadline that covers write + status-line + headers
        // + body. Pre-fix only the `connect` step had a timeout; a hung HA
        // (Python GC pause, integration reload, Supervisor self-update,
        // stalled add-on) blocked the calling task indefinitely.
        let response = match tokio::time::timeout(request_timeout, async move {
            writer.write_all(head.as_bytes()).await?;
            writer.write_all(token.as_bytes()).await?;
            writer.write_all(tail.as_bytes()).await?;
            read_http_response(reader, max_response_bytes).await
        })
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!(
                "Home Assistant {} {} timed out after {:?}",
                method,
                path,
                request_timeout
            ),
        };

        if !(200..300).contains(&response.status_code) {
            let body = response.body.trim().replace('\n', " ");
            anyhow::bail!(
                "Home Assistant HTTP {} for {} {}{}",
                response.status_code,
                method,
                path,
                if body.is_empty() {
                    String::new()
                } else {
                    format!(": {}", body)
                }
            );
        }

        Ok(response)
    }
}

async fn read_http_response(
    reader: tokio::net::tcp::OwnedReadHalf,
    max_response_bytes: usize,
) -> Result<HttpResponse> {
    let mut buf_reader = BufReader::new(reader);
    let limits = HttpResponseLimits::with_body_cap(max_response_bytes);
    let response = read_response(&mut buf_reader, &limits)
        .await
        .map_err(|e| match e {
            HttpReadError::BodyTooLarge => {
                anyhow::anyhow!("Home Assistant response exceeded body cap")
            }
            other => anyhow::anyhow!("{other}"),
        })?;
    if response.status == 0 {
        anyhow::bail!("invalid HTTP response status line");
    }
    Ok(HttpResponse {
        status_code: response.status,
        body: response.body,
    })
}

fn parse_http_url(url: &str) -> Result<(String, u16)> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported Home Assistant URL '{}': only http:// is supported",
            url
        )
    })?;

    let authority = rest
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid Home Assistant URL '{}'", url))?;

    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| anyhow::anyhow!("invalid IPv6 Home Assistant URL '{}'", url))?;
        let host = authority[1..end].to_string();
        let port = authority[end + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8123);
        return Ok((host, port));
    }

    if let Some((host, port)) = authority.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return Ok((host.to_string(), port));
    }

    Ok((authority.to_string(), 8123))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_url_with_api_path() {
        let (host, port) = parse_http_url("http://127.0.0.1:8123/api/").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 8123);
    }

    #[test]
    fn parse_http_url_defaults_port() {
        let (host, port) = parse_http_url("http://homeassistant.local/").unwrap();
        assert_eq!(host, "homeassistant.local");
        assert_eq!(port, 8123);
    }

    #[test]
    fn parse_http_url_rejects_https() {
        let err = parse_http_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("only http:// is supported"));
    }

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    /// Spawn a `TcpListener` on a free local port, run `handler` on each
    /// accepted connection. Returns the bound address.
    async fn spawn_listener<F, Fut>(handler: F) -> std::net::SocketAddr
    where
        F: Fn(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                tokio::spawn(handler(conn));
            }
        });
        addr
    }

    fn test_client(addr: std::net::SocketAddr) -> HaClient {
        HaClient::new(&addr.ip().to_string(), addr.port(), "test-token").with_test_limits(
            Duration::from_millis(200),
            Duration::from_millis(300),
            16 * 1024,
        )
    }

    /// Server that accepts the connection and then never writes a response —
    /// mimics Home Assistant after a `kill -STOP`, mid-GC pause, or stalled
    /// integration. Pre-fix, the calling task hangs forever; with the fix
    /// it surfaces as a clean `Err("…timed out…")` within the request budget.
    #[tokio::test]
    async fn hung_server_after_connect_times_out_cleanly() {
        let addr = spawn_listener(|mut conn| async move {
            // Drain the request so the client's `write_all` doesn't stall on
            // a full receive buffer, but never write a response.
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
        .await;

        let client = test_client(addr);
        let start = std::time::Instant::now();
        let err = client.test_connection().await.unwrap_err().to_string();
        let elapsed = start.elapsed();

        assert!(
            err.contains("timed out"),
            "expected a timeout error, got: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must surface inside the request budget, elapsed {elapsed:?}"
        );
    }

    /// Server that accepts, waits briefly (< budget), then sends a valid
    /// minimal HTTP/1.1 200. Confirms healthy HA still works after the fix.
    #[tokio::test]
    async fn slow_server_within_budget_succeeds() {
        let addr = spawn_listener(|mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let body = "[]";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.shutdown().await;
        })
        .await;

        let client = test_client(addr);
        let entities = client.get_states().await.unwrap();
        assert!(entities.is_empty());
    }

    /// Server that advertises `Content-Length` larger than `max_response_bytes`.
    /// Pre-fix the client would allocate that much memory; with the fix it
    /// rejects up front with a clear "exceeds cap" error.
    #[tokio::test]
    async fn oversize_content_length_is_rejected() {
        let addr = spawn_listener(|mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await;
            // Advertise 1 MiB but the test cap is 16 KiB.
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n";
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.shutdown().await;
        })
        .await;

        let client = test_client(addr);
        let err = client.get_states().await.unwrap_err().to_string();
        assert!(
            err.contains("exceeds cap") || err.contains("exceeded"),
            "expected size-cap error, got: {err}"
        );
    }

    /// Server that streams data without `Content-Length` and without chunked
    /// encoding (the read-to-EOF fallback path) until the client closes.
    /// Pre-fix this read unbounded into RSS; with the fix it stops at the
    /// `max_response_bytes` cap and surfaces a clean error.
    #[tokio::test]
    async fn read_to_eof_response_is_size_capped() {
        let addr = spawn_listener(|mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await;
            // No Content-Length, no chunked. Stream lots of bytes.
            let header =
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
            let _ = conn.write_all(header.as_bytes()).await;
            let filler = vec![b'x'; 4096];
            for _ in 0..16 {
                if conn.write_all(&filler).await.is_err() {
                    break;
                }
            }
            let _ = conn.shutdown().await;
        })
        .await;

        let client = test_client(addr);
        let err = client.get_states().await.unwrap_err().to_string();
        assert!(
            err.contains("exceeded") || err.contains("without Content-Length"),
            "expected size-cap error, got: {err}"
        );
    }

    /// Server that uses `Transfer-Encoding: chunked` and emits more aggregate
    /// payload than `max_response_bytes`. The chunked decoder must bail before
    /// allocating it all.
    #[tokio::test]
    async fn chunked_body_aggregate_size_is_capped() {
        let addr = spawn_listener(|mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await;
            let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            let _ = conn.write_all(header.as_bytes()).await;
            // 8 chunks of 4 KiB = 32 KiB, exceeds the 16 KiB test cap.
            let chunk_payload = vec![b'y'; 4096];
            for _ in 0..8 {
                let chunk_header = format!("{:x}\r\n", chunk_payload.len());
                if conn.write_all(chunk_header.as_bytes()).await.is_err() {
                    break;
                }
                if conn.write_all(&chunk_payload).await.is_err() {
                    break;
                }
                if conn.write_all(b"\r\n").await.is_err() {
                    break;
                }
            }
            let _ = conn.write_all(b"0\r\n\r\n").await;
            let _ = conn.shutdown().await;
        })
        .await;

        let client = test_client(addr);
        let err = client.get_states().await.unwrap_err().to_string();
        assert!(
            err.contains("chunked response exceeded") || err.contains("exceeded"),
            "expected chunked-cap error, got: {err}"
        );
    }
}
