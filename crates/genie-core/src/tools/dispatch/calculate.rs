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
        Ok(result) => Ok(format!("{} = {}", expr, format_calc_result(result))),
        Err(e) => Err(anyhow::anyhow!("calculation error: {}", e)),
    }
}

/// Render a calculator result for display and voice. Integer values print
/// without a decimal point; non-integers are shown to at most 6 decimal places
/// with trailing zeros trimmed. Previously the non-integer branch used a raw
/// `{:.6}`, so `10 / 4` returned `2.500000` — which the voice formatter speaks
/// digit-by-digit ("two point five zero zero zero zero zero") — instead of
/// `2.5`. This finishes the "drop trailing zeros" the integer branch already did.
fn format_calc_result(result: f64) -> String {
    if result == result.floor() && result.abs() < 1e15 {
        return format!("{}", result as i64);
    }
    let rendered = format!("{result:.6}");
    rendered
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(expr: &str) -> String {
        exec_calculate(&serde_json::json!({ "expression": expr })).unwrap()
    }

    #[test]
    fn drops_trailing_zeros_from_non_integer_results() {
        // Regression: these used to render as 2.500000 / 0.875000.
        assert_eq!(calc("10 / 4"), "10 / 4 = 2.5");
        assert_eq!(calc("7 / 8"), "7 / 8 = 0.875");
    }

    #[test]
    fn integer_results_are_unchanged() {
        assert_eq!(calc("2 + 2"), "2 + 2 = 4");
        assert_eq!(calc("100 - 32"), "100 - 32 = 68");
    }

    #[test]
    fn repeating_decimals_stay_capped_at_six_places() {
        assert_eq!(calc("1 / 3"), "1 / 3 = 0.333333");
    }

    #[test]
    fn format_helper_trims_zeros_and_keeps_integers() {
        assert_eq!(format_calc_result(2.5), "2.5");
        assert_eq!(format_calc_result(4.0), "4");
        assert_eq!(format_calc_result(0.875), "0.875");
        assert_eq!(format_calc_result(-2.5), "-2.5");
    }
}
