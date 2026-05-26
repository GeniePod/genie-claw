//! Hardened HTTP/1.1 request reader shared by the GeniePod backend servers.
//!
//! Both `genie-core` (chat/agent API, `:3000`) and `genie-api` (dashboard API,
//! `:3080`) speak a tiny hand-rolled HTTP/1.1 dialect over raw `tokio` sockets
//! — no framework. The original readers grew the request line and headers with
//! an unbounded `read_line` loop and never imposed a read deadline, so a single
//! unauthenticated peer on the LAN could exhaust memory (a header that never
//! terminates with `\n`) or stall the listener with half-open connections and
//! take the always-on home daemon down (issue #195).
//!
//! This module centralizes a bounded, deadline-guarded reader so the fix lives
//! in exactly one place. Callers supply [`HttpLimits`] — typically built from
//! the `[http]` config section plus a per-server body cap — and get back a
//! parsed [`HttpRequest`], or a typed [`HttpReadError`] they can map onto a
//! `431` / `413` response (or simply close the connection).
//!
//! The reader is bounded in three independent ways:
//!   * **Size** — request line, each header line, the total header bytes, and
//!     the header count are all capped; the body is capped too. Memory never
//!     grows past the configured ceilings regardless of what the peer sends.
//!   * **Time** — the entire read (line + headers + body) runs under a single
//!     [`tokio::time::timeout`], so a connection that opens and then stalls is
//!     dropped instead of awaiting forever.
//!   * **Liveness** — a half-sent request that never reaches the blank-line
//!     terminator is reported (`Closed` / `Timeout`) instead of being parsed.
//!
//! Connection-count ceilings and accept-loop resilience are the server's job
//! (a `tokio::sync::Semaphore` plus a log-and-continue accept loop); they are
//! not part of the per-request read and so live in each server's listener.

use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// Bounds enforced while reading one inbound HTTP request.
///
/// Construct with [`HttpLimits::from_config`] to pull the shared knobs from the
/// `[http]` config section, passing the per-server body cap (genie-core keeps
/// its 64 KiB cap, genie-api its 4 KiB cap).
#[derive(Debug, Clone, Copy)]
pub struct HttpLimits {
    /// Max bytes in the request line (`METHOD PATH VERSION\r\n`), newline included.
    pub max_request_line_bytes: usize,
    /// Max bytes in any single header line, newline included.
    pub max_header_line_bytes: usize,
    /// Max number of header lines.
    pub max_header_count: usize,
    /// Max total bytes across all header lines (the header phase ceiling).
    pub max_header_bytes: usize,
    /// Max declared `Content-Length` the server will read into memory.
    pub max_body_bytes: usize,
    /// Deadline for the whole request read (line + headers + body).
    pub read_timeout: Duration,
}

impl HttpLimits {
    /// Build limits from the shared `[http]` config plus a per-server body cap.
    pub fn from_config(cfg: &crate::config::HttpServerConfig, max_body_bytes: usize) -> Self {
        Self {
            max_request_line_bytes: cfg.max_request_line_bytes.max(1),
            max_header_line_bytes: cfg.max_header_line_bytes.max(1),
            max_header_count: cfg.max_header_count,
            max_header_bytes: cfg.max_header_bytes.max(1),
            max_body_bytes,
            read_timeout: Duration::from_secs(cfg.read_timeout_secs.max(1)),
        }
    }
}

/// A parsed inbound HTTP request.
///
/// Header names are stored lowercased; [`HttpRequest::header`] looks them up
/// case-insensitively. The body, if any, has already been read in full (and is
/// therefore bounded by [`HttpLimits::max_body_bytes`]).
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub content_length: usize,
    pub body: Option<String>,
}

impl HttpRequest {
    /// First value for `name`, matched case-insensitively. Returns the raw
    /// (un-lowercased) value so callers see exactly what the peer sent.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Why an inbound request could not be read.
///
/// [`HttpReadError::status_code`] gives the HTTP status to reply with for the
/// cases where a reply is appropriate; the rest mean "just drop the
/// connection" (the peer is gone, stalled, or sending garbage).
#[derive(Debug)]
pub enum HttpReadError {
    /// The whole-request read deadline elapsed (idle / slowloris connection).
    Timeout,
    /// The peer closed before sending a complete request.
    Closed,
    /// The request line had no method/path.
    Malformed,
    /// The request line exceeded `max_request_line_bytes`.
    RequestLineTooLong,
    /// A header line exceeded `max_header_line_bytes`.
    HeaderLineTooLong,
    /// More than `max_header_count` header lines were sent.
    TooManyHeaders,
    /// The header phase exceeded `max_header_bytes` in total.
    HeadersTooLarge,
    /// The declared `Content-Length` exceeded `max_body_bytes`.
    BodyTooLarge,
    /// A low-level I/O error (including a truncated body).
    Io(std::io::Error),
}

impl HttpReadError {
    /// HTTP status to respond with, or `None` when the connection should just
    /// be dropped without a reply.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            HttpReadError::RequestLineTooLong
            | HttpReadError::HeaderLineTooLong
            | HttpReadError::TooManyHeaders
            | HttpReadError::HeadersTooLarge => Some(431),
            HttpReadError::BodyTooLarge => Some(413),
            // A read timeout, a vanished peer, garbage, or a socket error: there
            // is no point (or it is unsafe against a stalled peer) writing a
            // status line, so close the connection instead.
            HttpReadError::Timeout
            | HttpReadError::Closed
            | HttpReadError::Malformed
            | HttpReadError::Io(_) => None,
        }
    }
}

