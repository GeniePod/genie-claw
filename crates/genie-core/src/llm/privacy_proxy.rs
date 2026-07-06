//! PrivacyProxy backend for optional privacy-preserving cloud escalation.
//!
//! PrivacyProxy is an on-device anonymizing gateway. When the local model
//! fails or the context overflows, genie-core routes the request through
//! PrivacyProxy, which masks household identifiers (person names, device
//! aliases, etc.) before forwarding to a cloud model, then restores them
//! in the response. See issue #418.
//!
//! Architecture:
//!   genie-core → PrivacyProxy (localhost) → cloud LLM
//!
//! PrivacyProxy exposes an OpenAI-compatible endpoint at its configured
//! base URL. A vocabulary-seeding endpoint (`vocab_path`) receives the
//! set of household terms to mask, enabling deterministic substitution
//! (e.g. "Alex" → "__PERSON_1__") across a session.
//!
//! Safety invariant: `base_url` must always be a localhost address.
//! The config layer enforces this via `PrivacyProxyConfig::endpoint_is_valid`.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use genie_common::probe::{ProbeTimeouts, http_request};

use super::openai_compat::OpenAiCompatClient;
use super::{LlmBackendClient, LlmRequestHints, Message, ResponseFormat};

/// Connect-timeout cap for the vocabulary-seeding request.
const VOCAB_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Read-timeout cap. Without this, a proxy that accepts the connection but
/// never responds would previously go undetected entirely — `seed_vocab`
/// wrote the request and returned `Ok(())` without ever reading a reply.
const VOCAB_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Body-size cap on the vocab-seed acknowledgement.
const VOCAB_MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// LLM backend that routes through the on-device PrivacyProxy.
///
/// The proxy applies deterministic masking to household identifiers
/// before forwarding to its configured cloud model, then un-masks the
/// response before returning it. From genie-core's perspective this is
/// just another `LlmBackendClient`; the masking is transparent.
///
/// Only call [`PrivacyProxyBackend::seed_vocab`] with terms derived from
/// memory facts that have [`EscalationPolicy::Anonymized`]. Facts with
/// [`EscalationPolicy::LocalOnly`] must never be seeded because the proxy
/// sees raw content before masking.
pub struct PrivacyProxyBackend {
    client: OpenAiCompatClient,
    host: String,
    port: u16,
    vocab_path: String,
}

impl PrivacyProxyBackend {
    /// Build a backend from a `base_url` (e.g. `"http://127.0.0.1:8180/v1"`)
    /// and the proxy's vocabulary-seeding path (e.g. `"/vocab/seed"`).
    pub fn from_url(base_url: &str, vocab_path: &str) -> Self {
        let stripped = base_url.strip_prefix("http://").unwrap_or(base_url);
        let (host_port, _) = stripped.split_once('/').unwrap_or((stripped, ""));
        let (host, port_str) = host_port.split_once(':').unwrap_or((host_port, "8180"));
        let port: u16 = port_str.parse().unwrap_or(8180);

        Self {
            client: OpenAiCompatClient::from_url("privacy-proxy", base_url),
            host: host.to_string(),
            port,
            vocab_path: vocab_path.to_string(),
        }
    }

    /// Seed PrivacyProxy's masking vocabulary with household entity names.
    ///
    /// Terms are posted to the proxy's `vocab_path` endpoint so that the
    /// proxy can build a stable, session-scoped substitution map (e.g.
    /// "Alex" → "__PERSON_1__", "kitchen light" → "__DEVICE_2__") before
    /// the first chat request arrives.
    ///
    /// Only call this with terms extracted from memory entries whose
    /// [`escalation_policy`] returns [`EscalationPolicy::Anonymized`].
    /// Restriced or private terms must be excluded.
    ///
    /// A seeding failure is logged but does not abort the escalation path;
    /// the proxy will still anonymize what it can from prior context.
    ///
    /// [`escalation_policy`]: crate::memory::policy::escalation_policy
    /// [`EscalationPolicy::Anonymized`]: crate::memory::policy::EscalationPolicy::Anonymized
    pub async fn seed_vocab(&self, terms: &[String]) -> Result<()> {
        if terms.is_empty() {
            return Ok(());
        }

        let body = serde_json::to_string(&serde_json::json!({ "terms": terms }))?;
        let addr = format!("{}:{}", self.host, self.port);

        let (status, response_body) = http_request(
            &addr,
            &self.vocab_path,
            false,
            "POST",
            &[("Content-Type", "application/json")],
            Some(&body),
            ProbeTimeouts {
                connect: VOCAB_CONNECT_TIMEOUT,
                read: VOCAB_REQUEST_TIMEOUT,
            },
            VOCAB_MAX_RESPONSE_BYTES,
        )
        .await?;

        if !(200..300).contains(&status) {
            anyhow::bail!(
                "PrivacyProxy vocab seed {} failed: HTTP {}{}",
                self.vocab_path,
                status,
                if response_body.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", response_body.trim())
                }
            );
        }

