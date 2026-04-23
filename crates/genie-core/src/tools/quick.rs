//! Deterministic routing for high-frequency utility requests.
//!
//! These intents should not depend on the LLM selecting the right tool. The
//! scope is intentionally small: status, time, and diagnostics where arguments
//! are unambiguous and repeated daily usefulness matters.

use super::ToolCall;

pub fn route(text: &str) -> Option<ToolCall> {
    let normalized = normalize(text);
    if normalized.is_empty() {
        return None;
    }

    if asks_memory_status(&normalized) {
        return Some(tool("memory_status", serde_json::json!({})));
    }

    if asks_system_status(&normalized) || asks_home_assistant_status(&normalized) {
        return Some(tool("system_info", serde_json::json!({})));
    }

    if let Some((seconds, label)) = timer_request(&normalized) {
        return Some(tool(
            "set_timer",
            serde_json::json!({ "seconds": seconds, "label": label }),
        ));
    }

    if let Some((location, forecast)) = weather_request(&normalized) {
        return Some(tool(
            "get_weather",
            serde_json::json!({ "location": location, "forecast": forecast }),
        ));
    }

    if let Some(expression) = calculation_request(&normalized) {
        return Some(tool(
            "calculate",
            serde_json::json!({ "expression": expression }),
        ));
    }

    if let Some(entity) = home_status_target(&normalized) {
        return Some(tool("home_status", serde_json::json!({ "entity": entity })));
    }

    if asks_current_time(&normalized) {
        return Some(tool("get_time", serde_json::json!({})));
    }

    None
}

pub fn route_for_available_tools(text: &str, home_available: bool) -> Option<ToolCall> {
    let call = route(text)?;
    if call.name == "home_status" && !home_available {
        return None;
    }
    Some(call)
}

fn tool(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        name: name.to_string(),
        arguments,
    }
}

fn normalize(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn asks_memory_status(text: &str) -> bool {
    contains_any(
        text,
        &[
            "memory status",
            "memory health",
            "memory database",
            "memory diagnostics",
            "memory index",
        ],
    )
}

fn asks_home_assistant_status(text: &str) -> bool {
    contains_any(
        text,
        &[
            "home assistant status",
            "home assistant connected",
            "home assistant connection",
            "is home assistant connected",
            "ha status",
            "ha connected",
        ],
    )
}

fn asks_system_status(text: &str) -> bool {
    matches!(
        text,
        "system status"
            | "geniepod status"
            | "genie status"
            | "status of geniepod"
            | "status of genie"
            | "uptime"
            | "load average"
            | "governor status"
    )
}

fn home_status_target(text: &str) -> Option<String> {
    if text.contains("home assistant") || !looks_like_status_query(text) {
        return None;
    }

    let target = clean_status_target(text);
    if target.is_empty() {
        return None;
    }

    if contains_any(&target, &["light", "lights", "lamp", "lamps"]) {
        return Some(if target.split_whitespace().count() == 1 {
            "lights".into()
        } else {
            target
        });
    }

    if contains_any(
        &target,
        &["switch", "switches", "plug", "plugs", "outlet", "outlets"],
    ) {
        return Some(if target.split_whitespace().count() == 1 {
            "switches".into()
        } else {
            target
        });
    }

    if contains_any(
        &target,
        &["thermostat", "thermostats", "temperature", "climate"],
    ) {
        return Some(
            if target.split_whitespace().count() == 1 || target == "temperature" {
                "thermostat".into()
            } else {
                target
            },
        );
    }

    if contains_any(
        &target,
        &[
            "cover", "covers", "blind", "blinds", "shade", "shades", "curtain", "curtains",
        ],
    ) {
        return Some(if target.split_whitespace().count() == 1 {
            "covers".into()
        } else {
            target
        });
    }

    if contains_any(&target, &["lock", "locks", "door lock", "door locks"]) {
        return Some(if target.split_whitespace().count() == 1 {
            "locks".into()
        } else {
            target
        });
    }

    None
}

fn timer_request(text: &str) -> Option<(u64, String)> {
    if !(text.contains("timer") || text.starts_with("remind me ") || text.starts_with("remind us "))
    {
        return None;
    }

    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let (seconds, unit_end_index) = parse_duration(&tokens)?;
    if seconds == 0 {
        return None;
    }

    let label = reminder_label(&tokens, unit_end_index)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| {
            if text.starts_with("remind ") {
                "reminder".into()
            } else {
                "timer".into()
            }
        });

    Some((seconds, label))
}

