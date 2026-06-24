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

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use super::openai_compat::OpenAiCompatClient;
use super::{LlmBackendClient, LlmRequestHints, Message, ResponseFormat};

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

        let stream =
            tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr))
                .await??;

        let (_, mut writer) = stream.into_split();
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.vocab_path,
            addr,
            body.len(),
            body
        );
        writer.write_all(request.as_bytes()).await?;

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
}
