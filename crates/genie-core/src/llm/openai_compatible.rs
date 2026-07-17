//! Generic OpenAI-compatible HTTP/HTTPS transport for optional API providers (#569).
//!
//! Unlike the localhost raw-TCP client in [`super::openai_compat`], this backend
//! uses `reqwest` so it can reach loopback *and* remote HTTPS endpoints while
//! preserving the configured base path (for example `/v1`).

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Url;

use super::openai_compat::{
    ChatResponse, LlmTimeouts, backend_error_message, serialize_generic_chat_request, truncate_body,
};
use super::{LlmBackendClient, LlmRequestHints, Message, ResponseFormat};
use crate::security::sandbox::sanitize_output;

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// How the backend obtains its bearer credential.
#[derive(Clone)]
enum CredentialSource {
    /// Literal token (tests / explicit constructors). Never logged.
    Literal(String),
    /// Environment variable name resolved on every request (#569 fail-closed).
    EnvVar(String),
}

impl fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(_) => f.write_str("CredentialSource::Literal([redacted])"),
            Self::EnvVar(name) => f
                .debug_tuple("CredentialSource::EnvVar")
                .field(name)
                .finish(),
        }
    }
}

/// Generic OpenAI-compatible adapter for API providers that authenticate with
/// bearer tokens, including OAuth access tokens.
pub struct OpenAiCompatibleBackend {
    base_url: Url,
    model: String,
    credential: CredentialSource,
    timeouts: LlmTimeouts,
    http: reqwest::Client,
}