fn weather_request(text: &str) -> Option<(String, bool)> {
    if !(text.contains("weather") || text.contains("forecast")) {
        return None;
    }

    let location = extract_location_after_marker(text, " in ")
        .or_else(|| extract_location_after_marker(text, " for "))?;
    if location.is_empty() || location == "today" || location == "tomorrow" {
        return None;
    }

    let forecast = text.contains("forecast")
        || text.contains("tomorrow")
        || text.contains("week")
        || text.contains("7 day")
        || text.contains("seven day");

    Some((location, forecast))
}

fn extract_location_after_marker(text: &str, marker: &str) -> Option<String> {
    let (_, location) = text.rsplit_once(marker)?;
    let location = location
        .trim()
        .trim_start_matches("the ")
        .trim_end_matches(" today")
        .trim_end_matches(" tomorrow")
        .trim()
        .to_string();
    if location.is_empty() {
        None
    } else {
        Some(location)
    }
}

fn calculation_request(text: &str) -> Option<String> {
    percentage_expression(text).or_else(|| arithmetic_expression(text))
}

fn percentage_expression(text: &str) -> Option<String> {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let percent_idx = tokens
        .iter()
        .position(|token| matches!(*token, "percent" | "percentage" | "%"))?;
    let percent = parse_decimal_token(tokens.get(percent_idx.wrapping_sub(1))?)?;

    let of_idx = tokens.iter().position(|token| *token == "of")?;
    let base = parse_decimal_token(tokens.get(of_idx + 1)?)?;

    Some(format!("{} * {} / 100", base, percent))
}

fn arithmetic_expression(text: &str) -> Option<String> {
    let expression = text
        .strip_prefix("calculate ")
        .or_else(|| text.strip_prefix("what is "))
        .or_else(|| text.strip_prefix("whats "))
        .or_else(|| text.strip_prefix("what's "))
        .unwrap_or(text)
        .replace(" plus ", " + ")
        .replace(" minus ", " - ")
        .replace(" times ", " * ")
        .replace(" multiplied by ", " * ")
        .replace(" divided by ", " / ")
        .replace(" over ", " / ");

    if !expression.chars().any(|c| c.is_ascii_digit())
        || !expression
            .chars()
            .any(|c| matches!(c, '+' | '-' | '*' | '/' | '(' | ')'))
    {
        return None;
    }

    if !expression
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '.' | '+' | '-' | '*' | '/' | '(' | ')'))
    {
        return None;
    }

    Some(expression.trim().to_string())
}

