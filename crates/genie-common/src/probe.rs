//! Plain-TCP and TLS HTTP probes for configured service URLs.
//!
//! HTTPS probes verify server certificates against the Mozilla CA bundle shipped
//! in the `webpki-roots` crate (not the host OS trust store). LAN services with
//! self-signed certificates — common for local Home Assistant HTTPS — will fail
//! TLS verification until an opt-in trust policy is added.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use rustls::pki_types::{IpAddr, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use crate::config::ServiceProbeTarget;

#[derive(Debug, Clone, Copy)]
pub struct ProbeTimeouts {
    pub connect: Duration,
    pub read: Duration,
}

impl Default for ProbeTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(3),
            read: Duration::from_secs(3),
        }
    }
}

/// Probe a parsed service URL.
///
/// `https://` targets use the bundled Mozilla CA roots from `webpki-roots`.
/// Self-signed LAN certificates (e.g. local Home Assistant HTTPS) are rejected.
pub async fn probe_service_target(
    target: &ServiceProbeTarget,
    timeouts: ProbeTimeouts,
) -> Result<()> {
    match target {
        ServiceProbeTarget::Http { addr, path } => {
            probe_http_get(addr, path, false, timeouts).await
        }
        ServiceProbeTarget::Https { addr, path } => {
            probe_http_get(addr, path, true, timeouts).await
        }
        ServiceProbeTarget::UnsupportedScheme { scheme } => {
            anyhow::bail!("unsupported URL scheme for probe: {scheme}")
        }
    }
}

/// Probe a configured URL string (bare authority defaults to `http://`).
pub async fn probe_configured_url(url: &str, timeouts: ProbeTimeouts) -> Result<()> {
    probe_service_target(&crate::config::parse_service_probe_target(url), timeouts).await
}

/// Issue a minimal HTTP GET and require a 2xx/3xx status line.
pub async fn probe_http_get(
    addr: &str,
    path: &str,
    tls: bool,
    timeouts: ProbeTimeouts,
) -> Result<()> {
    let (status, _) = if tls {
        probe_http_response_tls(addr, path, timeouts).await?
    } else {
        probe_http_response_plain(addr, path, timeouts).await?
    };
    validate_probe_status(status)
}

/// Issue a minimal HTTP GET and return the response body on 2xx/3xx.
pub async fn probe_http_get_body(
    addr: &str,
    path: &str,
    tls: bool,
    timeouts: ProbeTimeouts,
) -> Result<String> {
    let (status, body) = if tls {
        probe_http_response_tls(addr, path, timeouts).await?
    } else {
        probe_http_response_plain(addr, path, timeouts).await?
    };
    validate_probe_status(status)?;
    Ok(body)
}

pub async fn probe_target_body(
    target: &ServiceProbeTarget,
    path: &str,
    timeouts: ProbeTimeouts,
) -> Result<String> {
    match target {
        ServiceProbeTarget::Http { addr, .. } => {
            probe_http_get_body(addr, path, false, timeouts).await
        }
        ServiceProbeTarget::Https { addr, .. } => {
            probe_http_get_body(addr, path, true, timeouts).await
        }
        ServiceProbeTarget::UnsupportedScheme { scheme } => {
            anyhow::bail!("unsupported URL scheme for probe: {scheme}")
        }
    }
}

async fn probe_http_response_plain(
    addr: &str,
    path: &str,
    timeouts: ProbeTimeouts,
) -> Result<(u16, String)> {
    let mut stream = timeout(timeouts.connect, TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("connect timeout"))??;

    write_get_request(&mut stream, path, addr).await?;
    read_http_response(&mut stream, timeouts.read).await
}

async fn probe_http_response_tls(
    addr: &str,
    path: &str,
    timeouts: ProbeTimeouts,
) -> Result<(u16, String)> {
    let tcp = timeout(timeouts.connect, TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("connect timeout"))??;

    let server_name = tls_server_name(addr)?;
    let connector = tls_connector()?;
    let mut stream = timeout(timeouts.connect, connector.connect(server_name, tcp))
        .await
        .map_err(|_| anyhow::anyhow!("TLS handshake timeout"))??;

    write_get_request(&mut stream, path, addr).await?;
    read_http_response(&mut stream, timeouts.read).await
}

