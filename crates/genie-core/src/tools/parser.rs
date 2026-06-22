use super::dispatch::{ToolCall, ToolDispatcher, ToolExecutionContext, ToolResult};

/// Parse a tool call from LLM output and execute it.
///
/// LLMs output tool calls in various formats. This parser handles:
/// 1. Raw JSON: `{"tool": "get_time", "arguments": {}}`
/// 2. Markdown code block: ````json\n{"tool": "get_time"}\n````
/// 3. Embedded in text: `I'll check that. {"tool": "get_weather", "arguments": {"location": "Denver"}}`
/// 4. With extra fields: `{"tool": "set_timer", "arguments": {"seconds": 300}, "reasoning": "..."}`
pub async fn try_tool_call(response: &str, tools: &ToolDispatcher) -> Option<ToolResult> {
    try_tool_call_with_context(response, tools, ToolExecutionContext::default()).await
}

pub async fn try_tool_call_with_context(
    response: &str,
    tools: &ToolDispatcher,
    exec_ctx: ToolExecutionContext,
) -> Option<ToolResult> {
    let json_str = extract_json(response)?;
    let value: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let call = parse_tool_call_value(value, tools)?;

    if call.name.is_empty() {
        return None;
    }

    Some(tools.execute_with_context(&call, exec_ctx).await)
}

/// Shown to the user instead of leaking raw tool-call JSON when the model
/// produced something that looks like a tool call but could not be parsed
/// (issue #378).
pub const UNPARSED_TOOL_CALL_FALLBACK: &str =
    "Sorry, I didn't quite catch that — could you say it another way?";

/// True when the model output structurally resembles a tool call but no valid
/// tool-call JSON can be extracted from it — e.g. the model emitted invalid
/// JSON like `{"seconds": 60*60*12}` (issue #378). Such output must not be
/// rendered to the user as a normal reply, which would leak raw tool-call JSON.
/// Normal prose, or a response whose tool-call JSON parses cleanly (and is
/// therefore handled by `try_tool_call_with_context`), returns false.
pub fn is_unparsed_tool_call(response: &str) -> bool {
    let trimmed = response.trim();
    let looks_toolish = (trimmed.starts_with('{') || trimmed.starts_with("```"))
        && (trimmed.contains("\"tool\"")
            || trimmed.contains("\"arguments\"")
            || trimmed.contains("\"name\""));
    looks_toolish && extract_json(response).is_none()
}

/// Parse tool calls from model output without executing them.
///
/// This is intentionally separate from `try_tool_call_with_context`: evaluation
/// should never depend on a live dispatcher or trigger side effects. It accepts
/// the same JSON shapes as the runtime parser plus common OpenAI-compatible
/// `tool_calls` / `function_call` wrappers used by function-calling benchmarks.
pub fn parse_tool_calls_for_eval(response: &str) -> Vec<ToolCall> {
    let Some(json_str) = extract_json(response) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json_str) else {
        return Vec::new();
    };

    parse_tool_call_value_for_eval(value)
}

fn parse_tool_call_value(value: serde_json::Value, tools: &ToolDispatcher) -> Option<ToolCall> {
    if let Ok(call) = serde_json::from_value::<ToolCall>(value.clone()) {
        return Some(call);
    }

    normalize_single_key_tool_call(value, tools)
}

fn normalize_single_key_tool_call(
    value: serde_json::Value,
    tools: &ToolDispatcher,
) -> Option<ToolCall> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }

    let (tool_name, nested) = object.iter().next()?;
    let known_tool = tools.tool_defs().iter().any(|tool| tool.name == *tool_name);
    if !known_tool {
        return None;
    }

    Some(ToolCall {
        name: tool_name.clone(),
        arguments: normalize_single_key_arguments(tool_name, nested),
    })
}

