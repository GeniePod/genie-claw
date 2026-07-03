//! Runtime `Provider` seam: the swappable completion surface an agent turn
//! depends on, with the local model as the default implementation.
//!
//! Counterpart to [`super::provider`], which only validates whether an optional
//! API provider is *eligible*. Here the turn depends on `&dyn Provider` rather
//! than a concrete client, and [`LocalProvider`] wraps the on-device
//! [`LlmClient`] as the default — identical behavior to calling it directly.

use anyhow::Result;
use async_trait::async_trait;

use super::{LlmClient, LlmRequestHints, Message};

/// Source of chat completions for an agent turn.
///
/// Prompt messages in, a completion string out; parsing that completion into a
/// tool call or a plain reply stays in the agent loop and is provider-agnostic.
/// Swapping the implementation changes only how the completion is produced,
/// which is the seam the optional API-provider work plugs into.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier for the underlying model surface.
    fn provider_name(&self) -> &str;

    /// Produce a completion for `messages`, honoring the token budget and the
    /// optional cache-aware request hints.
    async fn complete(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String>;
}

/// Default [`Provider`]: the on-device local model behind [`LlmClient`].
///
/// Borrows the client so the server keeps using it for the streaming and health
/// paths that are not routed through `Provider`.
pub struct LocalProvider<'a> {
    llm: &'a LlmClient,
}

impl<'a> LocalProvider<'a> {
    pub fn new(llm: &'a LlmClient) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Provider for LocalProvider<'_> {
    fn provider_name(&self) -> &str {
        self.llm.backend_name()
    }

    async fn complete(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        self.llm
            .chat_with_format_and_hints(messages, max_tokens, None, hints)
            .await
    }
}