async fn read_http_response(
    stream: &mut (impl AsyncReadExt + Unpin),
    read_timeout: Duration,
) -> Result<(u16, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        if let Some(idx) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = idx + 4;
            let status = parse_http_status(&buf[..header_end])?;
            let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
            let body = read_http_body(stream, &mut buf, header_end, &headers, read_timeout).await?;
            return Ok((status, body));
        }

        if buf.len() > 64 * 1024 {
            anyhow::bail!("response too large");
        }

        let n = read_with_timeout(stream, read_timeout, &mut chunk).await?;
        if n == 0 {
            anyhow::bail!("invalid HTTP response");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn read_http_body(
    stream: &mut (impl AsyncReadExt + Unpin),
    buf: &mut Vec<u8>,
    header_end: usize,
    headers: &str,
    read_timeout: Duration,
) -> Result<String> {
    let mut body = buf.split_off(header_end);

    if headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
    {
        let decoded = read_chunked_body(stream, &mut body, read_timeout).await?;
        return Ok(String::from_utf8_lossy(&decoded).trim().to_string());
    }

    if let Some(content_length) = parse_content_length(headers) {
        read_fixed_body(stream, &mut body, content_length, read_timeout).await?;
        return Ok(String::from_utf8_lossy(&body).trim().to_string());
    }

    // No Content-Length and not chunked — treat headers as complete. Do not
    // wait for EOF; keep-alive servers would otherwise hit the read timeout.
    Ok(String::from_utf8_lossy(&body).trim().to_string())
}

async fn read_fixed_body(
    stream: &mut (impl AsyncReadExt + Unpin),
    body: &mut Vec<u8>,
    content_length: usize,
    read_timeout: Duration,
) -> Result<()> {
    let mut chunk = [0u8; 1024];
    while body.len() < content_length {
        if body.len() > 64 * 1024 {
            anyhow::bail!("response too large");
        }
        let n = read_with_timeout(stream, read_timeout, &mut chunk).await?;
        if n == 0 {
            anyhow::bail!("invalid HTTP response");
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    Ok(())
}

async fn read_chunked_body(
    stream: &mut (impl AsyncReadExt + Unpin),
    scratch: &mut Vec<u8>,
    read_timeout: Duration,
) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        while !scratch.contains(&b'\n') {
            if scratch.len() > 64 * 1024 {
                anyhow::bail!("response too large");
            }
            let n = read_with_timeout(stream, read_timeout, &mut chunk).await?;
            if n == 0 {
                anyhow::bail!("invalid HTTP response");
            }
            scratch.extend_from_slice(&chunk[..n]);
        }

        let line_end = scratch.iter().position(|&b| b == b'\n').unwrap();
        let size_line = String::from_utf8_lossy(&scratch[..line_end])
            .trim()
            .to_string();
        let chunk_size =
            usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| anyhow::anyhow!("invalid HTTP response"))?;
        scratch.drain(..=line_end);

        if chunk_size == 0 {
            return Ok(decoded);
        }

        while scratch.len() < chunk_size + 2 {
            if decoded.len() + scratch.len() > 64 * 1024 {
                anyhow::bail!("response too large");
            }
            let n = read_with_timeout(stream, read_timeout, &mut chunk).await?;
            if n == 0 {
                anyhow::bail!("invalid HTTP response");
            }
            scratch.extend_from_slice(&chunk[..n]);
        }

        decoded.extend_from_slice(&scratch[..chunk_size]);
        scratch.drain(..chunk_size + 2);
    }
}

async fn read_with_timeout(
    stream: &mut (impl AsyncReadExt + Unpin),
    read_timeout: Duration,
    chunk: &mut [u8; 1024],
) -> Result<usize> {
    timeout(read_timeout, stream.read(chunk))
        .await
        .map_err(|_| anyhow::anyhow!("read timeout"))?
        .context("failed to read HTTP response")
}

fn parse_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("content-length") {
            return None;
        }
        value.trim().parse().ok()
    })
}

async fn write_get_request(
    stream: &mut (impl AsyncWriteExt + Unpin),
    path: &str,
    host: &str,
) -> Result<()> {
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );
    stream
        .write_all(request.as_bytes())
        .await
        .context("failed to write HTTP request")
}

fn parse_http_status(buf: &[u8]) -> Result<u16> {
    let response = String::from_utf8_lossy(buf);
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response"))
}

fn validate_probe_status(status: u16) -> Result<()> {
    if (200..400).contains(&status) {
        Ok(())
    } else if status > 0 {
        anyhow::bail!("HTTP {status}")
    } else {
        anyhow::bail!("invalid HTTP response")
    }
}

fn tls_connector() -> Result<TlsConnector> {
    static CONNECTOR: OnceLock<TlsConnector> = OnceLock::new();

    Ok(CONNECTOR
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            // Mozilla CA bundle (webpki-roots), not the host OS trust store.
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            TlsConnector::from(Arc::new(config))
        })
        .clone())
}

fn tls_server_name(addr: &str) -> Result<ServerName<'static>> {
    let host = host_from_addr(addr);
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(ServerName::IpAddress(IpAddr::from(ip)));
    }
    ServerName::try_from(host.to_string())
        .map_err(|_| anyhow::anyhow!("invalid TLS server name: {host}"))
}

fn host_from_addr(addr: &str) -> &str {
    if let Some(rest) = addr.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if let Some((host, port)) = addr.rsplit_once(':') {
        if port.chars().all(|ch| ch.is_ascii_digit()) {
            host
        } else {
            addr
        }
    } else {
        addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_service_probe_target;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn parse_https_service_target_uses_default_port() {
        match parse_service_probe_target("https://ha.example/api/") {
            ServiceProbeTarget::Https { addr, path } => {
                assert_eq!(addr, "ha.example:443");
                assert_eq!(path, "/api/");
            }
            other => panic!("expected Https target, got {other:?}"),
        }
    }

    #[test]
    fn host_from_addr_handles_bracketed_ipv6() {
        assert_eq!(host_from_addr("[::1]:443"), "::1");
        assert_eq!(host_from_addr("127.0.0.1:8443"), "127.0.0.1");
    }

    #[tokio::test]
    async fn probe_http_get_accepts_plain_http_200() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        probe_http_get(
            &addr.to_string(),
            "/health",
            false,
            ProbeTimeouts {
                connect: Duration::from_secs(2),
                read: Duration::from_secs(2),
            },
        )
        .await
        .unwrap();

        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_http_get_accepts_keep_alive_without_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
            // Leave the socket open — a keep-alive server would not send EOF.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        probe_http_get(
            &addr.to_string(),
            "/health",
            false,
            ProbeTimeouts {
                connect: Duration::from_secs(2),
                read: Duration::from_secs(2),
            },
        )
        .await
        .unwrap();

        server.abort();
    }

    #[tokio::test]
    async fn probe_http_get_body_reads_content_length_without_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: 5\r\n\r\nhello",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let body = probe_http_get_body(
            &addr.to_string(),
            "/health",
            false,
            ProbeTimeouts {
                connect: Duration::from_secs(2),
                read: Duration::from_secs(2),
            },
        )
        .await
        .unwrap();

        assert_eq!(body, "hello");
        server.abort();
    }
}
