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

/// Which HTTP server is asking for route-token policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePolicySurface {
    GenieCore,
    GenieApi,
}

/// Shared route-token policy for local HTTP servers.
///
/// Returns true when `method`+`path` must present a valid local API token when
/// one is configured. This is centralized here so `genie-core` and `genie-api`
/// cannot silently drift on sensitive route classification.
pub fn route_requires_local_token(surface: RoutePolicySurface, method: &str, path: &str) -> bool {
    match (method, path) {
        // Sensitive actuation routes.
        ("POST", "/api/actuation/confirm")
        | ("GET", "/api/actuation/pending")
        | ("GET", "/api/actuation/actions") => true,
        ("GET", "/api/actuation/audit") => matches!(surface, RoutePolicySurface::GenieApi),

        // Other mutating routes.
        ("POST", "/api/memories/update")
        | ("POST", "/api/memories/delete")
        | ("POST", "/api/memories/reorder")
        | ("POST", "/api/mode")
        | ("POST", "/api/chat/stream")
        | ("POST", "/api/chat")
        | ("POST", "/api/chat/clear")
        | ("POST", "/api/web-search")
        | ("POST", "/v1/chat/completions") => true,

        _ => false,
    }
}

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
    /// The response status line exceeded `max_status_line_bytes`.
    StatusLineTooLong,
    /// A header line exceeded `max_header_line_bytes`.
    HeaderLineTooLong,
    /// More than `max_header_count` header lines were sent.
    TooManyHeaders,
    /// The header phase exceeded `max_header_bytes` in total.
    HeadersTooLarge,
    /// The declared `Content-Length` exceeded `max_body_bytes`.
    BodyTooLarge,
    /// `Content-Length` was malformed, duplicated, or otherwise invalid.
    InvalidContentLength,
    /// Inbound request used unsupported `Transfer-Encoding`.
    UnsupportedTransferEncoding,
    /// A low-level I/O error (including a truncated body).
    Io(std::io::Error),
}

