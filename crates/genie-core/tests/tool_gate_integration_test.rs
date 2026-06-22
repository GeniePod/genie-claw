//! M1 exit integration test for issue #112.
//!
//! Exercises the dispatcher boundary end-to-end with a fake home provider so
//! origin ACLs, actuation rate limits, confirmation tokens, and audit logs are
//! proven without a real Home Assistant instance.

use async_trait::async_trait;
use genie_common::config::{ActuationSafetyConfig, ToolPolicyConfig, WebSearchConfig};
use genie_core::ha::{
    ActionResult, DeviceRef, HomeAction, HomeActionKind, HomeAutomationProvider, HomeGraph,
    HomeState, HomeTarget, HomeTargetKind, IntegrationHealth, SceneRef,
};
use genie_core::tools::{RequestOrigin, ToolCall, ToolDispatcher, ToolExecutionContext};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static TEST_RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TestAuditPaths {
    data_dir: PathBuf,
    tool_audit: PathBuf,
    actuation_audit: PathBuf,
}

impl TestAuditPaths {
    fn new() -> Self {
        let run = TEST_RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
        let data_dir =
            std::env::temp_dir().join(format!("genie-tool-gate-it-{}-{}", std::process::id(), run));
        let _ = std::fs::remove_dir_all(&data_dir);
        Self {
            tool_audit: data_dir.join("runtime/tool-audit.jsonl"),
            actuation_audit: data_dir.join("safety/actuation-audit.jsonl"),
            data_dir,
        }
    }

    fn dispatcher(
        &self,
        ha: Option<Arc<dyn HomeAutomationProvider>>,
        tool_policy: ToolPolicyConfig,
        actuation_safety: ActuationSafetyConfig,
    ) -> ToolDispatcher {
        ToolDispatcher::new(ha)
            .with_tool_policy_config(tool_policy)
            .with_actuation_safety_config(actuation_safety)
            .with_tool_audit_path(self.tool_audit.clone())
            .with_actuation_audit_path(self.actuation_audit.clone())
    }
}

impl Drop for TestAuditPaths {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line.trim())
                .unwrap_or_else(|err| panic!("audit line must be valid JSON: {err}\n{line:?}"))
        })
        .collect()
}

fn assert_append_only(path: &Path, previous_len: usize) {
    let lines = read_jsonl(path);
    assert!(
        lines.len() >= previous_len,
        "audit log at {} shrank from {previous_len} to {} lines",
        path.display(),
        lines.len()
    );
}

struct FakeHomeProvider {
    executed: Arc<Mutex<Vec<HomeActionKind>>>,
    entity_id: &'static str,
    domain: &'static str,
    area: &'static str,
    confidence: f32,
    voice_safe: bool,
}

impl FakeHomeProvider {
    fn light(executed: Arc<Mutex<Vec<HomeActionKind>>>) -> Self {
        Self {
            executed,
            entity_id: "light.kitchen",
            domain: "light",
            area: "Kitchen",
            confidence: 0.96,
            voice_safe: true,
        }
    }

    fn lock(executed: Arc<Mutex<Vec<HomeActionKind>>>) -> Self {
        Self {
            executed,
            entity_id: "lock.front_door",
            domain: "lock",
            area: "Entry",
            confidence: 0.95,
            voice_safe: false,
        }
    }
}

#[async_trait]
impl HomeAutomationProvider for FakeHomeProvider {
    async fn health(&self) -> IntegrationHealth {
        IntegrationHealth {
            connected: true,
            cached_graph: true,
            message: "ok".into(),
        }
    }

    async fn sync_structure(&self) -> anyhow::Result<HomeGraph> {
        anyhow::bail!("unused in tool gate integration test")
    }