impl std::fmt::Display for HttpReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpReadError::Timeout => write!(f, "request read timed out"),
            HttpReadError::Closed => write!(f, "peer closed before a complete request"),
            HttpReadError::Malformed => write!(f, "malformed request line"),
            HttpReadError::RequestLineTooLong => write!(f, "request line too long"),
            HttpReadError::HeaderLineTooLong => write!(f, "header line too long"),
            HttpReadError::TooManyHeaders => write!(f, "too many headers"),
            HttpReadError::HeadersTooLarge => write!(f, "headers too large"),
            HttpReadError::BodyTooLarge => write!(f, "request body too large"),
            HttpReadError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for HttpReadError {}

/// Read and parse one HTTP request from `reader`, enforcing every bound in
/// `limits`. The entire read runs under a single deadline, so a stalled peer
/// cannot hold the task open.
pub async fn read_request<R>(
    reader: &mut R,
    limits: &HttpLimits,
) -> Result<HttpRequest, HttpReadError>
where
    R: AsyncBufRead + Unpin,
{
    match tokio::time::timeout(limits.read_timeout, read_request_inner(reader, limits)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(HttpReadError::Timeout),
    }
}

async fn read_request_inner<R>(
    reader: &mut R,
    limits: &HttpLimits,
) -> Result<HttpRequest, HttpReadError>
where
    R: AsyncBufRead + Unpin,
{
    // Request line.
    let mut line = Vec::new();
    let n = read_line_bounded(reader, &mut line, limits.max_request_line_bytes, || {
        HttpReadError::RequestLineTooLong
    })
    .await?;
    if n == 0 {
        return Err(HttpReadError::Closed);
    }
    let request_line = String::from_utf8_lossy(&line);
    let mut parts = request_line.split_whitespace();
    let (method, path) = match (parts.next(), parts.next()) {
        (Some(method), Some(path)) => (method.to_string(), path.to_string()),
        _ => return Err(HttpReadError::Malformed),
    };

    // Headers.
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length: usize = 0;
    let mut total_header_bytes: usize = 0;
    loop {
        if headers.len() >= limits.max_header_count {
            return Err(HttpReadError::TooManyHeaders);
        }
        line.clear();
        let n = read_line_bounded(reader, &mut line, limits.max_header_line_bytes, || {
            HttpReadError::HeaderLineTooLong
        })
        .await?;
        if n == 0 {
            // EOF before the blank-line terminator — a truncated request.
            return Err(HttpReadError::Closed);
        }
        total_header_bytes = total_header_bytes.saturating_add(n);
        if total_header_bytes > limits.max_header_bytes {
            return Err(HttpReadError::HeadersTooLarge);
        }

        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
        // Lines without a colon are ignored, matching the previous lenient
        // parser (it only ever looked for specific `name: value` prefixes).
    }

    // Body.
    let body = if content_length > 0 {
        if content_length > limits.max_body_bytes {
            return Err(HttpReadError::BodyTooLarge);
        }
        let mut buf = vec![0u8; content_length];
        reader
            .read_exact(&mut buf)
            .await
            .map_err(HttpReadError::Io)?;
        Some(String::from_utf8_lossy(&buf).to_string())
    } else {
        None
    };

    Ok(HttpRequest {
        method,
        path,
        headers,
        content_length,
        body,
    })
}

