use std::sync::Arc;

use anyhow::Result;

use crate::tools::actuation::{ActionLedger, ConfirmationManager};
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct ActionHistoryTool {
    pub confirmations: Arc<ConfirmationManager>,
    pub action_ledger: Arc<ActionLedger>,
}

impl ToolEntry for ActionHistoryTool {
    fn name(&self) -> &str {
        "action_history"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "action_history".into(),
            description: "Report recent physical home actions and pending confirmations. Use when the user asks what you did, what changed, recent actions, or pending confirmations.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    fn execute<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let pending = self.confirmations.list();
        let actions = self.action_ledger.list();
        Box::pin(async move {
            if actions.is_empty() && pending.is_empty() {
                return Ok("No recent home actions or pending confirmations.".into());
            }

            let mut lines = Vec::new();
            if !actions.is_empty() {
                lines.push("Recent home actions:".to_string());
                for action in actions.iter().take(5) {
                    let undo = action
                        .inverse_action
                        .as_deref()
                        .map(|inverse| format!(" undo: {inverse}"))
                        .unwrap_or_else(|| " not undoable".into());
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
            Ok(lines.join("\n"))
        })
    }
}
