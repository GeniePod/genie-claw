//! `calculate` tool: parse the expression argument and evaluate it via
//! [`crate::tools::calc`].

use anyhow::Result;

use super::ToolDef;

pub(super) fn tool_def() -> ToolDef {
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

fn parse_calculate_expression(args: &serde_json::Value) -> Result<&str> {
    args.get("expression")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("calculate requires non-empty string argument 'expression'"))
}

pub(super) fn exec_calculate(args: &serde_json::Value) -> Result<String> {
    let expr = parse_calculate_expression(args)?;
    match crate::tools::calc::evaluate(expr) {
        Ok(result) => {
            // Format nicely: drop trailing zeros for integers.
            if result == result.floor() && result.abs() < 1e15 {
                Ok(format!("{} = {}", expr, result as i64))
            } else {
                Ok(format!("{} = {:.6}", expr, result))
            }
        }
        Err(e) => Err(anyhow::anyhow!("calculation error: {}", e)),
    }
}
