use std::sync::Arc;

use anyhow::Result;

use crate::ha::HomeAutomationProvider;
use crate::memory::Memory;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

use super::home_control::resolve_device_alias;

pub struct HomeStatusTool {
    pub ha: Arc<dyn HomeAutomationProvider>,
    pub memory: Option<Arc<std::sync::Mutex<Memory>>>,
}

impl ToolEntry for HomeStatusTool {
    fn name(&self) -> &str {
        "home_status"
    }

    fn schema(&self) -> ToolDef {
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
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let entity_name = parse_home_status_args(args)
            .map(|s| resolve_device_alias(&self.memory, s))
            .map_err(|e| e.to_string());
        let ha = Arc::clone(&self.ha);
        Box::pin(async move {
            let entity_name = entity_name.map_err(|e| anyhow::anyhow!(e))?;
            crate::tools::home::status(ha.as_ref(), &entity_name).await
        })
    }
}

fn parse_home_status_args(args: &serde_json::Value) -> Result<&str> {
    args.get("entity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("home_status requires non-empty string argument 'entity'"))
}
