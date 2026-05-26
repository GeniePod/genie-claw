//! Plain-TCP and TLS HTTP probes for configured service URLs.

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

/// Probe a parsed service URL. Uses system trust roots for `https://` URLs.
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
        let n = timeout(read_timeout, stream.read(&mut chunk))
            .await
            .map_err(|_| anyhow::anyhow!("read timeout"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            anyhow::bail!("response too large");
        }
    }

    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| idx + 4)
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response"))?;
    let status = parse_http_status(&buf[..header_end.min(buf.len())])?;
    let body = String::from_utf8_lossy(&buf[header_end..])
        .trim()
        .to_string();
    Ok((status, body))
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
}
