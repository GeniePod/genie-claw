mod genie_ai_runtime;
mod llama_cpp;
mod mock;
mod openai_compat;
mod retry;

use anyhow::Result;
use async_trait::async_trait;
use genie_common::config::{LlmBackendKind, ServiceEndpoint};

pub use genie_ai_runtime::GenieAiRuntimeBackend;
pub use llama_cpp::LlamaCppBackend;
pub use mock::MockLlmBackend;
pub use openai_compat::{Message, ResponseFormat};
#[allow(unused_imports)]
pub use retry::RetryLlmClient;

#[async_trait]
pub trait LlmBackendClient: Send + Sync {
    fn backend_name(&self) -> &str;

    async fn health(&self) -> bool;

    async fn chat_with_format(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String>;

    async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String>;
}

/// LLM client facade used by agent orchestration.
///
/// The public constructors preserve the legacy llama.cpp default. Backend
/// selection from config is available through [`LlmClient::from_service_config`].
pub struct LlmClient {
    backend: Box<dyn LlmBackendClient>,
}

impl LlmClient {
    pub fn new(host: &str, port: u16) -> Self {
        Self::llama_cpp(host, port)
    }

    pub fn from_url(url: &str) -> Self {
        Self::from_llama_cpp_url(url)
    }

    pub fn from_service_config(service: &ServiceEndpoint) -> Self {
        match service.backend {
            LlmBackendKind::LlamaCpp => Self::from_llama_cpp_url(&service.url),
            LlmBackendKind::GenieAiRuntime => Self::from_genie_ai_runtime_url(&service.url),
        }
    }

    pub fn llama_cpp(host: &str, port: u16) -> Self {
        Self {
            backend: Box::new(LlamaCppBackend::new(host, port)),
        }
    }

    pub fn from_llama_cpp_url(url: &str) -> Self {
        Self {
            backend: Box::new(LlamaCppBackend::from_url(url)),
        }
    }

    pub fn genie_ai_runtime(host: &str, port: u16) -> Self {
        Self {
            backend: Box::new(GenieAiRuntimeBackend::new(host, port)),
        }
    }

    pub fn from_genie_ai_runtime_url(url: &str) -> Self {
        Self {
            backend: Box::new(GenieAiRuntimeBackend::from_url(url)),
        }
    }

    /// Construct an in-memory LLM client that replays the given scripted
    /// replies in order. Used by `tests/voice_loop_integration.rs` (issue #21).
    pub fn mock<I, S>(replies: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            backend: Box::new(MockLlmBackend::new(replies)),
        }
    }

    /// Wrap any `LlmBackendClient` implementation in a client facade.
    /// Primarily useful in tests that need a custom backend (e.g. a slow
    /// streaming backend for client-disconnect smoke tests).
    pub fn from_backend(backend: impl LlmBackendClient + 'static) -> Self {
        Self {
            backend: Box::new(backend),
        }
    }

    pub fn backend_name(&self) -> &str {
        self.backend.backend_name()
    }

    pub async fn health(&self) -> bool {
        self.backend.health().await
    }

    pub async fn chat(&self, messages: &[Message], max_tokens: Option<u32>) -> Result<String> {
        self.chat_with_format(messages, max_tokens, None).await
    }

    pub async fn chat_json(&self, messages: &[Message], max_tokens: Option<u32>) -> Result<String> {
        self.chat_with_format(messages, max_tokens, Some(ResponseFormat::json()))
            .await
    }

    pub async fn chat_with_format(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        self.backend
            .chat_with_format(messages, max_tokens, response_format)
            .await
    }

    pub async fn chat_stream<F>(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        self.backend
            .chat_stream(messages, max_tokens, &mut on_token)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_constructor_keeps_llama_cpp_default() {
        let client = LlmClient::from_url("http://127.0.0.1:8080/health");
        assert_eq!(client.backend_name(), "llama.cpp");
    }

    #[test]
    fn can_construct_genie_ai_runtime_client() {
        let client = LlmClient::from_genie_ai_runtime_url("http://127.0.0.1:8080/health");
        assert_eq!(client.backend_name(), "genie-ai-runtime");
    }

    #[test]
    fn service_config_selects_genie_ai_runtime_backend() {
        let client = LlmClient::from_service_config(&ServiceEndpoint {
            url: "http://127.0.0.1:8080/health".into(),
            systemd_unit: "genie-ai-runtime.service".into(),
            backend: LlmBackendKind::GenieAiRuntime,
        });

        assert_eq!(client.backend_name(), "genie-ai-runtime");
    }
}
