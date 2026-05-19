//! M1 exit integration test (issue #112): tool-call gate ACL, rate-limit, and audit logs.
//!
//! Proves end-to-end that `ToolDispatcher::execute_with_context` enforces per-origin ACL,
//! physical-action rate caps, confirmation-token checks, and append-only audit ledgers.

use async_trait::async_trait;
use genie_common::config::{ActuationSafetyConfig, ToolPolicyConfig};
use genie_core::ha::{
    ActionResult, DeviceRef, HomeAction, HomeActionKind, HomeAutomationProvider, HomeGraph,
    HomeState, HomeTarget, HomeTargetKind, IntegrationHealth, SceneRef,
};
use genie_core::tools::{RequestOrigin, ToolCall, ToolDispatcher, ToolExecutionContext};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Mirrors production layout: `<data_dir>/runtime/tool-audit.jsonl` and
/// `<data_dir>/safety/actuation-audit.jsonl`.
struct TestAuditPaths {
    data_dir: PathBuf,
    tool_audit: PathBuf,
    actuation_audit: PathBuf,
}

impl TestAuditPaths {
    fn new() -> Self {
        let data_dir = std::env::temp_dir().join(format!("genie-tool-gate-it-{}", std::process::id()));
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

fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("audit line must be valid JSON"))
        .collect()
}

fn assert_append_only(path: &Path, previous_len: usize) {
    let lines = read_jsonl(path);
    assert!(
        lines.len() >= previous_len,
        "audit log at {} shrank from {previous_len} to {} lines (not append-only)",
        path.display(),
        lines.len()
    );
}

/// Kitchen light provider used for rate-limit scenarios.
struct RecordingLightProvider {
    executed: Arc<Mutex<Vec<HomeActionKind>>>,
}

#[async_trait]
impl HomeAutomationProvider for RecordingLightProvider {
    async fn health(&self) -> IntegrationHealth {
        IntegrationHealth {
            connected: true,
            cached_graph: true,
            message: "ok".into(),
        }
    }

    async fn sync_structure(&self) -> Result<HomeGraph, anyhow::Error> {
        anyhow::bail!("unused in integration test")
    }

    async fn resolve_target(
        &self,
        query: &str,
        _action_hint: Option<HomeActionKind>,
    ) -> Result<HomeTarget, anyhow::Error> {
        Ok(HomeTarget {
            kind: HomeTargetKind::Entity,
            query: query.into(),
            display_name: query.into(),
            entity_ids: vec!["light.kitchen".into()],
            domain: Some("light".into()),
            area: Some("Kitchen".into()),
            confidence: 0.96,
            voice_safe: true,
        })
    }

    async fn get_state(&self, target: &HomeTarget) -> Result<HomeState, anyhow::Error> {
        Ok(HomeState {
            target_name: target.display_name.clone(),
            domain: target.domain.clone(),
            area: target.area.clone(),
            entities: Vec::new(),
            available: true,
            spoken_summary: format!("{} is available", target.display_name),
        })
    }

    async fn execute(&self, action: HomeAction) -> Result<ActionResult, anyhow::Error> {
        self.executed.lock().unwrap().push(action.kind);
        Ok(ActionResult {
            success: true,
            spoken_summary: format!("Executed {:?}", action.kind),
            affected_targets: vec![action.target.display_name],
            state_snapshot: None,
            confidence: Some(action.target.confidence),
        })
    }

    async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>, anyhow::Error> {
        Ok(Vec::new())
    }

    async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>, anyhow::Error> {
        Ok(Vec::new())
    }
}

/// Lock provider for confirmation-gated actuation.
struct LockProvider {
    executed: Arc<Mutex<Vec<HomeActionKind>>>,
}

#[async_trait]
impl HomeAutomationProvider for LockProvider {
    async fn health(&self) -> IntegrationHealth {
        IntegrationHealth {
            connected: true,
            cached_graph: true,
            message: "ok".into(),
        }
    }

    async fn sync_structure(&self) -> Result<HomeGraph, anyhow::Error> {
        anyhow::bail!("unused in integration test")
    }

    async fn resolve_target(
        &self,
        query: &str,
        _action_hint: Option<HomeActionKind>,
    ) -> Result<HomeTarget, anyhow::Error> {
        Ok(HomeTarget {
            kind: HomeTargetKind::Entity,
            query: query.into(),
            display_name: query.into(),
            entity_ids: vec!["lock.front_door".into()],
            domain: Some("lock".into()),
            area: Some("Entry".into()),
            confidence: 0.95,
            voice_safe: false,
        })
    }

    async fn get_state(&self, target: &HomeTarget) -> Result<HomeState, anyhow::Error> {
        Ok(HomeState {
            target_name: target.display_name.clone(),
            domain: target.domain.clone(),
            area: target.area.clone(),
            entities: Vec::new(),
            available: true,
            spoken_summary: format!("{} is locked", target.display_name),
        })
    }