/// Read one `\n`-terminated line into `out`, appending at most `max_bytes`.
///
/// Returns the number of bytes appended for this line (0 on immediate EOF).
/// If `max_bytes` is reached before a newline, returns the error built by
/// `too_long` — crucially *without* having accumulated more than `max_bytes`,
/// so a peer streaming an endless line cannot drive memory growth.
///
/// Bytes are pulled out of the buffered reader a chunk at a time via
/// `fill_buf` / `consume` rather than `read_line`, which would append the whole
/// (attacker-controlled) line to a `String` with no ceiling.
async fn read_line_bounded<R, F>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max_bytes: usize,
    too_long: F,
) -> Result<usize, HttpReadError>
where
    R: AsyncBufRead + Unpin,
    F: Fn() -> HttpReadError,
{
    let start = out.len();
    loop {
        let available = reader.fill_buf().await.map_err(HttpReadError::Io)?;
        if available.is_empty() {
            // EOF.
            return Ok(out.len() - start);
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(idx) => {
                let take = idx + 1;
                if (out.len() - start) + take > max_bytes {
                    return Err(too_long());
                }
                out.extend_from_slice(&available[..take]);
                reader.consume(take);
                return Ok(out.len() - start);
            }
            None => {
                let take = available.len();
                if (out.len() - start) + take > max_bytes {
                    // The cap is reached and still no newline in sight: refuse
                    // now instead of buffering an unbounded line.
                    return Err(too_long());
                }
                out.extend_from_slice(available);
                reader.consume(take);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

    fn test_limits() -> HttpLimits {
        HttpLimits {
            max_request_line_bytes: 8 * 1024,
            max_header_line_bytes: 8 * 1024,
            max_header_count: 64,
            max_header_bytes: 64 * 1024,
            max_body_bytes: 64 * 1024,
            read_timeout: Duration::from_secs(5),
        }
    }

    fn reader_for(bytes: &[u8]) -> BufReader<Cursor<Vec<u8>>> {
        BufReader::new(Cursor::new(bytes.to_vec()))
    }

    #[tokio::test]
    async fn parses_simple_get() {
        let mut reader = reader_for(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let req = read_request(&mut reader, &test_limits()).await.unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/api/health");
        assert_eq!(req.header("host"), Some("localhost"));
        assert!(req.body.is_none());
    }

    #[tokio::test]
    async fn parses_post_with_body_and_origin_header() {
        let body = r#"{"message":"hi"}"#;
        let raw = format!(
            "POST /api/chat HTTP/1.1\r\nContent-Length: {}\r\nX-Genie-Origin: Voice\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = reader_for(raw.as_bytes());
        let req = read_request(&mut reader, &test_limits()).await.unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.content_length, body.len());
        assert_eq!(req.body.as_deref(), Some(body));
        // Header name matched case-insensitively; raw value preserved.
        assert_eq!(req.header("x-genie-origin"), Some("Voice"));
    }

    #[tokio::test]
    async fn oversized_request_line_is_rejected() {
        let mut limits = test_limits();
        limits.max_request_line_bytes = 64;
        let raw = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(4096));
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::RequestLineTooLong));
        assert_eq!(err.status_code(), Some(431));
    }

    #[tokio::test]
    async fn unterminated_header_is_rejected_in_bounded_memory() {
        // A header that never ends with `\n` is the OOM vector from issue #195:
        // chain a valid request line with an infinite stream of 'A'. The reader
        // must reject it, not grow without limit.
        let mut limits = test_limits();
        limits.max_header_line_bytes = 4096;
        let prefix = Cursor::new(b"GET / HTTP/1.1\r\nX-Pad: ".to_vec());
        let endless = prefix.chain(tokio::io::repeat(b'A'));
        let mut reader = BufReader::new(endless);
        let err = read_request(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::HeaderLineTooLong));
        assert_eq!(err.status_code(), Some(431));
    }

    #[tokio::test]
    async fn too_many_headers_is_rejected() {
        let mut limits = test_limits();
        limits.max_header_count = 4;
        let mut raw = String::from("GET / HTTP/1.1\r\n");
        for i in 0..50 {
            raw.push_str(&format!("X-H-{i}: v\r\n"));
        }
        raw.push_str("\r\n");
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::TooManyHeaders));
        assert_eq!(err.status_code(), Some(431));
    }

    #[tokio::test]
    async fn total_header_bytes_cap_is_enforced() {
        let mut limits = test_limits();
        limits.max_header_bytes = 256;
        limits.max_header_line_bytes = 8 * 1024;
        let mut raw = String::from("GET / HTTP/1.1\r\n");
        for i in 0..40 {
            raw.push_str(&format!("X-Filler-{i}: {}\r\n", "x".repeat(40)));
        }
        raw.push_str("\r\n");
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::HeadersTooLarge));
        assert_eq!(err.status_code(), Some(431));
    }

    #[tokio::test]
    async fn oversized_declared_body_is_rejected() {
        let mut limits = test_limits();
        limits.max_body_bytes = 4096;
        let raw = "POST /api/chat HTTP/1.1\r\nContent-Length: 999999\r\n\r\n";
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::BodyTooLarge));
        assert_eq!(err.status_code(), Some(413));
    }

    #[tokio::test]
    async fn idle_connection_times_out() {
        // A peer that sends a partial request and then stalls (never sending
        // the header terminator) must be dropped after the read deadline, not
        // awaited forever.
        let mut limits = test_limits();
        limits.read_timeout = Duration::from_millis(150);

        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"GET / HTTP/1.1\r\nX-Partial: ")
            .await
            .unwrap();
        // Keep `client` alive (no EOF) but send nothing more.

        let mut reader = BufReader::new(server);
        let start = std::time::Instant::now();
        let err = read_request(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::Timeout));
        assert_eq!(err.status_code(), None, "timeout should just drop the conn");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "read must abandon the stalled peer promptly"
        );
        drop(client);
    }

    #[tokio::test]
    async fn peer_close_before_request_is_reported() {
        let mut reader = reader_for(b"");
        let err = read_request(&mut reader, &test_limits()).await.unwrap_err();
        assert!(matches!(err, HttpReadError::Closed));
        assert_eq!(err.status_code(), None);
    }
}