    async fn resolve_target(
        &self,
        query: &str,
        _action_hint: Option<HomeActionKind>,
    ) -> anyhow::Result<HomeTarget> {
        Ok(HomeTarget {
            kind: HomeTargetKind::Entity,
            query: query.into(),
            display_name: query.into(),
            entity_ids: vec![self.entity_id.into()],
            domain: Some(self.domain.into()),
            area: Some(self.area.into()),
            confidence: self.confidence,
            voice_safe: self.voice_safe,
        })
    }

    async fn get_state(&self, target: &HomeTarget) -> anyhow::Result<HomeState> {
        Ok(HomeState {
            target_name: target.display_name.clone(),
            domain: target.domain.clone(),
            area: target.area.clone(),
            entities: Vec::new(),
            available: true,
            spoken_summary: format!("{} is available", target.display_name),
        })
    }

    async fn execute(&self, action: HomeAction) -> anyhow::Result<ActionResult> {
        self.executed.lock().unwrap().push(action.kind);
        Ok(ActionResult {
            success: true,
            spoken_summary: format!("Executed {:?}", action.kind),
            affected_targets: vec![action.target.display_name],
            state_snapshot: None,
            confidence: Some(action.target.confidence),
        })
    }

    async fn list_scenes(&self, _room: Option<&str>) -> anyhow::Result<Vec<SceneRef>> {
        Ok(Vec::new())
    }

    async fn list_devices(&self, _room: Option<&str>) -> anyhow::Result<Vec<DeviceRef>> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn tool_gate_acl_denies_disallowed_origin_and_audits() {
    let paths = TestAuditPaths::new();
    let mut policy = ToolPolicyConfig::default();
    policy
        .denied_tools_by_origin
        .insert("telegram".into(), vec!["get_time".into()]);

    let dispatcher = paths.dispatcher(None, policy, ActuationSafetyConfig::default());
    let result = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "get_time".into(),
                arguments: serde_json::json!({}),
            },
            ToolExecutionContext {
                request_origin: RequestOrigin::Telegram,
                ..ToolExecutionContext::default()
            },
        )
        .await;

    assert!(!result.success);
    assert!(
        result.output.contains("origin policy"),
        "expected ACL refusal, got: {}",
        result.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.len(), 1, "denied tool call must be audited");
    assert_eq!(events[0]["tool"], "get_time");
    assert_eq!(events[0]["origin"], "telegram");
    assert_eq!(events[0]["success"], false);
    assert_append_only(&paths.tool_audit, 1);
}

#[tokio::test]
async fn tool_gate_rate_limit_allows_n_then_denies_and_audits() {
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut safety = ActuationSafetyConfig::default();
    safety
        .max_actions_per_minute_by_origin
        .insert("dashboard".into(), 1);

    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::light(executed.clone()))),
        ToolPolicyConfig::default(),
        safety,
    );
    let call = ToolCall {
        name: "home_control".into(),
        arguments: serde_json::json!({
            "entity": "kitchen light",
            "action": "turn_on"
        }),
    };
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let first = dispatcher.execute_with_context(&call, ctx).await;
    let second = dispatcher.execute_with_context(&call, ctx).await;

    assert!(
        first.success,
        "first call should be inside the rate limit: {}",
        first.output
    );
    assert!(!second.success, "second call must be rate-limited");
    assert!(second.output.contains("rate limit"));
    assert_eq!(
        *executed.lock().unwrap(),
        vec![HomeActionKind::TurnOn],
        "only one physical action should execute"
    );

    let tool_events = read_jsonl(&paths.tool_audit);
    assert_eq!(tool_events.len(), 2, "both dispatch attempts are audited");
    assert_eq!(tool_events[0]["success"], true);
    assert_eq!(tool_events[1]["success"], false);

    let actuation_events = read_jsonl(&paths.actuation_audit);
    let statuses: Vec<_> = actuation_events
        .iter()
        .map(|event| event["status"].as_str().unwrap())
        .collect();
    assert!(
        statuses.contains(&"executed"),
        "allowed action must be in actuation audit: {statuses:?}"
    );
    assert!(
        statuses.contains(&"blocked_runtime"),
        "rate-limited action must be in actuation audit: {statuses:?}"
    );
    assert_append_only(&paths.actuation_audit, actuation_events.len());
}