impl HttpReadError {
    /// HTTP status to respond with, or `None` when the connection should just
    /// be dropped without a reply.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            HttpReadError::RequestLineTooLong
            | HttpReadError::StatusLineTooLong
            | HttpReadError::HeaderLineTooLong
            | HttpReadError::TooManyHeaders
            | HttpReadError::HeadersTooLarge => Some(431),
            HttpReadError::BodyTooLarge => Some(413),
            HttpReadError::InvalidContentLength | HttpReadError::UnsupportedTransferEncoding => {
                Some(400)
            }
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
            HttpReadError::StatusLineTooLong => write!(f, "response status line too long"),
            HttpReadError::HeaderLineTooLong => write!(f, "header line too long"),
            HttpReadError::TooManyHeaders => write!(f, "too many headers"),
            HttpReadError::HeadersTooLarge => write!(f, "headers too large"),
            HttpReadError::BodyTooLarge => write!(f, "request body too large"),
            HttpReadError::InvalidContentLength => write!(f, "invalid content-length"),
            HttpReadError::UnsupportedTransferEncoding => {
                write!(f, "unsupported transfer-encoding")
            }
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
    let mut saw_content_length = false;
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
                if saw_content_length {
                    return Err(HttpReadError::InvalidContentLength);
                }
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| HttpReadError::InvalidContentLength)?;
                content_length = parsed;
                saw_content_length = true;
            } else if name == "transfer-encoding" && !value.is_empty() {
                // Inbound chunked/TE requests are not supported by this reader.
                return Err(HttpReadError::UnsupportedTransferEncoding);
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

/// Bounds enforced while reading one outbound HTTP/1.1 response.
///
/// Header ceilings mirror the inbound [`HttpLimits`] defaults so a peer that
/// streams an endless status line or header cannot OOM an outbound client the
/// way issue #195 fixed for inbound servers.
#[derive(Debug, Clone, Copy)]
pub struct HttpResponseLimits {
    /// Max bytes in the status line (`HTTP/1.1 200 OK\r\n`), newline included.
    pub max_status_line_bytes: usize,
    /// Max bytes in any single header line, newline included.
    pub max_header_line_bytes: usize,
    /// Max number of header lines.
    pub max_header_count: usize,
    /// Max total bytes across all header lines.
    pub max_header_bytes: usize,
    /// Max decoded response body bytes.
    pub max_body_bytes: usize,
}

impl HttpResponseLimits {
    /// Build limits with shared header ceilings and a caller-supplied body cap.
    pub fn with_body_cap(max_body_bytes: usize) -> Self {
        // Mirror the `[http]` defaults from `HttpServerConfig` / issue #195 ceilings.
        Self {
            max_status_line_bytes: 8 * 1024,
            max_header_line_bytes: 8 * 1024,
            max_header_count: 100,
            max_header_bytes: 64 * 1024,
            max_body_bytes,
        }
    }
}

/// A parsed outbound HTTP response (status + body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Read and parse one HTTP response from `reader`, enforcing every bound in
/// `limits`. Unlike [`read_request`], chunked `Transfer-Encoding` is supported
/// because real backends (LLM servers, Home Assistant) use it.
pub async fn read_response<R>(
    reader: &mut R,
    limits: &HttpResponseLimits,
) -> Result<HttpResponse, HttpReadError>
where
    R: AsyncBufRead + Unpin,
{
    read_response_inner(reader, limits).await
}

async fn read_response_inner<R>(
    reader: &mut R,
    limits: &HttpResponseLimits,
) -> Result<HttpResponse, HttpReadError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    let n = read_line_bounded(reader, &mut line, limits.max_status_line_bytes, || {
        HttpReadError::StatusLineTooLong
    })
    .await?;
    if n == 0 {
        return Err(HttpReadError::Closed);
    }
    let status_line = String::from_utf8_lossy(&line);
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut header_count = 0usize;
    let mut total_header_bytes = 0usize;

    loop {
        if header_count >= limits.max_header_count {
            return Err(HttpReadError::TooManyHeaders);
        }
        line.clear();
        let n = read_line_bounded(reader, &mut line, limits.max_header_line_bytes, || {
            HttpReadError::HeaderLineTooLong
        })
        .await?;
        if n == 0 {
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
        header_count += 1;

        let lower = trimmed.to_ascii_lowercase();
        if let Some(val) = lower.strip_prefix("content-length:") {
            content_length = val.trim().parse().ok();
        }
        if let Some(val) = lower.strip_prefix("transfer-encoding:")
            && val.contains("chunked")
        {
            chunked = true;
        }
    }

    let body = if chunked {
        read_chunked_response_body(reader, limits).await?
    } else if let Some(content_length) = content_length {
        if content_length > limits.max_body_bytes {
            return Err(HttpReadError::BodyTooLarge);
        }
        let mut buf = vec![0u8; content_length];
        reader
            .read_exact(&mut buf)
            .await
            .map_err(HttpReadError::Io)?;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        let cap = limits.max_body_bytes;
        let mut limited = reader.take(cap as u64 + 1);
        let mut body = String::new();
        limited
            .read_to_string(&mut body)
            .await
            .map_err(HttpReadError::Io)?;
        if body.len() > cap {
            return Err(HttpReadError::BodyTooLarge);
        }
        body
    };

    Ok(HttpResponse { status, body })
}

/// Max bytes in a chunked `Transfer-Encoding` size line (hex digits + extensions).
const MAX_CHUNK_SIZE_LINE_BYTES: usize = 32;

async fn read_chunked_response_body<R>(
    reader: &mut R,
    limits: &HttpResponseLimits,
) -> Result<String, HttpReadError>
where
    R: AsyncBufRead + Unpin,
{
    let mut body = Vec::new();

    loop {
        let mut size_line = Vec::new();
        read_line_bounded(reader, &mut size_line, MAX_CHUNK_SIZE_LINE_BYTES, || {
            HttpReadError::HeaderLineTooLong
        })
        .await?;
        let size_line_text = String::from_utf8_lossy(&size_line);
        let size_hex = size_line_text.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| HttpReadError::Malformed)?;

        if size == 0 {
            let mut trailing = Vec::new();
            read_line_bounded(reader, &mut trailing, MAX_CHUNK_SIZE_LINE_BYTES, || {
                HttpReadError::HeaderLineTooLong
            })
            .await?;
            break;
        }

        if body.len().saturating_add(size) > limits.max_body_bytes {
            return Err(HttpReadError::BodyTooLarge);
        }

        let mut chunk = vec![0u8; size];
        reader
            .read_exact(&mut chunk)
            .await
            .map_err(HttpReadError::Io)?;
        body.extend_from_slice(&chunk);

        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .await
            .map_err(HttpReadError::Io)?;
    }

    Ok(String::from_utf8_lossy(&body).to_string())
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

// --- Cross-origin / DNS-rebinding request gate (issue #228) ---------------
//
// The hand-rolled servers in `genie-core` (`:3000`) and `genie-api` (`:3080`)
// previously answered every request with a wildcard
// `Access-Control-Allow-Origin: *` and performed no `Origin`/`Host`
// validation, so any web page the user happened to open could read private
// conversations/memories and drive home actuation cross-origin. This gate runs
// ahead of routing in both crates and is the single shared place that decides:
//   * whether the request's `Host` is an allowlisted value (closing the
//     DNS-rebinding hole),
//   * whether a present `Origin` is allowlisted (blocking cross-site reads and
//     state-changing requests), and reflected back (the wildcard is gone), and
//   * whether a mutating/actuating endpoint carries the shared local token.
//
// Same-origin browser requests and non-browser clients (no `Origin` header)
// keep working; only an allowlisted `Origin` is ever reflected, always with
// `Vary: Origin`.

/// Why the request gate rejected a request (always answered with `403`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardRejection {
    /// `Host` header is present but not allowlisted (possible DNS-rebinding).
    DisallowedHost,
    /// `Origin` header is present but not allowlisted (cross-site request).
    DisallowedOrigin,
    /// A mutating/actuating endpoint was hit without the configured token.
    MissingToken,
}

