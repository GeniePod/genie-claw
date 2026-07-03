//! `home_control` / `home_status` / `home_undo` / `action_history` tools: parse
//! the entity/action args, resolve device aliases, and drive Home Assistant
//! through [`crate::tools::home`] with the dispatcher's audit + confirmation
//! middleware.

use anyhow::Result;

use super::{
    TOO_MANY_PENDING_CONFIRMATIONS, ToolCall, ToolDef, ToolDispatcher, ToolExecutionContext,
    actuation_origin_allowed,
};
use crate::tools::actuation::{
    ActionLedger, AuditEvent, AuditStatus, now_ms, undo_restore_from_prior,
};
use crate::tools::home;
use crate::tools::home_action::{
    HOME_CONTROL_ACTIONS, action_requires_value, canon_home_control_action, home_action_kind,
};

pub(super) fn tool_defs() -> Vec<ToolDef> {
    vec![
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
        },
        ToolDef {
            name: "home_status".into(),
            description: "Get the current status of a smart home device, room lights, thermostat, lock, cover, scene, or other Home Assistant target.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "entity": {"type": "string", "description": "Household-facing target to query, such as 'living room lights' or 'front door lock'"}
                },
                "required": ["entity"]
            }),
        },
        ToolDef {
            name: "home_undo".into(),
            description: "Undo the most recent reversible home action. Use when the user says undo, put it back, revert that, or asks you to reverse the last device action. Still goes through runtime safety and may require confirmation.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "action_history".into(),
            description: "Report recent physical home actions and pending confirmations. Use when the user asks what you did, what changed, recent actions, or pending confirmations.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
    ]
}

pub(super) fn format_undo_output(output: String) -> String {
    if output.starts_with("Confirmation required") || output == TOO_MANY_PENDING_CONFIRMATIONS {
        output
    } else {
        format!("Undid the last home action. {}", output)
    }
}

pub(super) fn parse_home_control_args(
    args: &serde_json::Value,
) -> Result<(&str, &str, Option<f64>)> {
    let entity = args
        .get("entity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("home_control requires non-empty string argument 'entity'")
        })?;
    let raw_action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("home_control requires string argument 'action'"))?;
    let action = canon_home_control_action(raw_action).ok_or_else(|| {
        anyhow::anyhow!(
            "home_control action '{}' is invalid; expected one of: {}",
            raw_action,
            HOME_CONTROL_ACTIONS.join(", ")
        )
    })?;
    // `value` stays optional, but a *provided* value must be a number. The old
    // `args.get("value").and_then(|v| v.as_f64())` silently dropped a non-numeric
    // value (e.g. a model emitting `"value": "72"` or `"value": true`) to `None`,
    // so `set_temperature` / `set_brightness` actuated with no value instead of
    // the user's intent. Reject the malformed value at the boundary the same way
    // set_timer rejects a non-integer `seconds`; an absent or null value is still
    // a no-op None.
    let value = match args.get("value") {
        None | Some(serde_json::Value::Null) => None,
        Some(provided) => Some(provided.as_f64().ok_or_else(|| {
            anyhow::anyhow!("home_control 'value' must be a number when provided")
        })?),
    };
    // set_brightness / set_temperature actuate a numeric setpoint, so a call with
    // no `value` is under-specified. The provider used to substitute a hardcoded
    // default (brightness 50 / temperature 20) and report success, actuating a
    // setpoint the user never asked for. Reject the missing value at the boundary
    // so the agent asks for the number instead of guessing — the same boundary
    // #414 uses to reject a *non-numeric* value. (issue #421)
    if value.is_none() && action_requires_value(action) {
        anyhow::bail!("home_control '{action}' requires a numeric argument 'value'");
    }
    Ok((entity, action, value))
}

