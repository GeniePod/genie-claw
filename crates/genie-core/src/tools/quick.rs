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

    if let Some(entity) = home_status_target(&normalized) {
        return Some(tool("home_status", serde_json::json!({ "entity": entity })));
    }

    if asks_current_time(&normalized) {
        return Some(tool("get_time", serde_json::json!({})));
    }

    None
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
}