impl GuardRejection {
    /// HTTP status to answer a gated-out request with.
    pub const fn status(self) -> u16 {
        403
    }

    /// Short machine-stable reason, surfaced in the JSON error body and logs.
    pub const fn reason(self) -> &'static str {
        match self {
            GuardRejection::DisallowedHost => "host not allowed",
            GuardRejection::DisallowedOrigin => "origin not allowed",
            GuardRejection::MissingToken => "missing or invalid local API token",
        }
    }
}

/// Outcome of [`RequestGuard::check_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginDecision {
    /// The request may proceed. The inner value is the `Origin` to reflect in
    /// `Access-Control-Allow-Origin` (`Some` when an allowlisted `Origin` was
    /// sent), or `None` for a same-origin / non-browser request.
    Allow(Option<String>),
    /// The request must be rejected before routing.
    Reject(GuardRejection),
}

/// Cross-origin / DNS-rebinding request gate shared by both HTTP servers.
///
/// Build one per server with [`RequestGuard::new`], passing the server's own
/// bound address (so loopback `Host`/`Origin` values for the right port are
/// always allowed) plus any operator-configured extras and the shared token.
#[derive(Debug, Clone)]
pub struct RequestGuard {
    allowed_origins: Vec<String>,
    allowed_hosts: Vec<String>,
    token: Option<String>,
}

