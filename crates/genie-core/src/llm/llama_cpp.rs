use anyhow::Result;
use async_trait::async_trait;

use super::openai_compat::{LlmTimeouts, OpenAiCompatClient, RequestProfile};
use super::{LlmBackendClient, LlmRequestHints, Message, ResponseFormat};

/// llama.cpp `--server` adapter for the OpenAI-compatible chat API.
pub struct LlamaCppBackend {
    inner: OpenAiCompatClient,
}

impl LlamaCppBackend {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            inner: OpenAiCompatClient::new("llama.cpp", host, port),
        }
    }

    pub fn from_url(url: &str) -> Self {
        Self::from_url_with_timeouts(url, LlmTimeouts::default())
    }

    pub fn from_url_with_timeouts(url: &str, timeouts: LlmTimeouts) -> Self {
        Self {
            inner: OpenAiCompatClient::from_url_with_profile_and_timeouts(
                "llama.cpp",
                url,
                RequestProfile::generic(),
                timeouts,
            ),
        }
    }
}

#[async_trait]
impl LlmBackendClient for LlamaCppBackend {
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
        let _ = hints;
        self.inner
            .chat_with_format(messages, max_tokens, response_format)
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
        let _ = hints;
        self.inner.chat_stream(messages, max_tokens, on_token).await
    }
}
