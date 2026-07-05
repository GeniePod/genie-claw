//! Runtime `Provider` seam: the swappable completion surface an agent turn
//! depends on, with the local model as the default implementation.
//!
//! Counterpart to [`super::provider`], which only validates whether an optional
//! API provider is *eligible*. Here the turn depends on `&dyn Provider` rather
//! than a concrete client, and [`LocalProvider`] wraps the on-device
//! [`LlmClient`] as the default — identical behavior to calling it directly.

use anyhow::Result;
use async_trait::async_trait;
use genie_common::config::{ActiveLlmProviderKind, AgentConfig, Config};

use super::provider::{OptionalProviderPlan, ProviderReadiness};
use super::{LlmClient, LlmRequestHints, Message};

/// Whether completions are allowed through the configured LLM surface.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionGate {
    Local,
    OptionalApi(OptionalProviderPlan),
}

/// [`Provider`] that enforces optional API eligibility before completing (#630).
pub struct GatedProvider<'a> {
    inner: LocalProvider<'a>,
    gate: CompletionGate,
    agent: &'a AgentConfig,
}

impl<'a> GatedProvider<'a> {
    pub fn from_config(config: &'a Config, llm: &'a LlmClient) -> Self {
        let gate = match config.active_llm_provider_kind() {
            ActiveLlmProviderKind::Local => CompletionGate::Local,
            ActiveLlmProviderKind::OptionalApi => CompletionGate::OptionalApi(
                OptionalProviderPlan::from_config(&config.optional_ai_provider)
                    .expect("optional API active implies enabled plan"),
            ),
        };
        Self {
            inner: LocalProvider::new(llm),
            gate,
            agent: &config.agent,
        }
    }

    /// Construct with an explicit optional API gate (tests and harnesses).
    pub fn with_optional_plan(
        llm: &'a LlmClient,
        plan: OptionalProviderPlan,
        agent: &'a AgentConfig,
    ) -> Self {
        Self {
            inner: LocalProvider::new(llm),
            gate: CompletionGate::OptionalApi(plan),
            agent,
        }
    }

    pub fn readiness(&self) -> ProviderReadiness {
        match &self.gate {
            CompletionGate::Local => ProviderReadiness::Ready,
            CompletionGate::OptionalApi(plan) => plan.readiness(self.agent),
        }
    }
}

/// Build the runtime completion [`Provider`] from config (#630).
pub fn gated_provider_from_config<'a>(config: &'a Config, llm: &'a LlmClient) -> GatedProvider<'a> {
    GatedProvider::from_config(config, llm)
}

#[async_trait]
impl Provider for GatedProvider<'_> {
    fn provider_name(&self) -> &str {
        self.inner.provider_name()
    }

    async fn complete(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        match &self.gate {
            CompletionGate::Local => self.inner.complete(messages, max_tokens, hints).await,
            CompletionGate::OptionalApi(plan) => {
                plan.ensure_ready(self.agent)?;
                self.inner.complete(messages, max_tokens, hints).await
            }
        }
    }
}

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