    async fn execute(&self, action: HomeAction) -> Result<ActionResult, anyhow::Error> {
        self.executed.lock().unwrap().push(action.kind);
        Ok(ActionResult {
            success: true,
            spoken_summary: format!("Executed {:?}", action.kind),
            affected_targets: vec![action.target.display_name],
            state_snapshot: None,
            confidence: Some(action.target.confidence),
        })
    }

    async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>, anyhow::Error> {
        Ok(Vec::new())
    }

    async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>, anyhow::Error> {
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
    let call = ToolCall {
        name: "get_time".into(),
        arguments: serde_json::json!({}),
    };
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Telegram,
        ..ToolExecutionContext::default()
    };

    let result = dispatcher.execute_with_context(&call, ctx).await;
    assert!(!result.success);
    assert!(
        result.output.contains("origin policy"),
        "expected ACL refusal, got: {}",
        result.output
    );

    let events = read_jsonl(&paths.tool_audit);
    assert_eq!(events.len(), 1, "denied tool call must appear in tool audit");
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
        Some(Arc::new(RecordingLightProvider {
            executed: executed.clone(),
        })),
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

    assert!(first.success, "first call within rate limit: {}", first.output);
    assert!(!second.success, "second call must be rate-limited");
    assert!(second.output.contains("rate limit"));

    assert_eq!(
        *executed.lock().unwrap(),
        vec![HomeActionKind::TurnOn],
        "only one physical action should execute"
    );

    let tool_events = read_jsonl(&paths.tool_audit);
    assert_eq!(
        tool_events.len(),
        2,
        "both home_control attempts must be in tool audit"
    );
    assert_eq!(tool_events[0]["success"], true);
    assert_eq!(tool_events[1]["success"], false);

    let actuation_events = read_jsonl(&paths.actuation_audit);
    let statuses: Vec<_> = actuation_events
        .iter()
        .map(|e| e["status"].as_str().unwrap())
        .collect();
    assert!(
        statuses.contains(&"executed"),
        "allowed call must be in actuation audit: {statuses:?}"
    );
    assert!(
        statuses.contains(&"blocked_runtime"),
        "rate-limited call must be in actuation audit: {statuses:?}"
    );
    assert_append_only(&paths.actuation_audit, actuation_events.len());
}

#[tokio::test]
async fn tool_gate_confirmation_token_refused_without_pending() {
    let paths = TestAuditPaths::new();
    let executed = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = paths.dispatcher(
        Some(Arc::new(LockProvider {
            executed: executed.clone(),
        })),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );

    let err = dispatcher
        .confirm_pending_home_action("act-deadbeef-no-pending")
        .await;
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
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
        Some(Arc::new(LockProvider {
            executed: executed.clone(),
        })),
        ToolPolicyConfig::default(),
        ActuationSafetyConfig::default(),
    );
    let call = ToolCall {
        name: "home_control".into(),
        arguments: serde_json::json!({
            "entity": "front door",
            "action": "unlock"
        }),
    };
    let ctx = ToolExecutionContext {
        request_origin: RequestOrigin::Dashboard,
        ..ToolExecutionContext::default()
    };

    let result = dispatcher.execute_with_context(&call, ctx).await;
    assert!(
        result.success,
        "confirmation-required path returns success with guidance: {}",
        result.output
    );
    assert!(result.output.contains("Confirmation required"));
    assert!(result.output.contains("Pending token:"));
    assert!(
        executed.lock().unwrap().is_empty(),
        "sensitive action must not execute without confirmation"
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
        Some(Arc::new(RecordingLightProvider {
            executed: executed.clone(),
        })),
        policy,
        safety,
    );

    // 1) ACL deny (voice origin not on api allowlist for calculate)
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

    // 2) ACL allow
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

    // 3) Rate limit: one allowed home_control, one denied
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
    let tool_len_final = read_jsonl(&paths.tool_audit).len();
    assert_eq!(tool_len_final, 4, "every dispatch must append one tool-audit line");

    let actuation_events = read_jsonl(&paths.actuation_audit);
    assert_eq!(
        actuation_events.len(),
        2,
        "one executed + one blocked_runtime home_control"
    );

    // Metadata sanity on tool audit lines
    for event in read_jsonl(&paths.tool_audit) {
        assert!(event["ts_ms"].as_u64().is_some());
        assert!(event["tool"].is_string());
        assert!(event["origin"].is_string());
        assert!(event["duration_ms"].as_u64().is_some());
        assert!(event["argument_keys"].is_array());
    }

    let _ = std::fs::remove_dir_all(&paths.data_dir);
}
