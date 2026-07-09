mod genie_ai_runtime;
mod llama_cpp;
mod local_provider;
mod mock;
mod openai_compat;
mod openai_compatible;
pub mod privacy_proxy;
mod provider;

use anyhow::Result;
use async_trait::async_trait;
use genie_common::config::{
    ActiveLlmProviderKind, Config, LlmBackendKind, OptionalAiProviderKind, ServiceEndpoint,
};

pub use genie_ai_runtime::GenieAiRuntimeBackend;
pub use llama_cpp::LlamaCppBackend;
pub use local_provider::{GatedProvider, LocalProvider, Provider, gated_provider_from_config};
pub use mock::MockLlmBackend;
pub use openai_compat::{LlmTimeouts, Message, ResponseFormat};
pub use openai_compatible::OpenAiCompatibleBackend;
pub use privacy_proxy::PrivacyProxyBackend;
pub use provider::{OptionalProviderPlan, ProviderReadiness};

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

    async fn chat_with_format_and_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        let _ = hints;
        self.chat_with_format(messages, max_tokens, response_format)
            .await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String>;

    async fn chat_stream_with_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: Option<&LlmRequestHints>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        let _ = hints;
        self.chat_stream(messages, max_tokens, on_token).await
    }
}

/// Harness-visible inference metadata for cache-aware local runtimes.
///
/// This mirrors the small Dynamo-style surface that matters to GenieClaw:
/// stable session identity, expected output length, request priority, and
/// short-lived cache retention. Generic backends ignore it; `genie-ai-runtime`
/// serializes it as `conversation_id` plus `nvext.agent_hints`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LlmRequestHints {
    pub session_id: Option<String>,
    pub priority: Option<i32>,
    pub output_sequence_length: Option<u32>,
    pub speculative_prefill: bool,
    pub cache_ttl_secs: Option<u32>,
}

impl LlmRequestHints {
    pub fn agent_turn(session_id: impl Into<String>, output_sequence_length: u32) -> Self {
        Self {
            session_id: Some(session_id.into()),
            priority: Some(50),
            output_sequence_length: Some(output_sequence_length),
            speculative_prefill: false,
            cache_ttl_secs: Some(15 * 60),
        }
    }

