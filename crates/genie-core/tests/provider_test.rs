//! Runtime `Provider` seam (issue #567) and gated optional API completions (#630).

use genie_common::config::{
    ActiveLlmProviderKind, AgentConfig, Config, OptionalAiProviderAuthMode,
    OptionalAiProviderConfig, OptionalAiProviderKind,
};
use genie_core::llm::{
    GatedProvider, LlmClient, LocalProvider, Message, OptionalProviderPlan, Provider,
    ProviderReadiness, gated_provider_for_http, gated_provider_from_config,
};

fn user(text: &str) -> Message {
    Message {
        role: "user".into(),
        content: text.into(),
    }
}

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

#[tokio::test]
async fn local_provider_delegates_completion_to_the_wrapped_client() {
    let llm = LlmClient::mock(["scripted reply"]);
    let provider = LocalProvider::new(&llm);

    let out = provider
        .complete(&[user("hi")], Some(64), None)
        .await
        .unwrap();

    assert_eq!(out, "scripted reply");
    assert_eq!(provider.provider_name(), "mock");
}

#[tokio::test]
async fn local_provider_is_usable_as_a_trait_object() {
    let llm = LlmClient::mock(["dyn reply"]);
    let local = LocalProvider::new(&llm);
    let provider: &dyn Provider = &local;

    let out = provider.complete(&[user("hi")], None, None).await.unwrap();

    assert_eq!(out, "dyn reply");
}

#[tokio::test]
async fn gated_provider_gate_off_uses_local_mock_without_optional_plan() {
    let config = test_config();
    assert_eq!(
        config.active_llm_provider_kind(),
        ActiveLlmProviderKind::Local
    );

    let llm = LlmClient::mock(["local only"]);
    let provider = gated_provider_from_config(&config, &llm);
    assert_eq!(provider.readiness(), ProviderReadiness::Ready);

    let out = provider.complete(&[user("hi")], None, None).await.unwrap();
    assert_eq!(out, "local only");
    assert_eq!(provider.provider_name(), "mock");
}

#[tokio::test]
async fn gated_provider_gate_on_completes_when_plan_is_ready() {
    let agent = AgentConfig::default();
    let plan = OptionalProviderPlan {
        provider: OptionalAiProviderKind::OpenAiCompatible,
        auth_mode: OptionalAiProviderAuthMode::ApiKey,
        base_url: "http://127.0.0.1:11434/v1".into(),
        api_key_env: "GATED_PROVIDER_TEST_KEY".into(),
        oauth_token_env: String::new(),
        context_window_tokens: 4096,
        remote_allowed: false,
    };
    assert_eq!(plan.readiness(&agent), ProviderReadiness::Ready);

    let llm = LlmClient::mock(["optional api path"]);
    let provider = GatedProvider::with_optional_plan(&llm, plan, &agent);

    let out = provider.complete(&[user("hi")], None, None).await.unwrap();
    assert_eq!(out, "optional api path");
}

#[tokio::test]
async fn gated_provider_key_missing_returns_clear_error() {
    let agent = AgentConfig::default();
    let plan = OptionalProviderPlan {
        provider: OptionalAiProviderKind::OpenAiCompatible,
        auth_mode: OptionalAiProviderAuthMode::ApiKey,
        base_url: "http://127.0.0.1:11434/v1".into(),
        api_key_env: String::new(),
        oauth_token_env: String::new(),
        context_window_tokens: 4096,
        remote_allowed: false,
    };
    assert!(matches!(
        plan.readiness(&agent),
        ProviderReadiness::Blocked(_)
    ));

    let llm = LlmClient::mock(["should not run"]);
    let provider = GatedProvider::with_optional_plan(&llm, plan, &agent);

    let err = provider
        .complete(&[user("hi")], None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("optional_ai_provider misconfigured"),
        "expected clear config error, got: {err}"
    );
    assert!(err.contains("api_key_env"));
}

#[test]
fn gated_provider_for_http_defaults_to_local_gate() {
    let llm = LlmClient::mock(["unused"]);
    let agent = AgentConfig::default();
    let optional = OptionalAiProviderConfig::default();
    let provider = gated_provider_for_http(&llm, &agent, &optional);
    assert_eq!(provider.readiness(), ProviderReadiness::Ready);
}