#[tokio::test]
async fn tool_gate_confirmation_token_refused_without_pending() {
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::lock(executed.clone()))),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );

    let err = dispatcher
        .confirm_pending_home_action("act-deadbeef-no-pending")
        .await
        .expect_err("unknown confirmation token must be refused");

    assert!(
        err.to_string()
            .contains("unknown or expired confirmation token")
    );
    assert!(
        executed.lock().unwrap().is_empty(),
        "confirm without pending token must not execute"
    );
}

#[tokio::test]
async fn tool_gate_confirmable_home_action_requires_token_and_audits() {
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::lock(executed.clone()))),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );

    let result = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "home_control".into(),
                arguments: serde_json::json!({
                    "entity": "front door",
                    "action": "unlock"
                }),
            },
            ToolExecutionContext {
                request_origin: RequestOrigin::Dashboard,
                ..ToolExecutionContext::default()
            },
        )
        .await;

    assert!(
        result.success,
        "confirmation-required path returns guidance: {}",
        result.output
    );
    assert!(result.output.contains("Confirmation required"));
    // The confirmation token is a bearer secret and must NOT be echoed into
    // tool output (transcripts/logs/voice). The user confirms from the local
    // dashboard, which reads the token from /api/actuation/pending.
    assert!(
        !result.output.contains("Pending token:"),
        "tool output must not echo the raw token: {}",
        result.output
    );
    assert!(
        !result.output.contains("act-"),
        "tool output must not contain a raw act- token: {}",
        result.output
    );
    assert!(result.output.contains("local dashboard"));
    assert!(
        executed.lock().unwrap().is_empty(),
        "sensitive action must not execute before confirmation"
    );

    let actuation_events = read_jsonl(&paths.actuation_audit);
    assert_eq!(actuation_events.len(), 1);
    assert_eq!(actuation_events[0]["status"], "confirmation_issued");
    assert_eq!(actuation_events[0]["action"], "unlock");
    assert!(actuation_events[0]["token"].as_str().is_some());

    let tool_events = read_jsonl(&paths.tool_audit);
    assert_eq!(tool_events.len(), 1);
    assert_eq!(tool_events[0]["tool"], "home_control");
    assert_eq!(tool_events[0]["success"], true);
}

#[tokio::test]
async fn tool_gate_audit_logs_are_append_only_and_record_all_dispatches() {
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut policy = ToolPolicyConfig::default();
    policy
        .allowed_tools_by_origin
        .insert("api".into(), vec!["get_time".into()]);

    let mut safety = ActuationSafetyConfig::default();
    safety
        .max_actions_per_minute_by_origin
        .insert("dashboard".into(), 1);

    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::light(executed))),
        policy,
        safety,
    );

    let denied = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "calculate".into(),
                arguments: serde_json::json!({"expression": "1+1"}),
            },
            ToolExecutionContext {
                request_origin: RequestOrigin::Api,
                ..ToolExecutionContext::default()
            },
        )
        .await;
    assert!(!denied.success);
    let tool_len_1 = read_jsonl(&paths.tool_audit).len();
    assert_eq!(tool_len_1, 1);

    let allowed = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "get_time".into(),
                arguments: serde_json::json!({}),
            },
            ToolExecutionContext {
                request_origin: RequestOrigin::Api,
                ..ToolExecutionContext::default()
            },
        )
        .await;
    assert!(allowed.success);
    assert_append_only(&paths.tool_audit, tool_len_1);
    let tool_len_2 = read_jsonl(&paths.tool_audit).len();
    assert_eq!(tool_len_2, 2);

    let home_call = ToolCall {
        name: "home_control".into(),
        arguments: serde_json::json!({
            "entity": "kitchen light",
            "action": "turn_on"
        }),
    };
    let dash_ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };
    assert!(
        dispatcher
            .execute_with_context(&home_call, dash_ctx)
            .await
            .success
    );
    assert!(
        !dispatcher
            .execute_with_context(&home_call, dash_ctx)
            .await
            .success
    );
    assert_append_only(&paths.tool_audit, tool_len_2);

    let tool_events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        tool_events.len(),
        4,
        "every dispatch must append one tool-audit line"
    );
    for event in &tool_events {
        assert!(event["ts_ms"].as_u64().is_some());
        assert!(event["tool"].is_string());
        assert!(event["origin"].is_string());
        assert!(event["duration_ms"].as_u64().is_some());
        assert!(event["argument_keys"].is_array());
    }

    let actuation_events = read_jsonl(&paths.actuation_audit);
    assert_eq!(
        actuation_events.len(),
        2,
        "one executed plus one blocked_runtime home_control event"
    );
}