fn parse_tool_call_value_for_eval(value: serde_json::Value) -> Vec<ToolCall> {
    match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(parse_single_tool_call_for_eval)
            .collect(),
        serde_json::Value::Object(object) => {
            if let Some(tool_calls) = object.get("tool_calls").and_then(|value| value.as_array()) {
                return tool_calls
                    .iter()
                    .filter_map(parse_openai_tool_call_for_eval)
                    .collect();
            }

            if let Some(function_call) = object.get("function_call") {
                return parse_openai_function_call_for_eval(function_call)
                    .into_iter()
                    .collect();
            }

            parse_single_tool_call_for_eval(serde_json::Value::Object(object))
                .into_iter()
                .collect()
        }
        _ => Vec::new(),
    }
}

fn parse_single_tool_call_for_eval(value: serde_json::Value) -> Option<ToolCall> {
    if let Ok(mut call) = serde_json::from_value::<ToolCall>(value.clone()) {
        call.arguments = normalize_eval_arguments(call.arguments);
        return (!call.name.trim().is_empty()).then_some(call);
    }

    normalize_single_key_tool_call_for_eval(value)
}

fn normalize_single_key_tool_call_for_eval(value: serde_json::Value) -> Option<ToolCall> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }

    let (tool_name, nested) = object.iter().next()?;
    if matches!(
        tool_name.as_str(),
        "answer" | "response" | "message" | "content" | "text"
    ) {
        return None;
    }

    Some(ToolCall {
        name: tool_name.clone(),
        arguments: normalize_single_key_arguments(tool_name, nested),
    })
}

/// Map the nested value of a compact single-key tool call (`{tool: <value>}`)
/// to a tool arguments object.
///
/// Small models emit two shapes: a nested **object**
/// (`{"get_weather":{"location":"Denver"}}`) whose contents are the arguments
/// verbatim, and a nested **scalar** (`{"get_weather":"Denver"}`) where the
/// model inlined the tool's primary argument. The scalar shape previously
/// collapsed to `{}`, silently dropping the value and forcing a misleading
/// schema error or empty actuation (issue #438); route it to the tool's primary
/// argument instead. Shapes that are neither object nor scalar (array/null) keep
/// the previous empty-arguments behavior.
fn normalize_single_key_arguments(
    tool_name: &str,
    nested: &serde_json::Value,
) -> serde_json::Value {
    if nested.is_object() {
        return nested.clone();
    }

    if (nested.is_string() || nested.is_number() || nested.is_boolean())
        && let Some(primary) = scalar_primary_arg(tool_name)
    {
        let mut arguments = serde_json::Map::new();
        arguments.insert(primary.to_string(), nested.clone());
        return serde_json::Value::Object(arguments);
    }

    serde_json::json!({})
}

/// The argument a scalar single-key tool call should populate, for tools whose
/// schema has a single required scalar input (see `normalize_single_key_arguments`,
/// issue #438). Each entry is the sole `required` field of that tool in
/// `ToolDispatcher::tool_defs`; the `scalar_primary_arg_matches_tool_schema` test
/// guards against drift. Tools with zero or multiple required fields (e.g.
/// `home_control`, `get_time`) are intentionally absent — a lone scalar cannot
/// unambiguously fill them, so they keep the empty-arguments fallback.
fn scalar_primary_arg(tool_name: &str) -> Option<&'static str> {
    Some(match tool_name {
        "get_weather" => "location",
        "web_search" => "query",
        "calculate" => "expression",
        "play_media" => "query",
        "memory_recall" => "query",
        "memory_store" => "content",
        "memory_forget" => "query",
        "home_status" => "entity",
        "set_timer" => "seconds",
        _ => return None,
    })
}

fn parse_openai_tool_call_for_eval(value: &serde_json::Value) -> Option<ToolCall> {
    if let Some(function) = value.get("function") {
        return parse_openai_function_call_for_eval(function);
    }

    parse_openai_function_call_for_eval(value)
}

fn parse_openai_function_call_for_eval(value: &serde_json::Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }

    let arguments = value
        .get("arguments")
        .cloned()
        .map(normalize_eval_arguments)
        .unwrap_or_else(|| serde_json::json!({}));

    Some(ToolCall {
        name: name.to_string(),
        arguments,
    })
}