fn parse_home_status_args(args: &serde_json::Value) -> Result<&str> {
    args.get("entity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("home_status requires non-empty string argument 'entity'"))
}

impl ToolDispatcher {
    pub(super) async fn exec_home_control(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        self.exec_home_control_inner(args, exec_ctx, None).await
    }

    async fn exec_home_control_inner(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
        undo_of: Option<u64>,
    ) -> Result<String> {
        let ha = self
            .ha
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Home Assistant not connected"))?;
        let (entity_name, action, value) = parse_home_control_args(args)?;
        let resolved_entity = self.resolve_device_alias(entity_name);
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
                action: action.to_string(),
                value,
                reason: reason.clone(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
                undo_restore: None,
            });
            anyhow::bail!("Home action blocked by channel policy: {}", reason);
        }
        // Skip the rate-limit recharge for pre-confirmed actions. The
        // origin's bucket already paid one slot on the original request that
        // returned `ConfirmationRequired`; counting the confirmation re-entry
        // as a second hit would double-charge the same logical action.
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
                action: action.to_string(),
                value,
                reason: reason.clone(),
                token: None,
                confidence: None,
                action_id: None,
                undo_of: None,
                undo_restore: None,
            });
            anyhow::bail!("Home action blocked by rate limit: {}", reason);
        }
        let undo_restore = if undo_of.is_some() {
            None
        } else if matches!(action, "set_brightness" | "set_temperature" | "toggle") {
            match home_action_kind(action) {
                Ok(kind) => match ha.resolve_target(&resolved_entity, Some(kind)).await {
                    Ok(target) => ha
                        .get_state(&target)
                        .await
                        .ok()
                        .and_then(|prior| undo_restore_from_prior(action, &prior)),
                    Err(_) => None,
                },
                Err(_) => None,
            }
        } else {
            None
        };
        match home::control(
            ha.as_ref(),
            &resolved_entity,
            action,
            value,
            &self.actuation_safety,
            exec_ctx.request_origin,
            exec_ctx.confirmed,
        )
        .await
        {
            Ok(home::ControlOutcome::Executed(output, confidence)) => {
                let recorded = if let Some(original_id) = undo_of {
                    self.action_ledger.record_undo(
                        original_id,
                        &resolved_entity,
                        action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                        None,
                    )
                } else {
                    self.action_ledger.record(
                        &resolved_entity,
                        action,
                        value,
                        exec_ctx.request_origin,
                        &output,
                        confidence,
                        undo_restore,
                    )
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::Executed,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity.clone(),
                    action: action.to_string(),
                    value,
                    reason: "home action executed".into(),
                    token: None,
                    confidence,
                    action_id: Some(recorded.id),
                    undo_of: recorded.undo_of,
                    undo_restore: recorded.undo_restore.clone(),
                });
                Ok(output)
            }
            Ok(home::ControlOutcome::ConfirmationRequired { reason, .. }) => {
                let Some(pending) = self.confirmations.issue(
                    &resolved_entity,
                    action,
                    value,
                    &reason,
                    exec_ctx.request_origin,
                ) else {
                    return Ok(TOO_MANY_PENDING_CONFIRMATIONS.into());
                };
                self.audit_logger.append_or_log(AuditEvent {
                    ts_ms: now_ms(),
                    status: AuditStatus::ConfirmationIssued,
                    origin: exec_ctx.request_origin,
                    entity: resolved_entity.clone(),
                    action: action.to_string(),
                    value,
                    reason: reason.clone(),
                    token: Some(pending.token.clone()),
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                    undo_restore: None,
                });
                // The token is a bearer secret: a leaked one is a reusable
                // door-unlock credential for its full validity window. Keep it
                // out of LLM tool output (transcripts, voice, logs). The
                // dashboard fetches it over /api/actuation/pending to drive the
                // Confirm button; humans confirm there rather than reading the
                // token back from this string.
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
                    action: action.to_string(),
                    value,
                    reason: error.clone(),
                    token: None,
                    confidence: None,
                    action_id: None,
                    undo_of: None,
                    undo_restore: None,
                });
                Err(anyhow::anyhow!(error))
            }
        }
    }

    pub(super) async fn exec_home_undo(&self, exec_ctx: ToolExecutionContext) -> Result<String> {
        let action = self
            .action_ledger
            .last_undoable()
            .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?;
        let args = ActionLedger::undo_home_control_args(&action)
            .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?;
        let output = self
            .exec_home_control_inner(&args, exec_ctx, Some(action.id))
            .await?;
        Ok(format_undo_output(output))
    }

    pub(super) fn exec_action_history(&self) -> String {
        let pending = self.pending_confirmations();
        let actions = self.recent_home_actions();
        if actions.is_empty() && pending.is_empty() {
            return "No recent home actions or pending confirmations.".into();
        }

        let mut lines = Vec::new();
        if !actions.is_empty() {
            lines.push("Recent home actions:".to_string());
            for action in actions.iter().take(5) {
                let undo = action.action_history_undo_hint();
                lines.push(format!(
                    "- {} {} via {:?};{}",
                    action.action, action.entity, action.origin, undo
                ));
            }
        }
        if !pending.is_empty() {
            lines.push("Pending confirmations:".to_string());
            for item in pending.iter().take(5) {
                lines.push(format!(
                    "- {} {} requested by {:?}: {}",
                    item.action, item.entity, item.requested_by, item.reason
                ));
            }
        }
        lines.join("\n")
    }

    pub async fn confirm_pending_home_action(&self, token: &str) -> Result<String> {
        let pending = self
            .confirmations
            .confirm(token)
            .ok_or_else(|| anyhow::anyhow!("unknown or expired confirmation token"))?;
        let call = ToolCall {
            name: "home_control".into(),
            arguments: serde_json::json!({
                "entity": pending.entity,
                "action": pending.action,
                "value": pending.value,
            }),
        };
        // Re-enter with the channel that ORIGINALLY requested the action, not a
        // synthetic `Confirmation` origin. The `confirmed: true` flag is what
        // tells the policy gate the action is pre-approved; overriding origin
        // would (a) hide the originating channel in `AuditEvent.origin`,
        // (b) bypass `max_actions_per_minute_by_origin` for the requesting
        // channel by charging the `Confirmation` bucket instead, and
        // (c) break ACL setups whose `allowed_origins` exclude
        // `"confirmation"`. The original-bucket already paid one slot when
        // the request returned `ConfirmationRequired`, so the limiter skips
        // re-charging here (see `confirmed`-guard in `exec_home_control_inner`).
        let result = self
            .execute_with_context(
                &call,
                ToolExecutionContext {
                    request_origin: pending.requested_by,
                    confirmed: true,
                    ..ToolExecutionContext::default()
                },
            )
            .await;
        if result.success {
            Ok(result.output)
        } else {
            Err(anyhow::anyhow!(result.output))
        }
    }

    pub(super) async fn exec_home_status(&self, args: &serde_json::Value) -> Result<String> {
        let ha = self
            .ha
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Home Assistant not connected"))?;
        let entity_name = parse_home_status_args(args)?;
        let entity_name = self.resolve_device_alias(entity_name);

        home::status(ha.as_ref(), &entity_name).await
    }

    fn resolve_device_alias(&self, query: &str) -> String {
        let Some(memory) = &self.memory else {
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
}
