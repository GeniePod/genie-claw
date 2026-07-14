//! Config-driven LLM provider selection (#568).

use genie_common::config::{
    ActiveLlmProviderKind, AgentConfig, Config, OptionalAiProviderAuthMode,
    OptionalAiProviderConfig, OptionalAiProviderKind,
};
use genie_core::llm::LlmClient;

fn test_config() -> Config {
    Config {
        data_dir: "/tmp/geniepod".into(),
        core: Default::default(),
        agent: AgentConfig::default(),
        optional_ai_provider: OptionalAiProviderConfig::default(),
        privacy_proxy: Default::default(),
        governor: Default::default(),
        health: Default::default(),
        services: Default::default(),
        telegram: Default::default(),
        web_search: Default::default(),
        connectivity: Default::default(),
        http: Default::default(),
        storage: Default::default(),
    }
}

#[test]
fn from_config_uses_local_service_by_default() {
    let config = test_config();
    assert_eq!(
        config.active_llm_provider_kind(),
        ActiveLlmProviderKind::Local
    );

    let client = LlmClient::from_config(&config).unwrap();
    assert!(client.backend_name().contains("genie-ai-runtime"));
}

#[test]
fn from_config_selects_optional_openai_compatible_provider() {
    let mut config = test_config();
    config.optional_ai_provider = OptionalAiProviderConfig {
        enabled: true,
        provider: OptionalAiProviderKind::OpenAiCompatible,
        auth_mode: OptionalAiProviderAuthMode::ApiKey,
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: "test-model".into(),
        api_key_env: "PROVIDER_CONFIG_TEST_KEY".into(),
        oauth_token_env: String::new(),
        context_window_tokens: 4096,
        allow_remote_base_url: false,
    };

    // SAFETY: single-threaded test; no concurrent env mutation.
    unsafe {
        std::env::set_var("PROVIDER_CONFIG_TEST_KEY", "test-token");
    }

    config.validate_llm_provider().unwrap();
    assert_eq!(
        config.active_llm_provider_kind(),
        ActiveLlmProviderKind::OptionalApi
    );

    let client = LlmClient::from_config(&config).unwrap();
    assert_eq!(client.backend_name(), "openai-compatible");

    unsafe {
        std::env::remove_var("PROVIDER_CONFIG_TEST_KEY");
    }
}