fn parse_decimal_token(token: &str) -> Option<f64> {
    token
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_duration(tokens: &[&str]) -> Option<(u64, usize)> {
    for (idx, token) in tokens.iter().enumerate() {
        let Some(amount) = parse_number(token) else {
            continue;
        };
        let unit = tokens.get(idx + 1)?;
        let multiplier = match *unit {
            "second" | "seconds" | "sec" | "secs" => 1,
            "minute" | "minutes" | "min" | "mins" => 60,
            "hour" | "hours" | "hr" | "hrs" => 3600,
            _ => continue,
        };
        return Some((amount * multiplier, idx + 1));
    }
    None
}

fn parse_number(token: &str) -> Option<u64> {
    if let Ok(value) = token.parse::<u64>() {
        return Some(value);
    }

    match token {
        "one" | "a" | "an" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "fifteen" => Some(15),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "forty five" => Some(45),
        _ => None,
    }
}

fn reminder_label(tokens: &[&str], unit_end_index: usize) -> Option<String> {
    let after_unit = tokens.get(unit_end_index + 1..)?;
    let to_index = after_unit.iter().position(|token| *token == "to")?;
    let label_tokens = after_unit.get(to_index + 1..)?;
    if label_tokens.is_empty() {
        return None;
    }
    Some(label_tokens.join(" "))
}

fn looks_like_status_query(text: &str) -> bool {
    text.contains(" status")
        || text.ends_with(" status")
        || text.starts_with("what ")
        || text.starts_with("which ")
        || text.starts_with("is ")
        || text.starts_with("are ")
        || text.starts_with("any ")
        || text.starts_with("check ")
        || text.starts_with("tell me ")
}

fn clean_status_target(text: &str) -> String {
    let mut target = text.to_string();
    for prefix in [
        "what is the ",
        "what are the ",
        "what is ",
        "what are ",
        "what ",
        "which ",
        "is the ",
        "are the ",
        "is ",
        "are ",
        "any ",
        "check the ",
        "check ",
        "tell me the ",
        "tell me ",
    ] {
        if let Some(stripped) = target.strip_prefix(prefix) {
            target = stripped.to_string();
            break;
        }
    }

    for suffix in [
        " are on",
        " are off",
        " are open",
        " are closed",
        " are unlocked",
        " are locked",
        " is on",
        " is off",
        " is open",
        " is closed",
        " is unlocked",
        " is locked",
        " status",
        " on",
        " off",
        " open",
        " closed",
        " unlocked",
        " locked",
        " active",
        " right now",
        " now",
    ] {
        if let Some(stripped) = target.strip_suffix(suffix) {
            target = stripped.to_string();
            break;
        }
    }

    target.trim().to_string()
}

fn asks_current_time(text: &str) -> bool {
    matches!(
        text,
        "what time is it"
            | "what is the time"
            | "whats the time"
            | "current time"
            | "tell me the time"
            | "what date is it"
            | "what is today"
            | "what day is it"
            | "date and time"
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_home_assistant_status_to_system_info() {
        let call = route("home assistant status").unwrap();
        assert_eq!(call.name, "system_info");
    }

    #[test]
    fn routes_memory_health_to_memory_status() {
        let call = route("check memory health").unwrap();
        assert_eq!(call.name, "memory_status");
    }

    #[test]
    fn routes_time_question_to_get_time() {
        let call = route("what time is it?").unwrap();
        assert_eq!(call.name, "get_time");
    }

    #[test]
    fn routes_basic_timer() {
        let call = route("set a timer for 10 minutes").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 600);
        assert_eq!(call.arguments["label"], "timer");
    }

    #[test]
    fn routes_reminder_timer_with_label() {
        let call = route("remind me in 5 minutes to check the oven").unwrap();
        assert_eq!(call.name, "set_timer");
        assert_eq!(call.arguments["seconds"], 300);
        assert_eq!(call.arguments["label"], "check the oven");
    }

    #[test]
    fn routes_weather_with_explicit_location() {
        let call = route("weather in Tokyo").unwrap();
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments["location"], "tokyo");
        assert_eq!(call.arguments["forecast"], false);
    }

    #[test]
    fn routes_forecast_with_explicit_location() {
        let call = route("forecast for New York").unwrap();
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments["location"], "new york");
        assert_eq!(call.arguments["forecast"], true);
    }

    #[test]
    fn routes_simple_arithmetic() {
        let call = route("what is 12 plus 30").unwrap();
        assert_eq!(call.name, "calculate");
        assert_eq!(call.arguments["expression"], "12 + 30");
    }

    #[test]
    fn routes_percentage_math() {
        let call = route("what is 15 percent of 200").unwrap();
        assert_eq!(call.name, "calculate");
        assert_eq!(call.arguments["expression"], "200 * 15 / 100");
    }

    #[test]
    fn does_not_route_non_math_numbers() {
        assert!(route("what happened in 2024").is_none());
    }

    #[test]
    fn does_not_route_weather_without_location() {
        assert!(route("what is the weather").is_none());
    }

    #[test]
    fn routes_whole_home_light_status() {
        let call = route("what lights are on").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "lights");
    }

    #[test]
    fn routes_room_light_status_without_losing_room() {
        let call = route("is the kitchen light on").unwrap();
        assert_eq!(call.name, "home_status");
        assert_eq!(call.arguments["entity"], "kitchen light");
    }

    #[test]
    fn does_not_route_ambiguous_time_reference() {
        assert!(route("what time is my meeting").is_none());
    }

    #[test]
    fn does_not_route_home_control_commands_as_status() {
        assert!(route("turn on the kitchen light").is_none());
    }

    #[test]
    fn availability_filter_skips_home_status_without_home_tools() {
        assert!(route_for_available_tools("what lights are on", false).is_none());
        assert!(route_for_available_tools("what lights are on", true).is_some());
    }
}
