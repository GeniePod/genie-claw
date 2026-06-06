use anyhow::Result;

use super::dispatch::{ToolCall, ToolDef, ToolEntry, ToolExecutionContext};

pub struct ToolRegistry {
    entries: Vec<Box<dyn ToolEntry>>,
}

impl ToolRegistry {
    pub fn new(entries: Vec<Box<dyn ToolEntry>>) -> Self {
        Self { entries }
    }

    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.entries
            .iter()
            .filter(|e| e.enabled())
            .map(|e| e.schema())
            .collect()
    }

    pub async fn dispatch(
        &self,
        call: &ToolCall,
        ctx: ToolExecutionContext,
    ) -> Result<String> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.name() == call.name.as_str())
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", call.name))?;
        entry.execute(&call.arguments, ctx).await
    }
}
