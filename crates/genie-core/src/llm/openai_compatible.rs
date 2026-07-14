use anyhow::Result;
use async_trait::async_trait;

use super::openai_compat::{LlmTimeouts, OpenAiCompatClient, RequestProfile};
use super::{LlmBackendClient, LlmRequestHints, Message, ResponseFormat};

/// Generic OpenAI-compatible adapter for API providers that authenticate with
/// bearer tokens, including OAuth access tokens.
pub struct OpenAiCompatibleBackend {
    inner: OpenAiCompatClient,
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
        Self {
            inner: OpenAiCompatClient::from_url_with_profile_and_timeouts(
                "openai-compatible",
                url,
                RequestProfile::generic(),
                timeouts,
            )
            .with_bearer_token(token),
        }
    }

    pub fn from_url_with_bearer_token_env(url: &str, env_var: impl AsRef<str>) -> Self {
        Self::from_url_with_bearer_token_env_and_timeouts(url, env_var, LlmTimeouts::default())
    }

    pub fn from_url_with_bearer_token_env_and_timeouts(
        url: &str,
        env_var: impl AsRef<str>,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self {
            inner: OpenAiCompatClient::from_url_with_profile_and_timeouts(
                "openai-compatible",
                url,
                RequestProfile::generic(),
                timeouts,
            )
            .with_bearer_token_env(env_var),
        }
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
        Self {
            inner: OpenAiCompatClient::from_url_with_profile_and_timeouts(
                "openai-compatible",
                url,
                RequestProfile::generic_with_model(model),
                timeouts,
            )
            .with_bearer_token_env(env_var),
        }
    }
}

#[async_trait]
impl LlmBackendClient for OpenAiCompatibleBackend {
    fn backend_name(&self) -> &str {
        self.inner.backend_name()
    }

    async fn health(&self) -> bool {
        self.inner.health().await
    }

    async fn chat_with_format(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        self.inner
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
        self.inner
            .chat_with_format_and_hints(messages, max_tokens, response_format, hints)
            .await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        self.inner.chat_stream(messages, max_tokens, on_token).await
    }

    async fn chat_stream_with_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: Option<&LlmRequestHints>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        self.inner
            .chat_stream_with_hints(messages, max_tokens, hints, on_token)
            .await
    }
}
