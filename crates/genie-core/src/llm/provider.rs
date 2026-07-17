//! Optional API-provider planning for non-default LLM paths.
//!
//! This module intentionally does not add provider SDKs or change the local
//! Jetson default. It validates whether an API-key provider is eligible to be
//! wired behind the existing LLM facade without violating the limited-context
//! agent contract.

use genie_common::config::{
    AgentConfig, OptionalAiProviderAuthMode, OptionalAiProviderConfig, OptionalAiProviderKind,
};

use crate::security::sandbox::validate_inference_route;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderReadiness {
    Disabled,
    Ready,
    Blocked(Vec<&'static str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalProviderPlan {
    pub provider: OptionalAiProviderKind,
    pub auth_mode: OptionalAiProviderAuthMode,
    pub base_url: String,
    pub api_key_env: String,
    pub oauth_token_env: String,
    pub context_window_tokens: u32,
    pub remote_allowed: bool,
}

impl OptionalProviderPlan {
    pub fn from_config(config: &OptionalAiProviderConfig) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        Some(Self {
            provider: config.provider,
            auth_mode: config.auth_mode,
            base_url: config.base_url.clone(),
            api_key_env: config.api_key_env.clone(),
            oauth_token_env: config.oauth_token_env.clone(),
            context_window_tokens: config.context_window_tokens,
            remote_allowed: config.allow_remote_base_url,
        })
    }

    pub fn readiness(&self, agent: &AgentConfig) -> ProviderReadiness {
        let mut reasons = Vec::new();
        if self.context_window_tokens > agent.context_window_tokens {
            reasons.push("context_window_exceeds_agent_budget");
        }
        if self.credential_env().trim().is_empty() {
            reasons.push(match self.auth_mode {
                OptionalAiProviderAuthMode::ApiKey => "missing_api_key_env",
                OptionalAiProviderAuthMode::OAuthBearer => "missing_oauth_token_env",
            });
        } else {
            // Fail closed at complete-time if the operator unset/emptied the
            // credential after boot (#569). Config load already checks this;
            // readiness must too so GatedProvider cannot race a missing key.
            match std::env::var(self.credential_env()) {
                Ok(value) if !value.trim().is_empty() => {}
                Ok(_) => reasons.push(match self.auth_mode {
                    OptionalAiProviderAuthMode::ApiKey => "empty_api_key_env_value",
                    OptionalAiProviderAuthMode::OAuthBearer => "empty_oauth_token_env_value",
                }),
                Err(_) => reasons.push(match self.auth_mode {
                    OptionalAiProviderAuthMode::ApiKey => "api_key_env_unset",
                    OptionalAiProviderAuthMode::OAuthBearer => "oauth_token_env_unset",
                }),
            }
        }
        if self.base_url.trim().is_empty() {
            reasons.push("missing_base_url");
        }
        if remote_url(&self.base_url) && !self.remote_allowed {
            reasons.push("remote_base_url_not_allowed");
        }

        if reasons.is_empty() {
            ProviderReadiness::Ready
        } else {
            ProviderReadiness::Blocked(reasons)
        }
    }

    pub fn credential_env(&self) -> &str {
        match self.auth_mode {
            OptionalAiProviderAuthMode::ApiKey => &self.api_key_env,
            OptionalAiProviderAuthMode::OAuthBearer => &self.oauth_token_env,
        }
    }

    /// Fail-loud check used at config load when `[optional_ai_provider].enabled`.
    pub fn ensure_ready(&self, agent: &AgentConfig) -> anyhow::Result<()> {
        match self.readiness(agent) {
            ProviderReadiness::Ready => Ok(()),
            ProviderReadiness::Disabled => {
                anyhow::bail!("optional AI provider plan is disabled")
            }
            ProviderReadiness::Blocked(reasons) => {
                let details = reasons
                    .iter()
                    .map(|reason| blocked_reason_message(reason))
                    .collect::<Vec<_>>()
                    .join("; ");
                anyhow::bail!("optional_ai_provider misconfigured: {details}")
            }
        }
    }
}

fn blocked_reason_message(reason: &str) -> String {
    match reason {
        "context_window_exceeds_agent_budget" => {
            "[optional_ai_provider].context_window_tokens exceeds [agent].context_window_tokens"
                .into()
        }
        "missing_api_key_env" => {
            "[optional_ai_provider].api_key_env must be set when auth_mode = api_key".into()
        }
        "missing_oauth_token_env" => {
            "[optional_ai_provider].oauth_token_env must be set when auth_mode = oauth_bearer"
                .into()
        }
        "api_key_env_unset" => {
            "optional_ai_provider enabled but api_key_env environment variable is not set".into()
        }
        "oauth_token_env_unset" => {
            "optional_ai_provider enabled but oauth_token_env environment variable is not set"
                .into()
        }
        "empty_api_key_env_value" => {
            "optional_ai_provider enabled but api_key_env environment variable is empty".into()
        }
        "empty_oauth_token_env_value" => {
            "optional_ai_provider enabled but oauth_token_env environment variable is empty".into()
        }
        "missing_base_url" => "[optional_ai_provider].base_url must be set when enabled".into(),
        "remote_base_url_not_allowed" => {
            "[optional_ai_provider].base_url is remote; set allow_remote_base_url = true to opt in"
                .into()
        }
        _ => reason.to_string(),
    }
}

