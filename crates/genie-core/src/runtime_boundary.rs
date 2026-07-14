//! Runtime boundary contracts for the GenieClaw agent layer.
//!
//! GenieClaw owns agent policy, prompt assembly, family memory, tools,
//! channels, and audit for a low-latency private home agent. It consumes lower
//! runtime contracts for inference, voice I/O, and physical home control; it
//! should not grow into those runtimes.

use genie_common::config::{
    AgentConfig, AgentRuntimeProfile, OptionalAiProviderConfig, RuntimeBoundaryMode,
};
use serde::Serialize;

pub const JETSON_BASELINE_CONTEXT_TOKENS: u32 = 4096;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeBoundaryContract {
    pub layer: &'static str,
    pub owner: &'static str,
    pub mode: String,
    pub contract: &'static str,
    pub local_default: bool,
}

pub trait AiRuntimeBoundary {
    fn provider_name(&self) -> &str;
    fn context_window_tokens(&self) -> u32;
    fn local_default(&self) -> bool;
}

pub trait VoiceRuntimeBoundary {
    fn boundary_mode(&self) -> RuntimeBoundaryMode;
    fn owns_audio_pipeline(&self) -> bool;
}

pub trait HomeRuntimeBoundary {
    fn boundary_mode(&self) -> RuntimeBoundaryMode;
    fn owns_final_actuation(&self) -> bool;
}

pub fn runtime_boundaries(agent: &AgentConfig) -> Vec<RuntimeBoundaryContract> {
    vec![
        RuntimeBoundaryContract {
            layer: "ai_runtime",
            owner: "genie-ai-runtime",
            mode: format!("{:?}", agent.ai_runtime_boundary),
            contract: "OpenAI-compatible chat plus limited-context compaction, conversation_id, nvext.agent_hints, and system-prompt prefix cache hints",
            local_default: true,
        },
        RuntimeBoundaryContract {
            layer: "voice_runtime",
            owner: "external_voice_boundary",
            mode: format!("{:?}", agent.voice_runtime_boundary),
            contract: "transcript-in and speak-out session protocol",
            local_default: matches!(
                agent.voice_runtime_boundary,
                RuntimeBoundaryMode::ExternalRuntime | RuntimeBoundaryMode::TransitionalAdapter
            ),
        },
        RuntimeBoundaryContract {
            layer: "home_runtime",
            owner: "external_home_boundary",
            mode: format!("{:?}", agent.home_runtime_boundary),
            contract: "intent handoff plus final physical actuation gate",
            local_default: matches!(
                agent.home_runtime_boundary,
                RuntimeBoundaryMode::ExternalRuntime | RuntimeBoundaryMode::TransitionalAdapter
            ),
        },
    ]
}

pub fn profile_label(profile: AgentRuntimeProfile) -> &'static str {
    match profile {
        AgentRuntimeProfile::Jetson => "jetson",
        AgentRuntimeProfile::RaspberryPi => "raspberry_pi",
        AgentRuntimeProfile::PortableSbc => "portable_sbc",
        AgentRuntimeProfile::Laptop => "laptop",
        AgentRuntimeProfile::Mac => "mac",
    }
}

pub fn provider_respects_agent_context(
    provider: &OptionalAiProviderConfig,
    agent: &AgentConfig,
) -> bool {
    provider.limited_context_compatible(agent)
}

pub fn is_jetson_baseline(agent: &AgentConfig) -> bool {
    agent.runtime_profile == AgentRuntimeProfile::Jetson
        && agent.context_window_tokens <= JETSON_BASELINE_CONTEXT_TOKENS
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::{OptionalAiProviderAuthMode, OptionalAiProviderKind};

    #[test]
    fn default_agent_contract_is_jetson_limited_context() {
        let agent = AgentConfig::default();

        assert!(is_jetson_baseline(&agent));
        assert_eq!(profile_label(agent.runtime_profile), "jetson");
        assert_eq!(agent.context_window_tokens, JETSON_BASELINE_CONTEXT_TOKENS);
    }

    #[test]
    fn boundaries_make_genie_claw_an_agent_layer() {
        let agent = AgentConfig::default();
        let boundaries = runtime_boundaries(&agent);

        assert_eq!(boundaries.len(), 3);
        assert_eq!(boundaries[0].owner, "genie-ai-runtime");
        assert_eq!(boundaries[1].owner, "external_voice_boundary");
        assert_eq!(boundaries[2].owner, "external_home_boundary");
        assert!(boundaries.iter().all(|boundary| !boundary.layer.is_empty()));
    }

    #[test]
    fn provider_context_must_not_exceed_agent_budget() {
        let agent = AgentConfig::default();
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAi,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            oauth_token_env: "OPENAI_OAUTH_ACCESS_TOKEN".into(),
            context_window_tokens: 8192,
            allow_remote_base_url: true,
        };

        assert!(!provider_respects_agent_context(&provider, &agent));
        assert!(!provider.transitional_test_candidate(&agent));
    }
}