impl fmt::Debug for OpenAiCompatibleBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleBackend")
            .field("base_url", &self.base_url.as_str())
            .field("model", &self.model)
            .field("credential", &self.credential)
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleBackend {
    pub fn from_url_with_bearer_token(url: &str, token: impl AsRef<str>) -> Self {
        Self::from_url_with_bearer_token_and_timeouts(url, token, LlmTimeouts::default())
    }

    pub fn from_url_with_bearer_token_and_timeouts(
        url: &str,
        token: impl AsRef<str>,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self::new(
            url,
            "default",
            CredentialSource::Literal(token.as_ref().to_string()),
            timeouts,
        )
        .expect("literal-token OpenAI-compatible URL must be http/https")
    }

    pub fn from_url_with_bearer_token_env(url: &str, env_var: impl AsRef<str>) -> Self {
        Self::from_url_with_bearer_token_env_and_timeouts(url, env_var, LlmTimeouts::default())
    }

    pub fn from_url_with_bearer_token_env_and_timeouts(
        url: &str,
        env_var: impl AsRef<str>,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self::new(
            url,
            "default",
            CredentialSource::EnvVar(env_var.as_ref().trim().to_string()),
            timeouts,
        )
        .expect("env-token OpenAI-compatible URL must be http/https")
    }

    /// Same as [`Self::from_url_with_bearer_token_env_and_timeouts`], but with
    /// an operator-configured `model` instead of the `"default"` placeholder
    /// (#620) — required for OpenAI-compatible backends that reject it.
    pub fn from_url_with_bearer_token_env_and_model(
        url: &str,
        env_var: impl AsRef<str>,
        model: impl Into<String>,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self::new(
            url,
            model,
            CredentialSource::EnvVar(env_var.as_ref().trim().to_string()),
            timeouts,
        )
        .expect("configured OpenAI-compatible URL must be http/https")
    }

    /// Fallible constructor for config/load paths and integration tests.
    pub fn try_new(
        url: &str,
        model: impl Into<String>,
        credential_env: impl AsRef<str>,
        timeouts: LlmTimeouts,
    ) -> Result<Self> {
        Self::new(
            url,
            model,
            CredentialSource::EnvVar(credential_env.as_ref().trim().to_string()),
            timeouts,
        )
    }

    fn new(
        url: &str,
        model: impl Into<String>,
        credential: CredentialSource,
        timeouts: LlmTimeouts,
    ) -> Result<Self> {
        let base_url = parse_openai_compatible_base_url(url)?;
        // No client-wide request timeout: non-stream calls set one per request,
        // and streaming relies on per-chunk idle timeouts instead.
        let http = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .pool_max_idle_per_host(0)
            .build()
            .context("failed to build OpenAI-compatible HTTP client")?;
        Ok(Self {
            base_url,
            model: model.into(),
            credential,
            timeouts,
            http,
        })
    }

    fn chat_completions_url(&self) -> Result<Url> {
        join_chat_completions(&self.base_url)
    }

    fn resolve_bearer_token(&self) -> Result<String> {
        let token = match &self.credential {
            CredentialSource::Literal(token) => token.clone(),
            CredentialSource::EnvVar(name) => {
                if name.is_empty() {
                    anyhow::bail!(
                        "openai-compatible provider misconfigured: credential environment variable name is empty"
                    );
                }
                match std::env::var(name) {
                    Ok(value) => value,
                    Err(_) => anyhow::bail!(
                        "openai-compatible provider misconfigured: environment variable {name} is not set"
                    ),
                }
            }
        };
        let token = token.trim();
        if token.is_empty() {
            anyhow::bail!(
                "openai-compatible provider misconfigured: credential environment variable is empty"
            );
        }
        if token.contains(['\r', '\n']) {
            anyhow::bail!(
                "openai-compatible provider misconfigured: credential contains invalid header characters"
            );
        }
        Ok(token.to_string())
    }

    fn sanitize_error_detail(&self, body: &str, credential: &str) -> String {
        let mut detail = backend_error_message(body);
        if !credential.is_empty() {
            detail = detail.replace(credential, "[REDACTED]");
        }
        let detail = sanitize_output(&detail);
        truncate_body(&detail)
    }

    async fn post_chat(
        &self,
        stream: bool,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<reqwest::Response> {
        let token = self.resolve_bearer_token()?;
        let url = self.chat_completions_url()?;
        let body = serialize_generic_chat_request(
            &self.model,
            messages,
            max_tokens,
            stream,
            response_format,
        )?;

        let mut request = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(body);

        if stream {
            request = request.header("Accept", "text/event-stream");
        } else {
            request = request.timeout(self.timeouts.request);
        }

        let response = request.send().await.map_err(|err| {
            // reqwest errors can embed URLs; never include Authorization.
            anyhow::anyhow!(
                "openai-compatible request failed: {}",
                redact_reqwest_error(&err)
            )
        })?;

        Ok(response)
    }

    async fn chat_once(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        let token = self.resolve_bearer_token()?;
        let response = self
            .post_chat(false, messages, max_tokens, response_format)
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow::anyhow!("openai-compatible response read failed: {err}"))?;
        if body.len() > DEFAULT_MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "openai-compatible response exceeded {} bytes",
                DEFAULT_MAX_RESPONSE_BYTES
            );
        }
        if !status.is_success() {
            anyhow::bail!(
                "openai-compatible {}: {}",
                status.as_u16(),
                self.sanitize_error_detail(&body, &token)
            );
        }

        let chat_resp: ChatResponse = serde_json::from_str(&body).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse openai-compatible response: {}; body: {}",
                e,
                truncate_body(&body)
            )
        })?;
        Ok(chat_resp
            .choices
            .first()
            .and_then(|c| c.message.as_ref())
            .map(|m| m.content.clone())
            .unwrap_or_default())
    }

    async fn chat_stream_once(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        let token = self.resolve_bearer_token()?;
        let response = self.post_chat(true, messages, max_tokens, None).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body = if body.len() > MAX_ERROR_BODY_BYTES {
                truncate_body(&body)
            } else {
                body
            };
            anyhow::bail!(
                "openai-compatible {}: {}",
                status.as_u16(),
                self.sanitize_error_detail(&body, &token)
            );
        }

        let mut full_response = String::new();
        let mut line_buf = String::new();
        let mut total_bytes = 0usize;
        let mut stream = response;
        loop {
            let chunk = tokio::time::timeout(self.timeouts.read, stream.chunk())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "openai-compatible stream read timed out after {}s",
                        self.timeouts.read.as_secs()
                    )
                })?
                .map_err(|err| anyhow::anyhow!("openai-compatible stream read failed: {err}"))?;
            let Some(chunk) = chunk else {
                break;
            };
            total_bytes = total_bytes.saturating_add(chunk.len());
            if total_bytes > DEFAULT_MAX_RESPONSE_BYTES {
                anyhow::bail!(
                    "openai-compatible streaming response exceeded {} bytes",
                    DEFAULT_MAX_RESPONSE_BYTES
                );
            }
            let text = String::from_utf8_lossy(&chunk);
            for ch in text.chars() {
                if ch == '\n' {
                    let line = line_buf.trim_end_matches('\r').to_string();
                    line_buf.clear();
                    if line.len() > MAX_SSE_LINE_BYTES {
                        anyhow::bail!(
                            "openai-compatible streaming line exceeded {} bytes",
                            MAX_SSE_LINE_BYTES
                        );
                    }
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            return Ok(full_response);
                        }
                        if let Ok(chunk) = serde_json::from_str::<ChatResponse>(data)
                            && let Some(choice) = chunk.choices.first()
                        {
                            if let Some(delta) = &choice.delta
                                && let Some(content) = &delta.content
                            {
                                on_token(content);
                                full_response.push_str(content);
                            }
                            if choice.finish_reason.is_some() {
                                return Ok(full_response);
                            }
                        }
                    }
                } else {
                    line_buf.push(ch);
                    if line_buf.len() > MAX_SSE_LINE_BYTES {
                        anyhow::bail!(
                            "openai-compatible streaming line exceeded {} bytes",
                            MAX_SSE_LINE_BYTES
                        );
                    }
                }
            }
        }
        Ok(full_response)
    }
}