fn remote_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    validate_inference_route(url).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_provider_has_no_plan() {
        assert!(OptionalProviderPlan::from_config(&OptionalAiProviderConfig::default()).is_none());
    }

    fn with_env(name: &str, value: &str) {
        // SAFETY: unit tests run single-threaded and restore/remove keys they set.
        unsafe {
            std::env::set_var(name, value);
        }
    }

    fn clear_env(name: &str) {
        unsafe {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn remote_provider_requires_explicit_allow_and_budget_fit() {
        with_env("GENIE_PROVIDER_KEY", "test-token");
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "https://provider.example/v1".into(),
            model: "test-model".into(),
            api_key_env: "GENIE_PROVIDER_KEY".into(),
            oauth_token_env: "GENIE_PROVIDER_OAUTH_TOKEN".into(),
            context_window_tokens: 8192,
            allow_remote_base_url: false,
        };
        let plan = OptionalProviderPlan::from_config(&provider).unwrap();

        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Blocked(vec![
                "context_window_exceeds_agent_budget",
                "remote_base_url_not_allowed"
            ])
        );
        clear_env("GENIE_PROVIDER_KEY");
    }

    #[test]
    fn local_openai_compatible_provider_can_be_ready() {
        with_env("LOCAL_PROVIDER_KEY", "test-token");
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "test-model".into(),
            api_key_env: "LOCAL_PROVIDER_KEY".into(),
            oauth_token_env: "LOCAL_PROVIDER_OAUTH_TOKEN".into(),
            context_window_tokens: 4096,
            allow_remote_base_url: false,
        };
        let plan = OptionalProviderPlan::from_config(&provider).unwrap();

        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Ready
        );
        clear_env("LOCAL_PROVIDER_KEY");
    }

    #[test]
    fn loopback_127_range_allowed_without_remote_flag() {
        with_env("LOCAL_PROVIDER_KEY", "test-token");
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "http://127.0.0.2:11434/v1".into(),
            model: "test-model".into(),
            api_key_env: "LOCAL_PROVIDER_KEY".into(),
            oauth_token_env: "LOCAL_PROVIDER_OAUTH_TOKEN".into(),
            context_window_tokens: 4096,
            allow_remote_base_url: false,
        };
        let plan = OptionalProviderPlan::from_config(&provider).unwrap();

        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Ready
        );
        clear_env("LOCAL_PROVIDER_KEY");
    }

    #[test]
    fn loopback_looking_hostname_requires_remote_allow() {
        with_env("LOCAL_PROVIDER_KEY", "test-token");
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "http://127.evil.com:11434/v1".into(),
            model: "test-model".into(),
            api_key_env: "LOCAL_PROVIDER_KEY".into(),
            oauth_token_env: "LOCAL_PROVIDER_OAUTH_TOKEN".into(),
            context_window_tokens: 4096,
            allow_remote_base_url: false,
        };
        let plan = OptionalProviderPlan::from_config(&provider).unwrap();

        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Blocked(vec!["remote_base_url_not_allowed"])
        );
        clear_env("LOCAL_PROVIDER_KEY");
    }

    #[test]
    fn oauth_provider_uses_oauth_token_env_for_readiness() {
        with_env("OPENAI_OAUTH_ACCESS_TOKEN", "oauth-token");
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAi,
            auth_mode: OptionalAiProviderAuthMode::OAuthBearer,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            api_key_env: String::new(),
            oauth_token_env: "OPENAI_OAUTH_ACCESS_TOKEN".into(),
            context_window_tokens: 4096,
            allow_remote_base_url: true,
        };
        let plan = OptionalProviderPlan::from_config(&provider).unwrap();

        assert_eq!(plan.credential_env(), "OPENAI_OAUTH_ACCESS_TOKEN");
        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Ready
        );
        clear_env("OPENAI_OAUTH_ACCESS_TOKEN");
    }

    #[test]
    fn readiness_fails_closed_when_credential_env_disappears() {
        let env_name = "GATED_PROVIDER_RUNTIME_CREDS";
        with_env(env_name, "present");
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "test-model".into(),
            api_key_env: env_name.into(),
            oauth_token_env: String::new(),
            context_window_tokens: 4096,
            allow_remote_base_url: false,
        };
        let plan = OptionalProviderPlan::from_config(&provider).unwrap();
        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Ready
        );

        clear_env(env_name);
        assert_eq!(
            plan.readiness(&AgentConfig::default()),
            ProviderReadiness::Blocked(vec!["api_key_env_unset"])
        );
    }
}