fn normalize_eval_arguments(arguments: serde_json::Value) -> serde_json::Value {
    match arguments {
        serde_json::Value::String(text) => {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        }
        serde_json::Value::Null => serde_json::json!({}),
        other => other,
    }
}

/// Extract the first valid JSON object from LLM output.
///
/// Handles: raw JSON, markdown fenced blocks, embedded in prose.
fn extract_json(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // 1. Try the whole response as JSON.
    if is_json_container(trimmed) && serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }

    // 2. Try extracting from markdown code block: ```json ... ``` or ``` ... ```
    if let Some(json) = extract_from_code_block(trimmed) {
        return Some(json);
    }

    // 3. Try finding JSON embedded in text.
    if let Some(json) = extract_embedded_json(trimmed) {
        return Some(json);
    }

    None
}

fn is_json_container(text: &str) -> bool {
    (text.starts_with('{') && text.ends_with('}')) || (text.starts_with('[') && text.ends_with(']'))
}

/// Extract JSON from markdown fenced code blocks.
fn extract_from_code_block(text: &str) -> Option<String> {
    // Match ```json\n...\n``` or ```\n...\n```
    let patterns = ["```json\n", "```json\r\n", "```\n", "```\r\n"];

    for pattern in &patterns {
        if let Some(start) = text.find(pattern) {
            let content_start = start + pattern.len();
            if let Some(end) = text[content_start..].find("```") {
                let json_str = text[content_start..content_start + end].trim();
                if is_json_container(json_str)
                    && serde_json::from_str::<serde_json::Value>(json_str).is_ok()
                {
                    return Some(json_str.to_string());
                }
            }
        }
    }

    None
}

/// Find a JSON object embedded in prose text.
fn extract_embedded_json(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let close = match bytes[i] {
            b'{' => b'}',
            b'[' => b']',
            _ => {
                i += 1;
                continue;
            }
        };

        if let Some(candidate) = extract_balanced_json_candidate(text, i, bytes[i], close) {
            return Some(candidate);
        }
        i += 1;
    }

    None
}

