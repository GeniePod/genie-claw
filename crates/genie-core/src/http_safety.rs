//! Transport-level hardening for the hand-rolled HTTP/1.1 server.
//!
//! The genie-core and genie-api servers parse requests by hand on top of
//! tokio's `TcpListener` / `BufReader` so the binary stays small and free of a
//! framework dependency. That choice is fine — but only if the parser refuses
//! to allocate or wait without bound. This module supplies the bounded,
//! deadline-aware primitives the request handler relies on:
//!
//! * [`read_capped_line`] reads a single CRLF-terminated line with a hard byte
//!   cap and a wall-clock deadline. A line longer than the cap returns
//!   [`LineRead::TooLong`] (the caller then replies `431 Request Header Fields
//!   Too Large`). A reader that goes silent past the deadline returns
//!   [`LineRead::Timeout`] (the caller replies `408 Request Timeout`).
//! * The [`MAX_REQUEST_LINE_BYTES`] / [`MAX_HEADER_LINE_BYTES`] /
//!   [`MAX_TOTAL_HEADER_BYTES`] constants cap memory at parse time; the
//!   [`HEADER_READ_TIMEOUT`] / [`BODY_READ_TIMEOUT`] constants cap how long an
//!   idle peer can hold a connection open.
//! * [`MAX_CONCURRENT_CONNECTIONS`] is a hint the listener uses to size a
//!   semaphore so a single peer can't open thousands of connections and force
//!   the daemon into `EMFILE`.
//!
//! All four limits are deliberately tighter than browser HTTP/1.1 defaults
//! (nginx / Apache use 8 KB headers and a 60 s keep-alive timeout) because the
//! traffic this server handles is the local dashboard, a few first-party
//! adapters, and the chat UI — never an arbitrary internet client.

use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// Maximum bytes accepted on the HTTP request line ("GET /path HTTP/1.1").
pub const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;

/// Maximum bytes accepted on a single header line.
pub const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;

/// Maximum total bytes across all header lines for one request.
pub const MAX_TOTAL_HEADER_BYTES: usize = 64 * 1024;

/// Maximum wall-clock time spent reading the request line + all headers.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum wall-clock time spent reading the request body.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Soft cap on simultaneously-served HTTP connections per server.
///
/// The listener uses this to size a semaphore: a 257th client blocks at
/// `accept_owned()` rather than spawning a task and consuming an fd. Picked
/// well below typical `RLIMIT_NOFILE` (1024+) so the daemon retains fds for
/// SQLite, the LLM socket, and the connectivity coprocessor.
pub const MAX_CONCURRENT_CONNECTIONS: usize = 256;

/// Outcome of a single bounded line read.
#[derive(Debug, PartialEq, Eq)]
pub enum LineRead {
    /// Read completed with a trailing `\n` and within the byte cap.
    Ok,
    /// The line reached the byte cap before any `\n` was seen.
    TooLong,
    /// EOF before a `\n` (peer closed the connection).
    Eof,
    /// Wall-clock deadline elapsed before the read could complete.
    Timeout,
}

/// Read one CRLF-terminated line into `buf` with a hard byte cap and timeout.
///
/// The cap is enforced by wrapping `reader` in [`tokio::io::AsyncReadExt::take`]
/// so the underlying allocation is bounded even if the peer never sends a
/// newline. The timeout is enforced by [`tokio::time::timeout`] so a stalled
/// reader cannot pin the task forever.
///
/// On `TooLong` or `Timeout` the caller MUST stop reading from this connection
/// — the next bytes on the wire may be the tail of the over-long line and
/// would be interpreted as a fresh header.
pub async fn read_capped_line<R>(
    reader: &mut R,
    buf: &mut String,
    max_bytes: usize,
    read_timeout: Duration,
) -> std::io::Result<LineRead>
where
    R: AsyncBufRead + Unpin,
{
    let start_len = buf.len();
    let res = tokio::time::timeout(read_timeout, async {
        let mut take = AsyncReadExt::take(reader, max_bytes as u64);
        AsyncBufReadExt::read_line(&mut take, buf).await?;
        Ok::<u64, std::io::Error>(take.limit())
    })
    .await;

    match res {
        Err(_elapsed) => Ok(LineRead::Timeout),
        Ok(Err(e)) => Err(e),
        Ok(Ok(remaining)) => {
            let bytes_read = buf.len() - start_len;
            if bytes_read == 0 {
                Ok(LineRead::Eof)
            } else if buf.ends_with('\n') {
                Ok(LineRead::Ok)
            } else if remaining == 0 {
                Ok(LineRead::TooLong)
            } else {
                Ok(LineRead::Eof)
            }
        }
    }
}

/// Render a minimal HTTP/1.1 error response (status + plain-text body).
///
/// Used for transport-layer errors (request too large, timed out, etc.) where
/// the normal JSON-routing path is not reachable.
pub fn build_error_response(status: u16, reason: &str) -> String {
    let body = reason;
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_phrase(status),
        body.len(),
        body,
    )
}

fn status_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    fn reader(bytes: &[u8]) -> BufReader<Cursor<Vec<u8>>> {
        BufReader::new(Cursor::new(bytes.to_vec()))
    }

    #[tokio::test]
    async fn reads_complete_line_within_cap() {
        let mut r = reader(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        let mut buf = String::new();
        let outcome =
            read_capped_line(&mut r, &mut buf, MAX_REQUEST_LINE_BYTES, HEADER_READ_TIMEOUT)
                .await
                .unwrap();
        assert_eq!(outcome, LineRead::Ok);
        assert_eq!(buf, "GET / HTTP/1.1\r\n");
    }

    #[tokio::test]
    async fn returns_too_long_when_line_exceeds_cap() {
        // 100-byte cap, 200-byte unterminated line.
        let payload = vec![b'a'; 200];
        let mut r = reader(&payload);
        let mut buf = String::new();
        let outcome = read_capped_line(&mut r, &mut buf, 100, HEADER_READ_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(outcome, LineRead::TooLong);
        // Allocation must be bounded by the cap.
        assert!(buf.len() <= 100, "buffer grew past cap: {} bytes", buf.len());
    }

    #[tokio::test]
    async fn returns_eof_on_empty_input() {
        let mut r = reader(b"");
        let mut buf = String::new();
        let outcome = read_capped_line(&mut r, &mut buf, 1024, HEADER_READ_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(outcome, LineRead::Eof);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn returns_timeout_when_reader_stalls() {
        // A duplex stream whose write end we never drive — the read side
        // hangs until the timeout elapses.
        let (client, _server) = tokio::io::duplex(64);
        let mut r = BufReader::new(client);
        let mut buf = String::new();
        let outcome = read_capped_line(&mut r, &mut buf, 1024, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(outcome, LineRead::Timeout);
    }

    #[test]
    fn error_response_includes_status_and_body() {
        let body = "header too large";
        let r = build_error_response(431, body);
        assert!(r.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
        assert!(r.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(r.ends_with(body));
    }
}