        tracing::debug!(
            terms = terms.len(),
            path = %self.vocab_path,
            "seeded PrivacyProxy vocabulary"
        );

        Ok(())
    }
}

#[async_trait]
impl LlmBackendClient for PrivacyProxyBackend {
    fn backend_name(&self) -> &str {
        "privacy-proxy"
    }

    async fn health(&self) -> bool {
        self.client.health().await
    }

    async fn chat_with_format(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        self.client
            .chat_with_format(messages, max_tokens, response_format)
            .await
    }

    async fn chat_with_format_and_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        let _ = hints;
        self.client
            .chat_with_format(messages, max_tokens, response_format)
            .await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        self.client
            .chat_stream(messages, max_tokens, on_token)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_name_is_privacy_proxy() {
        let backend = PrivacyProxyBackend::from_url("http://127.0.0.1:8180/v1", "/vocab/seed");
        assert_eq!(backend.backend_name(), "privacy-proxy");
    }

    #[test]
    fn parses_host_and_port_from_url() {
        let backend = PrivacyProxyBackend::from_url("http://127.0.0.1:8180/v1", "/vocab/seed");
        assert_eq!(backend.host, "127.0.0.1");
        assert_eq!(backend.port, 8180);
        assert_eq!(backend.vocab_path, "/vocab/seed");
    }

    #[test]
    fn uses_default_port_when_missing() {
        let backend = PrivacyProxyBackend::from_url("http://127.0.0.1/v1", "/vocab/seed");
        assert_eq!(backend.host, "127.0.0.1");
        assert_eq!(backend.port, 8180);
    }

    #[test]
    fn parses_localhost_alias() {
        let backend = PrivacyProxyBackend::from_url("http://localhost:9090/v1", "/vocab/seed");
        assert_eq!(backend.host, "localhost");
        assert_eq!(backend.port, 9090);
    }

    #[tokio::test]
    async fn seed_vocab_with_empty_terms_returns_ok_without_connecting() {
        // No listener bound at all — if this connected, it would fail.
        let backend = PrivacyProxyBackend::from_url("http://127.0.0.1:1/v1", "/vocab/seed");
        assert!(backend.seed_vocab(&[]).await.is_ok());
    }

    /// Regression for the "fire and forget" bug: `seed_vocab` previously
    /// wrote the request and returned `Ok(())` without ever reading a
    /// response, so a proxy rejecting the vocab (e.g. malformed terms, 500)
    /// went completely undetected. It must now surface a non-2xx status.
    #[tokio::test]
    async fn seed_vocab_surfaces_non_2xx_as_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"error":"unknown term format"}"#;
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let backend = PrivacyProxyBackend::from_url(&format!("http://{addr}/v1"), "/vocab/seed");
        let err = backend
            .seed_vocab(&["Alex".to_string()])
            .await
            .expect_err("a 400 response must surface as an error, not a silent Ok");
        server.abort();

        assert!(
            err.to_string().contains("400"),
            "expected the status to appear in the error, got: {err}"
        );
    }

    /// A 2xx acknowledgement (with the request actually reaching the
    /// server) still succeeds — the fix must not turn a healthy proxy into
    /// a false failure.
    #[tokio::test]
    async fn seed_vocab_succeeds_and_sends_terms_on_2xx() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
            request
        });

        let backend = PrivacyProxyBackend::from_url(&format!("http://{addr}/v1"), "/vocab/seed");
        backend
            .seed_vocab(&["Alex".to_string(), "kitchen light".to_string()])
            .await
            .expect("a 200 response must succeed");

        let request = server.await.unwrap();
        assert!(request.starts_with("POST /vocab/seed HTTP/1.1"));
        assert!(request.contains("Alex"));
        assert!(request.contains("kitchen light"));
    }
}
