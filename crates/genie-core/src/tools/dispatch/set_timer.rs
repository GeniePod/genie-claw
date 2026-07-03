//! `set_timer` tool: parse the seconds/label args and start a countdown timer.

use anyhow::Result;

use super::{ToolDef, ToolDispatcher};

pub(super) fn tool_def() -> ToolDef {
    ToolDef {
        name: "set_timer".into(),
        description:
            "Set a countdown timer. Use for 'set a timer for 10 minutes', 'remind me in 5 minutes'."
                .into(),
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

fn parse_positive_integer_seconds(value: &serde_json::Value) -> Result<u64> {
    if let Some(seconds) = value.as_u64() {
        if seconds == 0 {
            anyhow::bail!("set_timer seconds must be at least 1");
        }
        return Ok(seconds);
    }
    if let Some(float) = value.as_f64() {
        if !float.is_finite() || float.fract() != 0.0 || float < 1.0 {
            anyhow::bail!("set_timer requires integer argument 'seconds'");
        }
        let seconds = float as u64;
        if (seconds as f64) != float {
            anyhow::bail!("set_timer requires integer argument 'seconds'");
        }
        return Ok(seconds);
    }
    anyhow::bail!("set_timer requires integer argument 'seconds'")
}

/// `label` stays optional — absent or null defaults to `"timer"`. But a *provided*
/// value must be a real string; the old `and_then(|v| v.as_str()).unwrap_or("timer")`
/// silently dropped a number/boolean/array to the default, reporting success when
/// the model emitted a schema-invalid label.
pub(super) fn parse_set_timer_label(args: &serde_json::Value) -> Result<&str> {
    match args.get("label") {
        None | Some(serde_json::Value::Null) => Ok("timer"),
        Some(serde_json::Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok("timer")
            } else {
                Ok(trimmed)
            }
        }
        Some(_) => Err(anyhow::anyhow!(
            "set_timer 'label' must be a string when provided"
        )),
    }
}

pub(super) fn parse_set_timer_args(args: &serde_json::Value) -> Result<(u64, &str)> {
    let seconds = match args.get("seconds") {
        Some(value) => parse_positive_integer_seconds(value)?,
        None => anyhow::bail!("set_timer requires integer argument 'seconds'"),
    };
    let label = parse_set_timer_label(args)?;
    Ok((seconds, label))
}

impl ToolDispatcher {
    pub(super) fn exec_set_timer(&self, args: &serde_json::Value) -> Result<String> {
        let (seconds, label) = parse_set_timer_args(args)?;
        self.timers
            .set(seconds, label)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(format!("Timer set for {} seconds: {}", seconds, label))
    }
}