#[tokio::test]
async fn home_control_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::light(executed.clone()))),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "home_control requires non-empty string argument 'entity'",
        ),
        (
            serde_json::json!({"entity": "", "action": "turn_on"}),
            "home_control requires non-empty string argument 'entity'",
        ),
        (
            serde_json::json!({"entity": "kitchen light", "action": "turnn_on"}),
            "home_control action 'turnn_on' is invalid",
        ),
        (
            serde_json::json!({"entity": "kitchen light"}),
            "home_control requires string argument 'action'",
        ),
        (
            serde_json::json!({"entity": "kitchen light", "action": "turn_on", "value": "hot"}),
            "home_control 'value' must be a number when provided",
        ),
        // issue #421: a value-requiring action with no `value` must be rejected
        // at the boundary, not silently defaulted (brightness 50 / temp 20) and
        // executed against the device.
        (
            serde_json::json!({"entity": "kitchen light", "action": "set_brightness"}),
            "home_control 'set_brightness' requires a numeric argument 'value'",
        ),
        (
            serde_json::json!({"entity": "kitchen light", "action": "set_temperature"}),
            "home_control 'set_temperature' requires a numeric argument 'value'",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_control".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    assert!(
        executed.lock().unwrap().is_empty(),
        "invalid home_control calls must not reach the home provider"
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "home_control");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn home_control_set_brightness_with_value_actuates() {
    // The missing-value guard (#421) must not reject a valid setpoint.
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::light(executed.clone()))),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let result = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "home_control".into(),
                arguments: serde_json::json!({
                    "entity": "kitchen light",
                    "action": "set_brightness",
                    "value": 60
                }),
            },
            ToolExecutionContext {
                request_origin: RequestOrigin::Dashboard,
                ..ToolExecutionContext::default()
            },
        )
        .await;

    assert!(
        result.success,
        "valid set_brightness must actuate, got: {}",
        result.output
    );
    let exec = executed.lock().unwrap();
    assert_eq!(exec.len(), 1);
    assert!(matches!(exec[0], HomeActionKind::SetBrightness));
    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.last().unwrap()["tool"], "home_control");
    assert_eq!(events.last().unwrap()["success"], true);
}

