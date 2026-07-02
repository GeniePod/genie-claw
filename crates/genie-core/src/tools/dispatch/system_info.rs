//! `system_info` tool schema. Execution lives in [`crate::tools::system`].

use super::ToolDef;

pub(super) fn tool_def() -> ToolDef {
    ToolDef {
        name: "system_info".into(),
        description: "Get GeniePod system status: Home Assistant connection state, memory, uptime, governor mode, and load average.".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }
}
