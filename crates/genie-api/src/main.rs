mod http;
mod routes;

use anyhow::Result;
use genie_common::config::Config;
use tracing_subscriber::EnvFilter;

/// GeniePod API server.
///
/// Lightweight HTTP server (no framework dependencies — raw tokio TcpListener).
/// Serves REST endpoints + static dashboard files.
///
/// Endpoints:
///   GET  /api/status      — current mode, memory, uptime
///   GET  /api/tegrastats  — recent tegrastats history (JSON array)
///   GET  /api/services    — service health status
///   GET  /api/security    — redacted household security posture
///   POST /api/mode        — change operating mode
///   GET  /                — dashboard HTML
///   GET  /dashboard.js    — dashboard JavaScript
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let config = Config::load()?;
    let bind_addr = "127.0.0.1:3080";

    tracing::info!(addr = bind_addr, "GeniePod API server starting");

    http::serve(bind_addr, config).await
}
