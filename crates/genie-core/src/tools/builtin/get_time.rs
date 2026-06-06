use anyhow::Result;

use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct GetTimeTool;

impl ToolEntry for GetTimeTool {
    fn name(&self) -> &str {
        "get_time"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "get_time".into(),
            description: "Get the current date and time.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    fn execute<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move { Ok(get_current_time()) })
    }
}

pub(crate) fn get_current_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    #[cfg(unix)]
    {
        let time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&time_t, &mut tm) };
        if !result.is_null() {
            return format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            );
        }
    }

    format!("Unix timestamp: {}", secs)
}