impl RequestGuard {
    /// Build the gate for a server bound to `listen_host:listen_port`.
    ///
    /// Loopback hosts/origins (`127.0.0.1`, `localhost`, `[::1]`) for the bound
    /// port are always allowed — that covers the on-device browser UIs and the
    /// genie-api→genie-core proxy. When the server binds a specific non-wildcard
    /// address it is added too. LAN access by hostname or a different UI origin
    /// must be opted into via `extra_origins` / `extra_hosts`. A blank `token`
    /// disables token enforcement (the Origin/Host gate still applies).
    pub fn new(
        listen_host: &str,
        listen_port: u16,
        extra_origins: &[String],
        extra_hosts: &[String],
        token: &str,
    ) -> Self {
        let (mut allowed_hosts, mut allowed_origins) =
            derive_hosts_and_origins(listen_host, listen_port);
        for host in extra_hosts {
            let host = host.trim().to_ascii_lowercase();
            if !host.is_empty() {
                push_unique(&mut allowed_hosts, host);
            }
        }
        for origin in extra_origins {
            let origin = normalize_origin(origin);
            if !origin.is_empty() {
                push_unique(&mut allowed_origins, origin);
            }
        }
        let token = token.trim();
        Self {
            allowed_origins,
            allowed_hosts,
            token: (!token.is_empty()).then(|| token.to_string()),
        }
    }

    /// True when a token is configured and will be enforced on mutating routes.
    pub fn enforces_token(&self) -> bool {
        self.token.is_some()
    }

    /// Validate `Host` and `Origin` ahead of routing.
    ///
    /// A missing `Host` (non-browser client) is allowed; a present-but-unlisted
    /// `Host` is rejected. A missing `Origin` (same-origin navigation / non-
    /// browser) is allowed with nothing reflected; a present `Origin` is
    /// reflected verbatim when allowlisted, else rejected.
    pub fn check_request(&self, request: &HttpRequest) -> OriginDecision {
        if let Some(host) = request.header("host") {
            let host = host.trim().to_ascii_lowercase();
            if !host.is_empty() && !self.allowed_hosts.iter().any(|h| h == &host) {
                return OriginDecision::Reject(GuardRejection::DisallowedHost);
            }
        }

        match request.header("origin") {
            None => OriginDecision::Allow(None),
            Some(origin) => {
                if self.allowed_origins.contains(&normalize_origin(origin)) {
                    OriginDecision::Allow(Some(origin.trim().to_string()))
                } else {
                    OriginDecision::Reject(GuardRejection::DisallowedOrigin)
                }
            }
        }
    }

    /// True when the request carries the configured token (via `X-Genie-Token`
    /// or `Authorization: Bearer <token>`), or when no token is configured.
    /// Call only for mutating/actuating endpoints.
    pub fn token_ok(&self, request: &HttpRequest) -> bool {
        let Some(expected) = self.token.as_deref() else {
            return true;
        };
        let presented = request
            .header("x-genie-token")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .or_else(|| bearer_token(request.header("authorization")));
        presented
            .map(|t| constant_time_eq(t.as_bytes(), expected.as_bytes()))
            .unwrap_or(false)
    }
}

/// Build the CORS response header lines (each `\r\n`-terminated) for an
/// outgoing response, given the `Origin` to reflect (from
/// [`RequestGuard::check_request`]).
///
/// With `Some(origin)` the origin is reflected and the permitted methods and
/// request headers are advertised; with `None` only `Vary: Origin` is emitted,
/// so no cross-origin caller may treat the response as world-readable — this is
/// what replaces the old wildcard `Access-Control-Allow-Origin: *`.
pub fn cors_response_headers(reflect_origin: Option<&str>) -> String {
    match reflect_origin {
        Some(origin) => format!(
            "Access-Control-Allow-Origin: {origin}\r\n\
             Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type, Authorization, X-Genie-Origin, X-Genie-Token\r\n\
             Vary: Origin\r\n"
        ),
        None => "Vary: Origin\r\n".to_string(),
    }
}

/// Loopback (+ specific bound address) hosts and origins for `listen_port`.
fn derive_hosts_and_origins(listen_host: &str, listen_port: u16) -> (Vec<String>, Vec<String>) {
    let mut bases = vec![
        "127.0.0.1".to_string(),
        "localhost".to_string(),
        "[::1]".to_string(),
    ];
    let host = listen_host.trim();
    if !host.is_empty() && !matches!(host, "0.0.0.0" | "::") {
        let bracketed = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        if !bases.iter().any(|b| b.eq_ignore_ascii_case(&bracketed)) {
            bases.push(bracketed);
        }
    }

    let mut hosts = Vec::new();
    let mut origins = Vec::new();
    for base in &bases {
        let base = base.to_ascii_lowercase();
        push_unique(&mut hosts, base.clone());
        push_unique(&mut hosts, format!("{base}:{listen_port}"));
        push_unique(&mut origins, format!("http://{base}"));
        push_unique(&mut origins, format!("http://{base}:{listen_port}"));
    }
    (hosts, origins)
}

