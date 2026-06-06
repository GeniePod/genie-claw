use anyhow::Result;

use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct GetWeatherTool;

impl ToolEntry for GetWeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "get_weather".into(),
            description:
                "Get current weather or forecast for a location. Use for any weather question."
                    .into(),
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

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let location = args
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("Denver")
            .to_owned();
        let forecast = args
            .get("forecast")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Box::pin(async move {
            if forecast {
                crate::tools::weather::get_forecast(&location).await
            } else {
                crate::tools::weather::get_weather(&location).await
            }
        })
    }
}
