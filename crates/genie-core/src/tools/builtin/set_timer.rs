use std::sync::Arc;

use anyhow::Result;

use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};
use crate::tools::timer::TimerManager;

pub struct SetTimerTool {
    pub timers: Arc<TimerManager>,
}

impl ToolEntry for SetTimerTool {
    fn name(&self) -> &str {
        "set_timer"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "set_timer".into(),
            description: "Set a countdown timer. Use for 'set a timer for 10 minutes', 'remind me in 5 minutes'.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "seconds": {"type": "integer", "description": "Duration in seconds"},
                    "label": {"type": "string", "description": "What the timer is for"}
                },
                "required": ["seconds"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let parsed = super::super::dispatch::parse_set_timer_args(args);
        let timers = Arc::clone(&self.timers);
        Box::pin(async move {
            let (seconds, label) = parsed?;
            let label = label.to_owned();
            timers
                .set(seconds, &label)
                .map_err(|e| anyhow::anyhow!(e))?;
            Ok(format!("Timer set for {} seconds: {}", seconds, label))
        })
    }
}