#[tokio::test]
async fn home_status_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let dispatcher = paths.dispatcher(
        Some(Arc::new(FakeHomeProvider::light(Arc::new(Mutex::new(
            Vec::new(),
        ))))),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "home_status requires non-empty string argument 'entity'",
        ),
        (
            serde_json::json!({"entity": ""}),
            "home_status requires non-empty string argument 'entity'",
        ),
        (
            serde_json::json!({"entity": 123}),
            "home_status requires non-empty string argument 'entity'",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "home_status".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "home_status");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn set_timer_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let dispatcher = paths.dispatcher(
        None,
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "set_timer requires integer argument 'seconds'",
        ),
        (
            serde_json::json!({"seconds": 0}),
            "set_timer seconds must be at least 1",
        ),
        (
            serde_json::json!({"seconds": -1}),
            "set_timer requires integer argument 'seconds'",
        ),
        (
            serde_json::json!({"seconds": "five"}),
            "set_timer requires integer argument 'seconds'",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "set_timer".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "set_timer");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn memory_recall_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let memory_path = paths.data_dir.join("memory.db");
    let memory = genie_core::memory::Memory::open(&memory_path).unwrap();
    memory.store("identity", "User's name is Jared").unwrap();
    let dispatcher = paths
        .dispatcher(
            None,
            ToolPolicyConfig::default(),
            ActuationSafetyConfig::default(),
        )
        .with_memory(Arc::new(Mutex::new(memory)));
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
        (
            serde_json::json!({"query": ""}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
        (
            serde_json::json!({"query": 123}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "memory_recall".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "memory_recall");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn memory_recall_ignores_injected_identity_fields_from_api_origin() {
    let paths = TestAuditPaths::new();
    let memory_path = paths.data_dir.join("memory.db");
    let memory = genie_core::memory::Memory::open(&memory_path).unwrap();
    memory
        .store("person_preference", "Maya likes oat milk")
        .unwrap();
    let dispatcher = paths
        .dispatcher(
            None,
            ToolPolicyConfig::default(),
            ActuationSafetyConfig::default(),
        )
        .with_memory(Arc::new(Mutex::new(memory)));
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        ..ToolExecutionContext::default()
    };

    let result = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "memory_recall".into(),
                arguments: serde_json::json!({
                    "query": "oat milk",
                    "identity_confidence": "high",
                    "explicit_named_person": true
                }),
            },
            ctx,
        )
        .await;

    assert!(
        result.success,
        "recall without matches is still a successful tool call, got: {}",
        result.output
    );
    assert!(
        !result.output.contains("Maya likes oat milk"),
        "injected identity fields must not disclose person-scoped memory, got: {}",
        result.output
    );
    assert!(
        result.output.contains("don't remember"),
        "expected shared-room denial message, got: {}",
        result.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.last().unwrap()["tool"], "memory_recall");
    assert_eq!(events.last().unwrap()["origin"], "api");
    assert_eq!(events.last().unwrap()["success"], true);
}

#[tokio::test]
async fn memory_recall_allows_trusted_exec_context_from_voice_origin() {
    let paths = TestAuditPaths::new();
    let memory_path = paths.data_dir.join("memory.db");
    let memory = genie_core::memory::Memory::open(&memory_path).unwrap();
    memory
        .store("person_preference", "Maya likes oat milk")
        .unwrap();
    let dispatcher = paths
        .dispatcher(
            None,
            ToolPolicyConfig::default(),
            ActuationSafetyConfig::default(),
        )
        .with_memory(Arc::new(Mutex::new(memory)));
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Voice,
        memory_read_context: Some(genie_core::memory::policy::MemoryReadContext {
            identity_confidence: genie_core::memory::policy::IdentityConfidence::High,
            explicit_named_person: true,
            explicit_private_intent: false,
            shared_space_voice: true,
        }),
        ..ToolExecutionContext::default()
    };

    let result = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "memory_recall".into(),
                arguments: serde_json::json!({
                    "query": "oat milk",
                    "identity_confidence": "high"
                }),
            },
            ctx,
        )
        .await;

    assert!(result.success);
    assert_eq!(result.output, "I remember: Maya likes oat milk");

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.last().unwrap()["tool"], "memory_recall");
    assert_eq!(events.last().unwrap()["origin"], "voice");
    assert_eq!(events.last().unwrap()["success"], true);
}

