use std::sync::Arc;

use anyhow::Result;

use crate::tools::actuation::{
    ActionLedger, AuditEvent, AuditLogger, AuditStatus, ConfirmationManager, RequestOrigin,
    now_ms,
};
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};
use crate::tools::dispatcher::ActuationRateLimiter;
use crate::{ha::HomeAutomationProvider, memory::Memory};
use genie_common::config::ActuationSafetyConfig;

pub const HOME_CONTROL_ACTIONS: &[&str] = &[
    "turn_on",
    "turn_off",
    "toggle",
    "set_brightness",
    "set_temperature",
    "open",
    "close",
    "lock",
    "unlock",
    "activate",
];

pub fn parse_home_control_args(args: &serde_json::Value) -> Result<(String, String, Option<f64>)> {
    let entity = args
        .get("entity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("home_control requires non-empty string argument 'entity'")
        })?
        .to_owned();
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("home_control requires string argument 'action'"))?
        .to_owned();
    if !HOME_CONTROL_ACTIONS.contains(&action.as_str()) {
        anyhow::bail!(
            "home_control action '{}' is invalid; expected one of: {}",
            action,
            HOME_CONTROL_ACTIONS.join(", ")
        );
    }
    Ok((entity, action, args.get("value").and_then(|v| v.as_f64())))
}

pub struct HomeControlTool {
    pub ha: Arc<dyn HomeAutomationProvider>,
    pub memory: Option<Arc<std::sync::Mutex<Memory>>>,
    pub actuation_safety: ActuationSafetyConfig,
    pub confirmations: Arc<ConfirmationManager>,
    pub action_ledger: Arc<ActionLedger>,
    pub(crate) actuation_rate_limiter: Arc<ActuationRateLimiter>,
    pub audit_logger: Arc<AuditLogger>,
}

impl ToolEntry for HomeControlTool {
    fn name(&self) -> &str {
        "home_control"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "home_control".into(),
            description: "Control Home Assistant devices, scenes, and voice-safe routines. Use for lights, switches, climate, safe covers, and scene activation. Risky actions like locks, garage doors, cameras, and alarms require local confirmation and may be blocked.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "entity": {"type": "string", "description": "Household-facing target such as 'living room lights', 'thermostat', 'front door lock', or 'movie night'"},
                    "action": {"type": "string", "enum": ["turn_on", "turn_off", "toggle", "set_brightness", "set_temperature", "open", "close", "lock", "unlock", "activate"]},
                    "value": {"type": "number", "description": "Optional value. Brightness may be 0-100 percent or 0-255. Temperature is in degrees."}
                },
                "required": ["entity", "action"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let args = args.clone();
        Box::pin(async move {
            self.exec_home_control_inner(&args, ctx, None).await
        })
    }
}

impl HomeControlTool {
    pub(crate) async fn exec_home_control_inner(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
        undo_of: Option<u64>,
    ) -> Result<String> {
        let (entity_name, action, value) = parse_home_control_args(args)?;
        let resolved_entity = resolve_device_alias(&self.memory, &entity_name);

        if !actuation_origin_allowed(&self.actuation_safety, exec_ctx.request_origin) {
            let reason = format!(
                "actuation from '{}' is not allowed by channel policy",
                exec_ctx.request_origin.as_policy_key()
            );
            self.audit_logger.append_or_log(AuditEvent {
                ts_ms: now_ms(),
                status: AuditStatus::BlockedPolicy,
                origin: exec_ctx.request_origin,
                entity: resolved_entity.clone(),
                action: action.clone(),
                value,
                reason: reason.clone(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
            });
            anyhow::bail!("Home action blocked by channel policy: {}", reason);
        }

        if !exec_ctx.confirmed
            && let Err(err) = self
                .actuation_rate_limiter
                .check_and_record(&self.actuation_safety, exec_ctx.request_origin)
        {
            let reason = err.to_string();
            self.audit_logger.append_or_log(AuditEvent {
                ts_ms: now_ms(),
                status: AuditStatus::BlockedRuntime,
                origin: exec_ctx.request_origin,
                entity: resolved_entity.clone(),
                action: action.clone(),
                value,
                reason: reason.clone(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
            });
            anyhow::bail!("Home action blocked by rate limit: {}", reason);
        }

        match crate::tools::home::control(
            self.ha.as_ref(),
            &resolved_entity,
            &action,
            value,
            &self.actuation_safety,
            exec_ctx.request_origin,
            exec_ctx.confirmed,
        )
        .await
        {
            Ok(crate::tools::home::ControlOutcome::Executed(output, confidence)) => {
                let recorded = if let Some(original_id) = undo_of {
                    self.action_ledger.record_undo(
                        original_id,
                        &resolved_entity,
                        &action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                    )
                } else {
                    self.action_ledger.record(
                        &resolved_entity,
                        &action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                    )
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::Executed,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity.clone(),
                    action: action.clone(),
                    value,
                    reason: "home action executed".into(),
                    token: None,
                    confidence,
                    action_id: Some(recorded.id),
                    undo_of: recorded.undo_of,
                });
                Ok(output)
            }
            Ok(crate::tools::home::ControlOutcome::ConfirmationRequired { reason, .. }) => {
                let Some(pending) = self.confirmations.issue(
                    &resolved_entity,
                    &action,
                    value,
                    &reason,
                    exec_ctx.request_origin,
                ) else {
                    return Ok(
                        "Too many pending home confirmations; confirm or wait for existing ones to expire before requesting another.".into(),
                    );
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::ConfirmationIssued,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity.clone(),
                    action: action.clone(),
                    value,
                    reason: reason.clone(),
                    token: Some(pending.token.clone()),
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                });
                Ok(format!(
                    "Confirmation required before I can do that: {}. Confirm this pending action from the local dashboard (or POST /api/actuation/confirm with its token from /api/actuation/pending).",
                    reason
                ))
            }
            Err(err) => {
                let error = err.to_string();
                let status = if error.contains("local policy") {
                    AuditStatus::BlockedPolicy
                } else if error.contains("runtime safety") {
                    AuditStatus::BlockedRuntime
                } else {
                    AuditStatus::Failed
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity,
                    action: action.clone(),
                    value,
                    reason: error.clone(),
                    token: None,
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                });
                Err(anyhow::anyhow!(error))
            }
        }
    }
}

pub(crate) fn actuation_origin_allowed(
    config: &ActuationSafetyConfig,
    origin: RequestOrigin,
) -> bool {
    config
        .allowed_origins
        .iter()
        .any(|allowed| allowed.trim().eq_ignore_ascii_case(origin.as_policy_key()))
}

pub(crate) fn resolve_device_alias(
    memory: &Option<Arc<std::sync::Mutex<Memory>>>,
    query: &str,
) -> String {
    let Some(memory) = memory else {
        return query.to_string();
    };
    let Ok(memory) = memory.lock() else {
        return query.to_string();
    };
    memory
        .device_alias(query)
        .ok()
        .flatten()
        .map(|alias| alias.target_id)
        .unwrap_or_else(|| query.to_string())
}
