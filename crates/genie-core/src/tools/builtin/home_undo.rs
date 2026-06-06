use std::sync::Arc;

use anyhow::Result;

use crate::ha::HomeAutomationProvider;
use crate::memory::Memory;
use crate::tools::actuation::{ActionLedger, AuditLogger, ConfirmationManager};
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};
use crate::tools::dispatcher::ActuationRateLimiter;
use genie_common::config::ActuationSafetyConfig;

use super::home_control::HomeControlTool;

pub struct HomeUndoTool {
    pub ha: Arc<dyn HomeAutomationProvider>,
    pub memory: Option<Arc<std::sync::Mutex<Memory>>>,
    pub actuation_safety: ActuationSafetyConfig,
    pub confirmations: Arc<ConfirmationManager>,
    pub action_ledger: Arc<ActionLedger>,
    pub(crate) actuation_rate_limiter: Arc<ActuationRateLimiter>,
    pub audit_logger: Arc<AuditLogger>,
}

impl ToolEntry for HomeUndoTool {
    fn name(&self) -> &str {
        "home_undo"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "home_undo".into(),
            description: "Undo the most recent reversible home action. Use when the user says undo, put it back, revert that, or asks you to reverse the last device action. Still goes through runtime safety and may require confirmation.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    fn execute<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let action = self
                .action_ledger
                .last_undoable()
                .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?;
            let inverse = action
                .inverse_action
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("No recent reversible home action to undo."))?
                .to_owned();
            let args = serde_json::json!({
                "entity": action.entity.clone(),
                "action": inverse,
            });
            let delegate = HomeControlTool {
                ha: Arc::clone(&self.ha),
                memory: self.memory.clone(),
                actuation_safety: self.actuation_safety.clone(),
                confirmations: Arc::clone(&self.confirmations),
                action_ledger: Arc::clone(&self.action_ledger),
                actuation_rate_limiter: Arc::clone(&self.actuation_rate_limiter),
                audit_logger: Arc::clone(&self.audit_logger),
            };
            let output = delegate
                .exec_home_control_inner(&args, ctx, Some(action.id))
                .await?;
            if output.starts_with("Confirmation required") {
                Ok(output)
            } else {
                Ok(format!("Undid the last home action. {}", output))
            }
        })
    }
}