    pub fn tool_summary(session_id: impl Into<String>, output_sequence_length: u32) -> Self {
        Self {
            session_id: Some(session_id.into()),
            priority: Some(30),
            output_sequence_length: Some(output_sequence_length),
            speculative_prefill: false,
            cache_ttl_secs: Some(2 * 60),
        }
    }
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
        Self::from_service_config_with_timeouts(service, LlmTimeouts::default())
    }

    /// Build the configured backend with explicit network timeouts.
    ///
    /// Production callers thread the operator-configured timeouts in here so
    /// every chat call site (web, OpenAI bridge, voice, REPL, Telegram)
    /// inherits a bounded read — the fix for issue #181, where an unbounded
    /// read could hold the chat turn lock forever.
    pub fn from_service_config_with_timeouts(
        service: &ServiceEndpoint,
        timeouts: LlmTimeouts,
    ) -> Self {
        match service.backend {
            LlmBackendKind::LlamaCpp => {
                Self::from_llama_cpp_url_with_timeouts(&service.url, timeouts)
            }
            LlmBackendKind::GenieAiRuntime => {
                Self::from_genie_ai_runtime_url_with_timeouts(&service.url, timeouts)
            }
        }
    }

    /// Build the active LLM client from `geniepod.toml` (#568).
    ///
    /// When `[optional_ai_provider].enabled = true`, routes through the gated
    /// OpenAI-compatible API surface; otherwise uses `[services.llm]`.
    pub fn from_config(config: &Config) -> Result<Self> {
        let timeouts = LlmTimeouts::from_secs(
            config.core.llm_connect_timeout_secs,
            config.core.llm_read_timeout_secs,
            config.core.llm_request_timeout_secs,
        );

        match config.active_llm_provider_kind() {
            ActiveLlmProviderKind::Local => Ok(Self::from_service_config_with_timeouts(
                &config.services.llm,
                timeouts,
            )),
            ActiveLlmProviderKind::OptionalApi => {
                let provider = &config.optional_ai_provider;
                match provider.provider {
                    OptionalAiProviderKind::OpenAiCompatible | OptionalAiProviderKind::OpenAi => {
                        Ok(
                            Self::from_openai_compatible_url_with_bearer_token_env_and_timeouts(
                                provider.base_url.trim(),
                                provider.credential_env(),
                                timeouts,
                            ),
                        )
                    }
                    OptionalAiProviderKind::Anthropic
                    | OptionalAiProviderKind::Gemini
                    | OptionalAiProviderKind::Custom => unreachable!(
                        "unsupported optional_ai_provider kinds are rejected at config load"
                    ),
                }
            }
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

    pub fn from_llama_cpp_url_with_timeouts(url: &str, timeouts: LlmTimeouts) -> Self {
        Self {
            backend: Box::new(LlamaCppBackend::from_url_with_timeouts(url, timeouts)),
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

    pub fn from_genie_ai_runtime_url_with_timeouts(url: &str, timeouts: LlmTimeouts) -> Self {
        Self {
            backend: Box::new(GenieAiRuntimeBackend::from_url_with_timeouts(url, timeouts)),
        }
    }

    pub fn from_openai_compatible_url_with_bearer_token(url: &str, token: impl AsRef<str>) -> Self {
        Self::from_openai_compatible_url_with_bearer_token_and_timeouts(
            url,
            token,
            LlmTimeouts::default(),
        )
    }

    pub fn from_openai_compatible_url_with_bearer_token_and_timeouts(
        url: &str,
        token: impl AsRef<str>,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self {
            backend: Box::new(
                OpenAiCompatibleBackend::from_url_with_bearer_token_and_timeouts(
                    url, token, timeouts,
                ),
            ),
        }
    }

    pub fn from_openai_compatible_url_with_bearer_token_env(
        url: &str,
        env_var: impl AsRef<str>,
    ) -> Self {
        Self::from_openai_compatible_url_with_bearer_token_env_and_timeouts(
            url,
            env_var,
            LlmTimeouts::default(),
        )
    }

    pub fn from_openai_compatible_url_with_bearer_token_env_and_timeouts(
        url: &str,
        env_var: impl AsRef<str>,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self {
            backend: Box::new(
                OpenAiCompatibleBackend::from_url_with_bearer_token_env_and_timeouts(
                    url, env_var, timeouts,
                ),
            ),
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

    /// Build a client that routes through the on-device PrivacyProxy.
    ///
    /// `base_url` must be a localhost endpoint (e.g. `"http://127.0.0.1:8180/v1"`).
    /// The caller is responsible for pre-seeding the proxy's vocabulary via
    /// [`PrivacyProxyBackend::seed_vocab`] before the first chat call.
    pub fn from_privacy_proxy_url(base_url: &str, vocab_path: &str) -> Self {
        Self {
            backend: Box::new(PrivacyProxyBackend::from_url(base_url, vocab_path)),
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

    pub async fn chat_with_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: &LlmRequestHints,
    ) -> Result<String> {
        self.chat_with_format_and_hints(messages, max_tokens, None, Some(hints))
            .await
    }

    pub async fn chat_with_format_and_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        self.backend
            .chat_with_format_and_hints(messages, max_tokens, response_format, hints)
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

    pub async fn chat_stream_with_hints<F>(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: &LlmRequestHints,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        self.backend
            .chat_stream_with_hints(messages, max_tokens, Some(hints), &mut on_token)
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
    fn privacy_proxy_client_has_correct_backend_name() {
        let client = LlmClient::from_privacy_proxy_url("http://127.0.0.1:8180/v1", "/vocab/seed");
        assert_eq!(client.backend_name(), "privacy-proxy");
    }

    #[test]
    fn can_construct_openai_compatible_bearer_client() {
        let client = LlmClient::from_openai_compatible_url_with_bearer_token(
            "http://127.0.0.1:8080/v1",
            "oauth-token",
        );
        assert_eq!(client.backend_name(), "openai-compatible");
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