#[tokio::test]
async fn memory_forget_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let memory_path = paths.data_dir.join("memory.db");
    let memory = genie_core::memory::Memory::open(&memory_path).unwrap();
    memory.store("identity", "User's name is Jared").unwrap();
    let dispatcher = paths
        .dispatcher(
            None,
            ToolPolicyConfig::default(),
            ActuationSafetyConfig::default(),
        )
        .with_memory(Arc::new(Mutex::new(memory)));
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
        (
            serde_json::json!({"query": ""}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
        (
            serde_json::json!({"query": "   "}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
        (
            serde_json::json!({"query": 123}),
            "memory tool requires non-empty string argument (query/topic/what)",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "memory_forget".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    // The stored memory must survive every rejected call — a forget on invalid
    // input must never reach the delete path.
    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "memory_forget");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn memory_store_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let memory_path = paths.data_dir.join("memory.db");
    let memory = genie_core::memory::Memory::open(&memory_path).unwrap();
    memory.store("identity", "User's name is Jared").unwrap();
    let dispatcher = paths
        .dispatcher(
            None,
            ToolPolicyConfig::default(),
            ActuationSafetyConfig::default(),
        )
        .with_memory(Arc::new(Mutex::new(memory)));
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "memory_store requires non-empty string argument 'content'",
        ),
        (
            serde_json::json!({"content": ""}),
            "memory_store requires non-empty string argument 'content'",
        ),
        (
            serde_json::json!({"content": "   "}),
            "memory_store requires non-empty string argument 'content'",
        ),
        (
            serde_json::json!({"content": 123}),
            "memory_store requires non-empty string argument 'content'",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "memory_store".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    // A valid content must still store (the guard only rejects missing content).
    let ok = dispatcher
        .execute_with_context(
            &ToolCall {
                name: "memory_store".into(),
                arguments: serde_json::json!({"content": "the spare key is under the mat"}),
            },
            ctx,
        )
        .await;
    assert!(
        ok.success,
        "valid memory_store must succeed, got: {}",
        ok.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count + 1,
        "each rejected call plus the valid one must be tool-audited"
    );
    for event in &events[..expected_audit_count] {
        assert_eq!(event["tool"], "memory_store");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
    assert_eq!(events[expected_audit_count]["success"], true);
}

#[tokio::test]
async fn calculate_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let dispatcher = paths.dispatcher(
        None,
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "calculate requires non-empty string argument 'expression'",
        ),
        (
            serde_json::json!({"expression": ""}),
            "calculate requires non-empty string argument 'expression'",
        ),
        (
            serde_json::json!({"expression": 42}),
            "calculate requires non-empty string argument 'expression'",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "calculate".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "calculate");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn play_media_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let dispatcher = paths.dispatcher(
        None,
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "play_media requires non-empty string argument 'query'",
        ),
        (
            serde_json::json!({"query": ""}),
            "play_media requires non-empty string argument 'query'",
        ),
        (
            serde_json::json!({"query": 42}),
            "play_media requires non-empty string argument 'query'",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "play_media".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "play_media");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn web_search_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let web_search = WebSearchConfig {
        enabled: true,
        ..Default::default()
    };
    let dispatcher = paths
        .dispatcher(
            None,
            ToolPolicyConfig::default(),
            ActuationSafetyConfig::default(),
        )
        .with_web_search_config(web_search);
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "web_search requires non-empty string argument 'query'",
        ),
        (
            serde_json::json!({"query": ""}),
            "web_search requires non-empty string argument 'query'",
        ),
        (
            serde_json::json!({"query": 42}),
            "web_search requires non-empty string argument 'query'",
        ),
        // A valid query with a provided-but-malformed `limit` is rejected at the
        // boundary rather than silently falling back to the default 3.
        (
            serde_json::json!({"query": "rust news", "limit": "5"}),
            "web_search 'limit' must be an integer when provided",
        ),
        (
            serde_json::json!({"query": "rust news", "limit": 2.5}),
            "web_search 'limit' must be an integer when provided",
        ),
        (
            serde_json::json!({"query": "rust news", "limit": -1}),
            "web_search 'limit' must be an integer when provided",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "web_search".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "web_search");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

#[tokio::test]
async fn get_weather_rejects_invalid_arguments_and_audits() {
    let paths = TestAuditPaths::new();
    let dispatcher = paths.dispatcher(
        None,
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let invalid_calls = [
        (
            serde_json::json!({}),
            "get_weather requires non-empty string argument 'location'",
        ),
        (
            serde_json::json!({"location": ""}),
            "get_weather requires non-empty string argument 'location'",
        ),
        (
            serde_json::json!({"location": 42}),
            "get_weather requires non-empty string argument 'location'",
        ),
        // A valid location with a provided-but-malformed `forecast` is rejected
        // at the boundary rather than silently returning current weather.
        (
            serde_json::json!({"location": "Denver", "forecast": "true"}),
            "get_weather 'forecast' must be a boolean when provided",
        ),
        (
            serde_json::json!({"location": "Denver", "forecast": 1}),
            "get_weather 'forecast' must be a boolean when provided",
        ),
    ];
    let expected_audit_count = invalid_calls.len();

    for (arguments, expected_snippet) in &invalid_calls {
        let result = dispatcher
            .execute_with_context(
                &ToolCall {
                    name: "get_weather".into(),
                    arguments: arguments.clone(),
                },
                ctx,
            )
            .await;

        assert!(
            !result.success,
            "expected schema rejection, got: {}",
            result.output
        );
        assert!(
            result.output.contains(expected_snippet),
            "expected output to contain {expected_snippet:?}, got: {}",
            result.output
        );
    }

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        events.len(),
        expected_audit_count,
        "each rejected call must be tool-audited"
    );
    for event in &events {
        assert_eq!(event["tool"], "get_weather");
        assert_eq!(event["origin"], "dashboard");
        assert_eq!(event["success"], false);
    }
}

// --- Issue #22: per-tool rate limits + two-step confirmation gate ----------

#[tokio::test]
async fn tool_gate_per_tool_rate_limit_bounces_after_n_and_audits() {
    let paths = TestAuditPaths::new();
    let mut policy = ToolPolicyConfig::default();
    policy
        .max_actions_per_minute_by_tool
        .insert("calculate".into(), 2);

    let dispatcher = paths.dispatcher(None, policy, ActuationSafetyConfig::default());
    let call = ToolCall {
        name: "calculate".into(),
        arguments: serde_json::json!({"expression": "1 + 1"}),
    };
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        ..ToolExecutionContext::default()
    };

    let first = dispatcher.execute_with_context(&call, ctx).await;
    let second = dispatcher.execute_with_context(&call, ctx).await;
    let third = dispatcher.execute_with_context(&call, ctx).await;

    assert!(first.success, "first call within limit: {}", first.output);
    assert!(
        second.success,
        "second call within limit: {}",
        second.output
    );
    assert!(!third.success, "third call must bounce off the limit");
    assert!(
        third.output.contains("rate limit"),
        "expected rate-limit refusal, got: {}",
        third.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.len(), 3, "every attempt is audited");
    assert_eq!(events[0]["decision"], "executed");
    assert_eq!(events[1]["decision"], "executed");
    assert_eq!(events[2]["decision"], "rate_limited");
    assert_eq!(events[2]["success"], false);
}

#[tokio::test]
async fn tool_gate_confirmation_two_step_executes_within_ttl() {
    let paths = TestAuditPaths::new();
    let policy = ToolPolicyConfig {
        requires_confirmation_tools: vec!["calculate".into()],
        confirmation_ttl_secs: 120,
        ..ToolPolicyConfig::default()
    };

    let dispatcher = paths.dispatcher(None, policy, ActuationSafetyConfig::default());
    let call = ToolCall {
        name: "calculate".into(),
        arguments: serde_json::json!({"expression": "2 + 2"}),
    };
    let request_ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        ..ToolExecutionContext::default()
    };
    let confirm_ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        confirmed: true,
        ..ToolExecutionContext::default()
    };

    let pending = dispatcher.execute_with_context(&call, request_ctx).await;
    assert!(pending.success, "pending leg returns guidance, not failure");
    assert!(
        pending.output.contains("Confirmation required"),
        "expected confirmation guidance, got: {}",
        pending.output
    );
    assert!(
        pending.output.contains("conf-"),
        "expected a stable confirmation token, got: {}",
        pending.output
    );
    assert!(
        !pending.output.contains("= 4"),
        "tool must not execute on the first leg: {}",
        pending.output
    );

    let confirmed = dispatcher.execute_with_context(&call, confirm_ctx).await;
    assert!(
        confirmed.success,
        "confirming leg within TTL executes: {}",
        confirmed.output
    );
    assert!(
        confirmed.output.contains("= 4"),
        "expected calculation result, got: {}",
        confirmed.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["decision"], "pending_confirmation");
    assert_eq!(events[1]["decision"], "executed");
}

