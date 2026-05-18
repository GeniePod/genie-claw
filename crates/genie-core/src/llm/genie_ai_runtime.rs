use anyhow::Result;
use async_trait::async_trait;

use super::openai_compat::{OpenAiCompatClient, RequestProfile};
use super::{LlmBackendClient, Message, ResponseFormat};

/// Adapter for the `genie-ai-runtime` OpenAI-compatible chat API surface.
pub struct GenieAiRuntimeBackend {
    inner: OpenAiCompatClient,
}

impl GenieAiRuntimeBackend {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            inner: OpenAiCompatClient::new_with_profile(
                "genie-ai-runtime",
                host,
                port,
                RequestProfile::genie_ai_runtime(),
            ),
        }
    }

    pub fn from_url(url: &str) -> Self {
        Self {
            inner: OpenAiCompatClient::from_url_with_profile(
                "genie-ai-runtime",
                url,
                RequestProfile::genie_ai_runtime(),
            ),
        }
    }
}

#[async_trait]
impl LlmBackendClient for GenieAiRuntimeBackend {
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

    async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        self.inner.chat_stream(messages, max_tokens, on_token).await
    }
}
