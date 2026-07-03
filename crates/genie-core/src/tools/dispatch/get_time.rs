//! `get_time` tool: report the current local date and time.

use super::ToolDef;

pub(super) fn tool_def() -> ToolDef {
    ToolDef {
        name: "get_time".into(),
        description: "Get the current date and time.".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }
}

pub(super) fn get_current_time() -> String {
    // Use libc for proper timezone.
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