/// Normalize an `Origin` value for allowlist comparison: trimmed, lowercased,
/// and with any trailing slash removed (browsers never send one, but operator
/// config might).
fn normalize_origin(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// Extract the token from an `Authorization: Bearer <token>` header value,
/// matching the scheme case-insensitively.
fn bearer_token(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    let (scheme, token) = value.split_once(char::is_whitespace)?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

/// Length-independent byte comparison for the local API token, so a network
/// peer cannot learn it byte-by-byte from response timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
    async fn invalid_content_length_is_rejected_with_400() {
        let raw = "POST /api/chat HTTP/1.1\r\nContent-Length: abc\r\n\r\n";
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &test_limits()).await.unwrap_err();
        assert!(matches!(err, HttpReadError::InvalidContentLength));
        assert_eq!(err.status_code(), Some(400));
    }

    #[tokio::test]
    async fn duplicate_content_length_is_rejected_with_400() {
        let raw = "POST /api/chat HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 0\r\n\r\nhello";
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &test_limits()).await.unwrap_err();
        assert!(matches!(err, HttpReadError::InvalidContentLength));
        assert_eq!(err.status_code(), Some(400));
    }

    #[tokio::test]
    async fn transfer_encoding_request_is_rejected_with_400() {
        let raw = "POST /api/chat HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        let mut reader = reader_for(raw.as_bytes());
        let err = read_request(&mut reader, &test_limits()).await.unwrap_err();
        assert!(matches!(err, HttpReadError::UnsupportedTransferEncoding));
        assert_eq!(err.status_code(), Some(400));
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

    fn test_response_limits() -> HttpResponseLimits {
        HttpResponseLimits::with_body_cap(64 * 1024)
    }

    #[tokio::test]
    async fn parses_simple_response_with_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
        let mut reader = reader_for(raw.as_bytes());
        let resp = read_response(&mut reader, &test_response_limits())
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn unterminated_response_header_is_rejected_in_bounded_memory() {
        let mut limits = test_response_limits();
        limits.max_header_line_bytes = 4096;
        let prefix = Cursor::new(b"HTTP/1.1 200 OK\r\nX-Pad: ".to_vec());
        let endless = prefix.chain(tokio::io::repeat(b'A'));
        let mut reader = BufReader::new(endless);
        let err = read_response(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::HeaderLineTooLong));
    }

    #[tokio::test]
    async fn oversized_response_status_line_is_rejected() {
        let mut limits = test_response_limits();
        limits.max_status_line_bytes = 64;
        let raw = format!("HTTP/1.1 200 {}\r\n\r\n", "x".repeat(4096));
        let mut reader = reader_for(raw.as_bytes());
        let err = read_response(&mut reader, &limits).await.unwrap_err();
        assert!(matches!(err, HttpReadError::StatusLineTooLong));
    }

    #[tokio::test]
    async fn chunked_response_body_is_decoded_with_cap() {
        let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let mut reader = reader_for(raw.as_bytes());
        let resp = read_response(&mut reader, &test_response_limits())
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "hello");
    }

    // --- Cross-origin request gate (issue #228) ---------------------------

    fn req(headers: &[(&str, &str)]) -> HttpRequest {
        HttpRequest {
            method: "POST".into(),
            path: "/api/chat".into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect(),
            content_length: 0,
            body: None,
        }
    }

    fn guard() -> RequestGuard {
        RequestGuard::new("127.0.0.1", 3000, &[], &[], "")
    }

    #[test]
    fn loopback_host_and_origin_for_bound_port_are_allowed() {
        let g = guard();
        for host in [
            "127.0.0.1:3000",
            "localhost:3000",
            "[::1]:3000",
            "localhost",
        ] {
            assert_eq!(
                g.check_request(&req(&[("Host", host)])),
                OriginDecision::Allow(None),
                "host {host} should be allowed"
            );
        }
        assert_eq!(
            g.check_request(&req(&[
                ("Host", "localhost:3000"),
                ("Origin", "http://localhost:3000"),
            ])),
            OriginDecision::Allow(Some("http://localhost:3000".into())),
        );
    }

    #[test]
    fn missing_host_and_origin_are_allowed_for_non_browser_clients() {
        assert_eq!(
            guard().check_request(&req(&[])),
            OriginDecision::Allow(None)
        );
    }

    #[test]
    fn cross_site_origin_is_rejected_not_reflected() {
        let g = guard();
        assert_eq!(
            g.check_request(&req(&[
                ("Host", "localhost:3000"),
                ("Origin", "http://evil.example"),
            ])),
            OriginDecision::Reject(GuardRejection::DisallowedOrigin),
        );
    }

    #[test]
    fn rebound_host_is_rejected() {
        // DNS-rebinding: the page is evil.example resolving to 127.0.0.1, so the
        // Host carries the attacker name — it must not match the allowlist.
        let g = guard();
        assert_eq!(
            g.check_request(&req(&[("Host", "evil.example:3000")])),
            OriginDecision::Reject(GuardRejection::DisallowedHost),
        );
    }

    #[test]
    fn configured_extras_are_allowed() {
        let g = RequestGuard::new(
            "0.0.0.0",
            3000,
            &["http://genie.local:3000/".into()],
            &["genie.local:3000".into()],
            "",
        );
        assert_eq!(
            g.check_request(&req(&[
                ("Host", "genie.local:3000"),
                ("Origin", "http://genie.local:3000"),
            ])),
            OriginDecision::Allow(Some("http://genie.local:3000".into())),
        );
    }

    #[test]
    fn token_required_only_when_configured() {
        let open = guard();
        assert!(open.token_ok(&req(&[])), "no token configured => allowed");
        assert!(!open.enforces_token());

        let g = RequestGuard::new("127.0.0.1", 3000, &[], &[], "s3cret");
        assert!(g.enforces_token());
        assert!(!g.token_ok(&req(&[])), "missing token => rejected");
        assert!(!g.token_ok(&req(&[("X-Genie-Token", "wrong")])));
        assert!(g.token_ok(&req(&[("X-Genie-Token", "s3cret")])));
        assert!(g.token_ok(&req(&[("Authorization", "Bearer s3cret")])));
        assert!(g.token_ok(&req(&[("Authorization", "bearer s3cret")])));
        assert!(!g.token_ok(&req(&[("Authorization", "Bearer ")])));
    }

    #[test]
    fn cors_headers_reflect_only_allowlisted_origin() {
        let reflected = cors_response_headers(Some("http://localhost:3000"));
        assert!(reflected.contains("Access-Control-Allow-Origin: http://localhost:3000\r\n"));
        assert!(reflected.contains("Vary: Origin\r\n"));
        assert!(!reflected.contains('*'));

        let bare = cors_response_headers(None);
        assert!(!bare.contains("Access-Control-Allow-Origin"));
        assert_eq!(bare, "Vary: Origin\r\n");
    }

    #[test]
    fn shared_route_token_policy_covers_sensitive_actuation_reads() {
        for surface in [RoutePolicySurface::GenieCore, RoutePolicySurface::GenieApi] {
            assert!(route_requires_local_token(
                surface,
                "GET",
                "/api/actuation/pending"
            ));
            assert!(route_requires_local_token(
                surface,
                "GET",
                "/api/actuation/actions"
            ));
            assert!(route_requires_local_token(
                surface,
                "POST",
                "/api/actuation/confirm"
            ));
        }

        assert!(!route_requires_local_token(
            RoutePolicySurface::GenieCore,
            "GET",
            "/api/actuation/audit"
        ));
        assert!(route_requires_local_token(
            RoutePolicySurface::GenieApi,
            "GET",
            "/api/actuation/audit"
        ));
    }
}