fn extract_balanced_json_candidate(
    text: &str,
    start: usize,
    open: u8,
    close: u8,
) -> Option<String> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut in_string = false;
    let mut escape = false;

    for j in start..bytes.len() {
        if escape {
            escape = false;
            continue;
        }

        match bytes[j] {
            b'\\' if in_string => escape = true,
            b'"' => in_string = !in_string,
            value if value == open && !in_string => depth += 1,
            value if value == close && !in_string => {
                depth -= 1;
                if depth == 0 {
                    let candidate = &text[start..=j];
                    if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
                        return Some(candidate.to_string());
                    }
                    return None;
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::dispatch::ToolDispatcher;

    #[test]
    fn parse_raw_json() {
        let input = r#"{"tool": "get_time", "arguments": {}}"#;
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call.name, "get_time");
    }

    #[test]
    fn parse_markdown_code_block() {
        let input = "Sure, let me check the time for you.\n\n```json\n{\"tool\": \"get_time\", \"arguments\": {}}\n```";
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call.name, "get_time");
    }

    #[test]
    fn parse_markdown_block_no_language() {
        let input = "```\n{\"tool\": \"set_timer\", \"arguments\": {\"seconds\": 300}}\n```";
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call.name, "set_timer");
    }

    #[test]
    fn parse_embedded_in_prose() {
        let input = "I'll turn on the lights for you. {\"tool\": \"home_control\", \"arguments\": {\"entity\": \"living room light\", \"action\": \"turn_on\"}} Done!";
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call.name, "home_control");
    }

    #[test]
    fn parse_with_extra_fields() {
        let input = r#"{"tool": "get_weather", "arguments": {"location": "Tokyo"}, "reasoning": "User asked about weather"}"#;
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call.name, "get_weather");
    }

    #[test]
    fn no_tool_call_in_normal_response() {
        let input = "The current time is 3:45 PM. Is there anything else I can help with?";
        assert!(extract_json(input).is_none());
    }

    #[test]
    fn unparsed_tool_call_detected_for_invalid_json() {
        // The exact Jetson leak (issue #378): `60*60*12` is a JS expression,
        // not valid JSON, so the tool call never parses and would otherwise be
        // shown to the user verbatim.
        let leak = r#"{"tool":"set_timer","arguments":{"seconds":60*60*12,"label":"meeting"}}"#;
        assert!(is_unparsed_tool_call(leak));
        // Also when fenced.
        assert!(is_unparsed_tool_call(
            "```json\n{\"tool\":\"set_timer\",\"arguments\":{\"seconds\":60*60}}\n```"
        ));
    }

    #[test]
    fn valid_tool_call_is_not_flagged_as_unparsed() {
        let ok = r#"{"tool":"set_timer","arguments":{"seconds":300}}"#;
        assert!(!is_unparsed_tool_call(ok));
    }

    #[test]
    fn normal_prose_is_not_flagged_as_unparsed() {
        assert!(!is_unparsed_tool_call("The timer is set for 5 minutes."));
        assert!(!is_unparsed_tool_call("Hi! How can I help you today?"));
        assert!(!is_unparsed_tool_call(""));
    }

    #[test]
    fn nested_json_in_arguments() {
        let input = r#"{"tool": "home_control", "arguments": {"entity": "thermostat", "action": "set_temperature", "value": 72}}"#;
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(call.name, "home_control");
        assert_eq!(call.arguments["value"], 72);
    }

    #[test]
    fn empty_tool_name_rejected() {
        let input = r#"{"tool": "", "arguments": {}}"#;
        let json = extract_json(input).unwrap();
        let call: ToolCall = serde_json::from_str(&json).unwrap();
        assert!(call.name.is_empty()); // Parser returns it, but try_tool_call filters it
    }

    #[test]
    fn normalize_single_key_tool_call_for_known_tool() {
        let dispatcher = ToolDispatcher::new(None);
        let value = serde_json::json!({
            "system_info": {
                "uptime": 100,
                "memory": 1024
            }
        });

        let call = normalize_single_key_tool_call(value, &dispatcher).unwrap();
        assert_eq!(call.name, "system_info");
        assert_eq!(call.arguments["uptime"], 100);
    }

    #[test]
    fn normalize_single_key_tool_call_rejects_unknown_tool_name() {
        let dispatcher = ToolDispatcher::new(None);
        let value = serde_json::json!({
            "not_a_real_tool": {
                "foo": "bar"
            }
        });

        assert!(normalize_single_key_tool_call(value, &dispatcher).is_none());
    }

    #[test]
    fn single_key_scalar_value_maps_to_primary_arg() {
        let dispatcher = ToolDispatcher::new(None);
        for (json, tool, arg, expected) in [
            (
                r#"{"get_weather":"Denver"}"#,
                "get_weather",
                "location",
                "Denver",
            ),
            (
                r#"{"calculate":"2 + 2"}"#,
                "calculate",
                "expression",
                "2 + 2",
            ),
            (
                r#"{"memory_recall":"Maya"}"#,
                "memory_recall",
                "query",
                "Maya",
            ),
            (r#"{"play_media":"jazz"}"#, "play_media", "query", "jazz"),
        ] {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            let call = normalize_single_key_tool_call(value, &dispatcher).unwrap();
            assert_eq!(call.name, tool);
            assert_eq!(call.arguments[arg], expected, "tool {tool}");
        }
    }

    #[test]
    fn single_key_scalar_number_maps_to_primary_arg() {
        let dispatcher = ToolDispatcher::new(None);
        let value = serde_json::json!({ "set_timer": 300 });
        let call = normalize_single_key_tool_call(value, &dispatcher).unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 300);
    }

    #[test]
    fn single_key_object_value_is_unchanged() {
        // Regression: nested objects must still pass through verbatim.
        let dispatcher = ToolDispatcher::new(None);
        let value = serde_json::json!({ "get_weather": {"location": "Tokyo", "forecast": true} });
        let call = normalize_single_key_tool_call(value, &dispatcher).unwrap();
        assert_eq!(call.arguments["location"], "Tokyo");
        assert_eq!(call.arguments["forecast"], true);
    }

    #[test]
    fn single_key_scalar_without_primary_arg_stays_empty() {
        // home_control has two required fields (entity, action): a lone scalar
        // cannot fill it unambiguously, so arguments stay empty as before.
        let dispatcher = ToolDispatcher::new(None);
        if dispatcher
            .tool_defs()
            .iter()
            .any(|t| t.name == "home_control")
        {
            let value = serde_json::json!({ "home_control": "kitchen light" });
            let call = normalize_single_key_tool_call(value, &dispatcher).unwrap();
            assert_eq!(call.name, "home_control");
            assert_eq!(call.arguments, serde_json::json!({}));
        }
    }

    #[test]
    fn eval_parser_recovers_scalar_single_key_call() {
        let calls = parse_tool_calls_for_eval(r#"{"web_search":"jetson power consumption"}"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "jetson power consumption");
    }

    #[test]
    fn scalar_primary_arg_matches_tool_schema() {
        // Drift guard: every mapped primary arg must be the tool's sole required
        // field in its actual schema, so the table cannot silently diverge.
        let dispatcher = ToolDispatcher::new(None);
        for def in dispatcher.tool_defs() {
            if let Some(primary) = scalar_primary_arg(&def.name) {
                let required: Vec<&str> = def
                    .parameters
                    .get("required")
                    .and_then(|r| r.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                assert_eq!(
                    required,
                    vec![primary],
                    "scalar_primary_arg({}) must equal the tool's sole required field",
                    def.name
                );
            }
        }
    }

    #[tokio::test]
    async fn try_tool_call_recovers_scalar_single_key_calculate() {
        // End-to-end: a compact scalar call now executes instead of failing
        // schema validation with empty arguments (issue #438). `calculate` is
        // always registered and needs no network or home automation.
        let dispatcher = ToolDispatcher::new(None);
        let result = try_tool_call(r#"{"calculate":"2 + 2"}"#, &dispatcher)
            .await
            .unwrap();
        assert_eq!(result.tool, "calculate");
        assert!(
            result.success,
            "scalar single-key calculate should execute, got: {}",
            result.output
        );
        assert!(result.output.contains('4'), "output: {}", result.output);
    }

    #[test]
    fn eval_parser_accepts_json_array_tool_calls() {
        let input = r#"[{"tool": "get_time", "arguments": {}}, {"tool": "set_timer", "arguments": {"seconds": 60}}]"#;
        let calls = parse_tool_calls_for_eval(input);

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "get_time");
        assert_eq!(calls[1].arguments["seconds"], 60);
    }

    #[test]
    fn eval_parser_accepts_openai_tool_calls_wrapper() {
        let input = r#"{"tool_calls":[{"type":"function","function":{"name":"home_control","arguments":"{\"entity\":\"kitchen light\",\"action\":\"turn_on\"}"}}]}"#;
        let calls = parse_tool_calls_for_eval(input);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "home_control");
        assert_eq!(calls[0].arguments["entity"], "kitchen light");
    }

    #[test]
    fn eval_parser_does_not_treat_answer_object_as_tool_call() {
        let calls = parse_tool_calls_for_eval(r#"{"answer":"hello"}"#);

        assert!(calls.is_empty());
    }

    // The `system_info` tool reads /proc/meminfo (via tegrastats), /proc/uptime,
    // and /proc/loadavg. On macOS those files do not exist, so the "Memory
    // available:" line is absent from the rendered output and this assertion
    // fails. Per issue #21 AC-D1 we gate the test (not the production code) —
    // the tool itself is Linux-targeted by design, so its end-to-end shape
    // assertion only makes sense on Linux. macOS dev boxes still exercise the
    // dispatch / parsing path through the unit tests above.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn try_tool_call_executes_single_key_system_info_shape() {
        let dispatcher = ToolDispatcher::new(None);
        let input = r#"{"system_info":{"uptime":100,"memory":1024,"governor_mode":"user","load_average":0.0}}"#;

        let result = try_tool_call(input, &dispatcher).await.unwrap();
        assert_eq!(result.tool, "system_info");
        assert!(result.success);
        assert!(result.output.contains("Memory available:"));
    }
}
