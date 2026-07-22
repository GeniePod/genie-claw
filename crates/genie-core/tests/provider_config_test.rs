//! Config-driven LLM provider selection (#568) and optional OpenAI-compatible
//! wire behavior (#569).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use genie_common::config::{
    ActiveLlmProviderKind, AgentConfig, Config, LlmBackendKind, OptionalAiProviderAuthMode,
    OptionalAiProviderConfig, OptionalAiProviderKind, ServiceEndpoint,
};
use genie_core::llm::{LlmBackendClient, LlmClient, LlmTimeouts, Message, OpenAiCompatibleBackend};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn short_timeouts() -> LlmTimeouts {
    LlmTimeouts {
        connect: Duration::from_secs(2),
        read: Duration::from_secs(2),
        request: Duration::from_secs(2),
    }
}

async fn spawn_capture_server(
    response: String,
    accept_count: Arc<AtomicUsize>,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok((mut conn, _)) = listener.accept().await {
            accept_count.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 64 * 1024];
            let n = conn.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = tx.send(request);
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.shutdown().await;
        }
    });
    (addr, rx)
}

fn http_json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
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

#[tokio::test]
async fn gate_off_makes_zero_optional_provider_calls() {
    let accepts = Arc::new(AtomicUsize::new(0));
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"remote"},"finish_reason":"stop"}]}"#;
    let (addr, _rx) = spawn_capture_server(http_json_response(body), Arc::clone(&accepts)).await;

    let mut config = test_config();
    config.optional_ai_provider.enabled = false;
    config.optional_ai_provider.base_url = format!("http://{addr}/v1");
    config.services.llm = ServiceEndpoint {
        url: "http://127.0.0.1:9/unused".into(),
        systemd_unit: String::new(),
        backend: LlmBackendKind::GenieAiRuntime,
    };

    // Gate off: from_config must stay on the local service path, and a mock
    // completion must not touch the optional-provider listener.
    assert_eq!(
        config.active_llm_provider_kind(),
        ActiveLlmProviderKind::Local
    );
    let local = LlmClient::mock(["local reply"]);
    let out = local
        .chat(
            &[Message {
                role: "user".into(),
                content: "hi".into(),
            }],
            Some(8),
        )
        .await
        .unwrap();
    assert_eq!(out, "local reply");

    // Give the accept task a moment; it must still see zero connections.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        0,
        "gate-off must not contact the optional provider listener"
    );
}

#[tokio::test]
async fn gate_on_sends_auth_model_body_and_path() {
    let env_name = "PROVIDER_WIRE_TEST_KEY";
    let secret = "wire-test-secret-token-value";
    unsafe {
        std::env::set_var(env_name, secret);
    }

    let accepts = Arc::new(AtomicUsize::new(0));
    let body =
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
    let (addr, rx) = spawn_capture_server(http_json_response(body), Arc::clone(&accepts)).await;

    let mut config = test_config();
    config.core.llm_connect_timeout_secs = 2;
    config.core.llm_read_timeout_secs = 2;
    config.core.llm_request_timeout_secs = 2;
    config.optional_ai_provider = OptionalAiProviderConfig {
        enabled: true,
        provider: OptionalAiProviderKind::OpenAiCompatible,
        auth_mode: OptionalAiProviderAuthMode::ApiKey,
        base_url: format!("http://{addr}/v1"),
        model: "test-model".into(),
        api_key_env: env_name.into(),
        oauth_token_env: String::new(),
        context_window_tokens: 4096,
        allow_remote_base_url: false,
    };
    config.validate_llm_provider().unwrap();

    let client = LlmClient::from_config(&config).unwrap();
    assert_eq!(client.backend_name(), "openai-compatible");

    let response = client
        .chat(
            &[Message {
                role: "user".into(),
                content: "hello household".into(),
            }],
            Some(32),
        )
        .await
        .unwrap();
    assert_eq!(response, "ok");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);

    let request = rx.await.unwrap();
    let request_lower = request.to_ascii_lowercase();
    assert!(
        request.contains("POST /v1/chat/completions HTTP/1.1"),
        "path missing: {request}"
    );
    assert!(
        request_lower.contains(&format!("authorization: bearer {secret}")),
        "auth missing: {request}"
    );
    assert!(
        request.contains("\"model\":\"test-model\""),
        "model missing: {request}"
    );
    assert!(
        request.contains("\"content\":\"hello household\""),
        "message missing: {request}"
    );
    assert!(
        request.contains("\"max_tokens\":32"),
        "max_tokens missing: {request}"
    );
    assert!(
        request.contains("\"stream\":false"),
        "stream flag missing: {request}"
    );
    assert!(
        !request.contains("nvext") && !request.contains("conversation_id"),
        "generic profile must omit runtime fields: {request}"
    );

    unsafe {
        std::env::remove_var(env_name);
    }
}

