use std::sync::Arc;

use anyhow::Result;

use crate::ha::HomeAutomationProvider;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct SystemInfoTool {
    pub ha: Option<Arc<dyn HomeAutomationProvider>>,
}

impl ToolEntry for SystemInfoTool {
    fn name(&self) -> &str {
        "system_info"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "system_info".into(),
            description: "Get GeniePod system status: Home Assistant connection state, memory, uptime, governor mode, and load average.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    fn execute<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let ha = self.ha.clone();
        Box::pin(async move { crate::tools::system::system_info(ha.as_deref()).await })
    }
}
