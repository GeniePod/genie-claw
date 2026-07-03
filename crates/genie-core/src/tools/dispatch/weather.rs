//! `get_weather` tool: parse the location/forecast args and fetch current
//! weather or a forecast via [`crate::tools::weather`].

use anyhow::Result;

use super::ToolDef;

pub(super) fn tool_def() -> ToolDef {
    ToolDef {
        name: "get_weather".into(),
        description:
            "Get current weather or forecast for a location. Use for any weather question.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "location": {"type": "string", "description": "City name (e.g., 'Denver', 'Tokyo', 'London')"},
                "forecast": {"type": "boolean", "description": "true for 7-day forecast, false for current weather"}
            },
            "required": ["location"]
        }),
    }
}

fn parse_get_weather_location(args: &serde_json::Value) -> Result<&str> {
    args.get("location")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("get_weather requires non-empty string argument 'location'"))
}

/// `forecast` stays optional — an absent or null value means current weather
/// (the no-op default). But a *provided* value must be a real boolean. The old
/// `args.get("forecast").and_then(|v| v.as_bool()).unwrap_or(false)` silently
/// coerced a stringified `"true"` (a form small models routinely emit) to
/// `false`, returning current weather when the user asked for the 7-day
/// forecast. Reject the malformed value at the boundary the same way
/// home_control rejects a non-numeric `value` (PR #414).
pub(super) fn parse_get_weather_forecast(args: &serde_json::Value) -> Result<bool> {
    match args.get("forecast") {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(anyhow::anyhow!(
            "get_weather 'forecast' must be a boolean when provided"
        )),
    }
}

pub(super) async fn exec_weather(args: &serde_json::Value) -> Result<String> {
    let location = parse_get_weather_location(args)?;
    let forecast = parse_get_weather_forecast(args)?;

    if forecast {
        crate::tools::weather::get_forecast(location).await
    } else {
        crate::tools::weather::get_weather(location).await
    }
}