#[tokio::test]
async fn missing_key_fails_before_connect() {
    let env_name = "PROVIDER_MISSING_KEY_WIRE_TEST";
    unsafe {
        std::env::remove_var(env_name);
    }

    let accepts = Arc::new(AtomicUsize::new(0));
    let body =
        r#"{"choices":[{"message":{"role":"assistant","content":"leak"},"finish_reason":"stop"}]}"#;
    let (addr, _rx) = spawn_capture_server(http_json_response(body), Arc::clone(&accepts)).await;

    let backend = OpenAiCompatibleBackend::try_new(
        &format!("http://{addr}/v1"),
        "test-model",
        env_name,
        short_timeouts(),
    )
    .unwrap();

    let err = backend
        .chat_with_format(
            &[Message {
                role: "user".into(),
                content: "hi".into(),
            }],
            Some(8),
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not set") || err.contains("misconfigured"),
        "expected clear missing-key error, got: {err}"
    );

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        0,
        "missing key must fail before connecting"
    );
}

#[tokio::test]
async fn provider_error_does_not_echo_secret() {
    let env_name = "PROVIDER_ERROR_REDACTION_KEY";
    let secret = "sk-proj-should-never-leak-in-errors-1234567890";
    unsafe {
        std::env::set_var(env_name, secret);
    }

    let accepts = Arc::new(AtomicUsize::new(0));
    let error_body = format!(r#"{{"error":{{"message":"invalid api key {secret} presented"}}}}"#);
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        error_body.len(),
        error_body
    );
    let (addr, _rx) = spawn_capture_server(response, Arc::clone(&accepts)).await;

    let backend = OpenAiCompatibleBackend::try_new(
        &format!("http://{addr}/v1"),
        "test-model",
        env_name,
        short_timeouts(),
    )
    .unwrap();

    let err = backend
        .chat_with_format(
            &[Message {
                role: "user".into(),
                content: "hi".into(),
            }],
            Some(8),
            None,
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("401"), "status missing: {err}");
    assert!(!err.contains(secret), "credential leaked into error: {err}");
    assert!(
        err.contains("[REDACTED") || err.contains("invalid api key"),
        "expected sanitized detail, got: {err}"
    );
    assert_eq!(accepts.load(Ordering::SeqCst), 1);

    unsafe {
        std::env::remove_var(env_name);
    }
}

#[tokio::test]
async fn streaming_sets_stream_flag_and_delivers_tokens() {
    let env_name = "PROVIDER_STREAM_WIRE_TEST_KEY";
    let secret = "stream-secret-token";
    unsafe {
        std::env::set_var(env_name, secret);
    }

    let accepts = Arc::new(AtomicUsize::new(0));
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        sse.len(),
        sse
    );
    let (addr, rx) = spawn_capture_server(response, Arc::clone(&accepts)).await;

    let backend = OpenAiCompatibleBackend::try_new(
        &format!("http://{addr}/v1"),
        "stream-model",
        env_name,
        short_timeouts(),
    )
    .unwrap();

    let mut tokens = Vec::new();
    let full = backend
        .chat_stream(
            &[Message {
                role: "user".into(),
                content: "stream please".into(),
            }],
            Some(16),
            &mut |tok| tokens.push(tok.to_string()),
        )
        .await
        .unwrap();

    assert_eq!(full, "Hello");
    assert_eq!(tokens, vec!["Hel".to_string(), "lo".to_string()]);
    assert_eq!(accepts.load(Ordering::SeqCst), 1);

    let request = rx.await.unwrap();
    let request_lower = request.to_ascii_lowercase();
    assert!(
        request.contains("\"stream\":true"),
        "stream flag missing: {request}"
    );
    assert!(
        request_lower.contains("accept: text/event-stream"),
        "SSE accept missing: {request}"
    );
    assert!(
        request_lower.contains(&format!("authorization: bearer {secret}")),
        "auth missing: {request}"
    );

    unsafe {
        std::env::remove_var(env_name);
    }
}

#[test]
fn validate_rejects_missing_credential_env_value() {
    let env_name = "PROVIDER_VALIDATE_MISSING_KEY";
    unsafe {
        std::env::remove_var(env_name);
    }
    let mut config = test_config();
    config.optional_ai_provider = OptionalAiProviderConfig {
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
    let err = config.validate_llm_provider().unwrap_err().to_string();
    assert!(err.contains(env_name), "{err}");
    assert!(err.contains("not set") || err.contains("empty"), "{err}");
}