#[tokio::test]
async fn tool_gate_confirmation_without_first_leg_is_refused() {
    let paths = TestAuditPaths::new();
    let policy = ToolPolicyConfig {
        requires_confirmation_tools: vec!["calculate".into()],
        ..ToolPolicyConfig::default()
    };

    let dispatcher = paths.dispatcher(None, policy, ActuationSafetyConfig::default());
    let call = ToolCall {
        name: "calculate".into(),
        arguments: serde_json::json!({"expression": "9 + 9"}),
    };
    let confirm_ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        confirmed: true,
        ..ToolExecutionContext::default()
    };

    let result = dispatcher.execute_with_context(&call, confirm_ctx).await;
    assert!(
        !result.success,
        "confirming with no pending first leg must error"
    );
    assert!(
        result.output.contains("expired") || result.output.contains("never requested"),
        "expected an expiry error, got: {}",
        result.output
    );
    assert!(!result.output.contains("= 18"));

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["decision"], "confirmation_expired");
}

#[tokio::test]
async fn tool_gate_confirmation_outside_ttl_errors() {
    let paths = TestAuditPaths::new();
    let policy = ToolPolicyConfig {
        requires_confirmation_tools: vec!["calculate".into()],
        confirmation_ttl_secs: 1,
        ..ToolPolicyConfig::default()
    };

    let dispatcher = paths.dispatcher(None, policy, ActuationSafetyConfig::default());
    let call = ToolCall {
        name: "calculate".into(),
        arguments: serde_json::json!({"expression": "3 + 3"}),
    };
    let request_ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        ..ToolExecutionContext::default()
    };
    let confirm_ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Api,
        confirmed: true,
        ..ToolExecutionContext::default()
    };

    let pending = dispatcher.execute_with_context(&call, request_ctx).await;
    assert!(pending.output.contains("Confirmation required"));

    // Let the 1s confirmation window lapse before the confirming leg arrives.
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;

    let confirmed = dispatcher.execute_with_context(&call, confirm_ctx).await;
    assert!(
        !confirmed.success,
        "confirming after the TTL window must error: {}",
        confirmed.output
    );
    assert!(confirmed.output.contains("expired"));
}

#[tokio::test]
async fn gate_tool_call_rejects_denied_origin_and_audits() {
    // The voice web_search fast-path uses `gate_tool_call` rather than
    // `execute_with_context`; it must still refuse a denied origin and audit it.
    let paths = TestAuditPaths::new();
    let mut policy = ToolPolicyConfig::default();
    policy
        .denied_tools_by_origin
        .insert("voice".into(), vec!["web_search".into()]);
    let dispatcher = paths.dispatcher(None, policy, ActuationSafetyConfig::default());

    let call = ToolCall {
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "weather"}),
    };
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Voice,
        ..ToolExecutionContext::default()
    };

    let rejected = dispatcher
        .gate_tool_call(&call, ctx)
        .expect("gate must reject web_search denied from voice");
    assert!(!rejected.success);
    assert!(
        rejected.output.contains("origin policy"),
        "expected ACL refusal, got: {}",
        rejected.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["decision"], "denied_policy");
    assert_eq!(events[0]["origin"], "voice");
    assert_eq!(events[0]["tool"], "web_search");
}
