use std::sync::Arc;

use anyhow::Result;

use crate::memory::Memory;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct MemoryForgetTool {
    pub memory: Arc<std::sync::Mutex<Memory>>,
}

impl ToolEntry for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "memory_forget".into(),
            description: "Forget a specific piece of information. Use ONLY when the user explicitly asks to forget something, like 'forget my age' or 'delete what you know about X'.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to forget (e.g., 'age', 'name', 'favorite color')"}
                },
                "required": ["query"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let memory = Arc::clone(&self.memory);
        Box::pin(async move {
            if query.is_empty() {
                return Ok("Please specify what to forget.".to_string());
            }
            let mem = memory
                .lock()
                .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
            let deleted = mem.delete_matching(&query)?;
            if deleted == 0 {
                Ok(format!("No memories found matching '{}'.", query))
            } else {
                Ok(format!("Forgot {} memory(ies) about '{}'.", deleted, query))
            }
        })
    }
}