/// Parse and validate an OpenAI-compatible base URL (http/https only).
pub(crate) fn parse_openai_compatible_base_url(url: &str) -> Result<Url> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        anyhow::bail!("openai-compatible base_url must not be empty");
    }
    let parsed = Url::parse(trimmed)
        .with_context(|| format!("invalid openai-compatible base_url: {trimmed}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => anyhow::bail!("openai-compatible base_url must use http or https (got {other})"),
    }
    if parsed.host_str().is_none() {
        anyhow::bail!("openai-compatible base_url must include a host");
    }
    Ok(parsed)
}

/// Join `{base}/chat/completions`, preserving the configured base path.
pub(crate) fn join_chat_completions(base: &Url) -> Result<Url> {
    let mut base_str = base.as_str().trim_end_matches('/').to_string();
    if base_str.is_empty() {
        anyhow::bail!("openai-compatible base_url became empty");
    }
    base_str.push_str("/chat/completions");
    Url::parse(&base_str).context("failed to join openai-compatible chat/completions URL")
}

fn redact_reqwest_error(err: &reqwest::Error) -> String {
    // Avoid dumping full debug which may include request builder state.
    let mut msg = err.to_string();
    if let Some(url) = err.url() {
        // URL is fine; strip any accidental userinfo.
        let safe = url.as_str().split('@').next_back().unwrap_or(url.as_str());
        if !msg.contains(safe) {
            msg = format!("{msg} ({safe})");
        }
    }
    sanitize_output(&msg)
}

#[async_trait]
impl LlmBackendClient for OpenAiCompatibleBackend {
    fn backend_name(&self) -> &str {
        "openai-compatible"
    }

    async fn health(&self) -> bool {
        // Optional remote providers often lack a portable /health; treat a
        // successful HEAD/GET on the base URL as best-effort reachability.
        let url = self.base_url.clone();
        matches!(
            self.http
                .get(url)
                .timeout(self.timeouts.connect.max(Duration::from_secs(2)))
                .send()
                .await,
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 404
        )
    }

    async fn chat_with_format(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        self.chat_once(messages, max_tokens, response_format).await
    }

    async fn chat_with_format_and_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
        _hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        // Generic providers ignore cache-aware hints (no nvext).
        self.chat_once(messages, max_tokens, response_format).await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        self.chat_stream_once(messages, max_tokens, on_token).await
    }

    async fn chat_stream_with_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        _hints: Option<&LlmRequestHints>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        self.chat_stream_once(messages, max_tokens, on_token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_http_and_https() {
        let http = parse_openai_compatible_base_url("http://127.0.0.1:11434/v1").unwrap();
        assert_eq!(http.scheme(), "http");
        assert_eq!(http.path(), "/v1");

        let https = parse_openai_compatible_base_url("https://api.openai.com/v1").unwrap();
        assert_eq!(https.scheme(), "https");
        assert_eq!(https.host_str(), Some("api.openai.com"));
    }

    #[test]
    fn parse_rejects_unsupported_schemes() {
        let err = parse_openai_compatible_base_url("ftp://example.com/v1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http or https"), "{err}");
    }

    #[test]
    fn join_preserves_base_path() {
        let base = parse_openai_compatible_base_url("http://127.0.0.1:11434/v1").unwrap();
        let joined = join_chat_completions(&base).unwrap();
        assert_eq!(
            joined.as_str(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );

        let with_slash = parse_openai_compatible_base_url("http://127.0.0.1:11434/v1/").unwrap();
        let joined = join_chat_completions(&with_slash).unwrap();
        assert_eq!(
            joined.as_str(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn debug_redacts_literal_credential() {
        let backend = OpenAiCompatibleBackend::from_url_with_bearer_token(
            "http://127.0.0.1:9/v1",
            "super-secret-token",
        );
        let rendered = format!("{backend:?}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
    }

    #[test]
    fn resolve_bearer_token_requires_env_value() {
        let backend = OpenAiCompatibleBackend::from_url_with_bearer_token_env_and_model(
            "http://127.0.0.1:9/v1",
            "OPENAI_COMPAT_MISSING_ENV_FOR_UNIT_TEST",
            "test-model",
            LlmTimeouts::default(),
        );
        unsafe {
            std::env::remove_var("OPENAI_COMPAT_MISSING_ENV_FOR_UNIT_TEST");
        }
        let err = backend.resolve_bearer_token().unwrap_err().to_string();
        assert!(err.contains("is not set"), "{err}");
        assert!(!err.contains("Bearer"), "{err}");
    }
}
