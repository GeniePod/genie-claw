//! Transport-level hardening for the hand-rolled HTTP/1.1 server.
//!
//! See the equivalent module in genie-core for the full rationale. genie-api
//! ships its own copy so the API crate doesn't pick up a transitive
//! dependency on the genie-core runtime. The two implementations should stay
//! in sync; if you change the bounds or timeout policy here, mirror it in
//! `genie-core/src/http_safety.rs` as well.

use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

pub const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;
pub const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
pub const MAX_TOTAL_HEADER_BYTES: usize = 64 * 1024;
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Soft cap on simultaneously-served connections. genie-api fronts a polling
/// dashboard (one refresh every ~5 s) so the ceiling is intentionally low
/// compared to genie-core's chat workload.
pub const MAX_CONCURRENT_CONNECTIONS: usize = 128;

#[derive(Debug, PartialEq, Eq)]
pub enum LineRead {
    Ok,
    TooLong,
    Eof,
    Timeout,
}

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
        let mut r = reader(b"GET / HTTP/1.1\r\n");
        let mut buf = String::new();
        let outcome =
            read_capped_line(&mut r, &mut buf, MAX_REQUEST_LINE_BYTES, HEADER_READ_TIMEOUT)
                .await
                .unwrap();
        assert_eq!(outcome, LineRead::Ok);
    }

    #[tokio::test]
    async fn returns_too_long_when_line_exceeds_cap() {
        let payload = vec![b'a'; 200];
        let mut r = reader(&payload);
        let mut buf = String::new();
        let outcome = read_capped_line(&mut r, &mut buf, 100, HEADER_READ_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(outcome, LineRead::TooLong);
        assert!(buf.len() <= 100);
    }

    #[tokio::test]
    async fn returns_timeout_when_reader_stalls() {
        let (client, _server) = tokio::io::duplex(64);
        let mut r = BufReader::new(client);
        let mut buf = String::new();
        let outcome = read_capped_line(&mut r, &mut buf, 1024, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(outcome, LineRead::Timeout);
    }
}
