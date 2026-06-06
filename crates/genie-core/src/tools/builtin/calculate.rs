use anyhow::Result;

use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct CalculateTool;

impl ToolEntry for CalculateTool {
    fn name(&self) -> &str {
        "calculate"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "calculate".into(),
            description: "Evaluate a math expression. Supports +, -, *, /, parentheses, decimals."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": {"type": "string", "description": "Math expression (e.g., '(100 - 32) * 5 / 9')"}
                },
                "required": ["expression"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let expr = args
            .get("expression")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .to_owned();
        Box::pin(async move {
            match crate::tools::calc::evaluate(&expr) {
                Ok(result) => {
                    if result == result.floor() && result.abs() < 1e15 {
                        Ok(format!("{} = {}", expr, result as i64))
                    } else {
                        Ok(format!("{} = {:.6}", expr, result))
                    }
                }
                Err(e) => Err(anyhow::anyhow!("calculation error: {}", e)),
            }
        })
    }
}
