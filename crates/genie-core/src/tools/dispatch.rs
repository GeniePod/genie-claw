use std::collections::HashMap;

use anyhow::Result;
use genie_common::config::ToolPolicyConfig;
use serde::{Deserialize, Serialize};

use super::actuation::RequestOrigin;

/// Implemented by each compiled-in tool.
///
/// The registry calls these to build the schema manifest and route execution
/// without a central match arm.
pub trait ToolEntry: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolDef;
    fn enabled(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>>;
}

/// Tool definition for LLM function calling.
///
/// These are sent to the configured LLM backend as part of the system prompt or
/// via the `tools` parameter when a backend supports OpenAI function-calling.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Result from executing a tool.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub tool: String,
    pub action_class: ToolActionClass,
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActionClass {
    ReadOnly,
    Diagnostic,
    MemoryRead,
    MemoryWrite,
    HomeActuation,
    Media,
    Network,
    Timer,
    Skill,
}

impl ToolActionClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Diagnostic => "diagnostic",
            Self::MemoryRead => "memory_read",
            Self::MemoryWrite => "memory_write",
            Self::HomeActuation => "home_actuation",
            Self::Media => "media",
            Self::Network => "network",
            Self::Timer => "timer",
            Self::Skill => "skill",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ToolExecutionContext {
    pub memory_read_context: Option<crate::memory::policy::MemoryReadContext>,
    pub request_origin: RequestOrigin,
    pub confirmed: bool,
}

/// LLM-generated tool call (parsed from model output).
/// Accepts both `{"tool": "..."}` and `{"name": "..."}` formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(alias = "tool")]
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

pub use super::dispatcher::ToolDispatcher;

pub(super) fn parse_set_timer_args(args: &serde_json::Value) -> Result<(u64, &str)> {
    let seconds = match args.get("seconds") {
        Some(value) => value
            .as_u64()
            .filter(|seconds| *seconds >= 1)
            .ok_or_else(|| {
                if value.as_u64() == Some(0) {
                    anyhow::anyhow!("set_timer seconds must be at least 1")
                } else {
                    anyhow::anyhow!("set_timer requires integer argument 'seconds'")
                }
            })?,
        None => anyhow::bail!("set_timer requires integer argument 'seconds'"),
    };
    let label = args
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("timer");
    Ok((seconds, label))
}

pub(super) fn parse_memory_recall_query(args: &serde_json::Value) -> Result<String> {
    let raw = args
        .get("query")
        .or_else(|| args.get("topic"))
        .or_else(|| args.get("what"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("memory_recall requires non-empty string argument 'query'")
        })?;
    Ok(normalize_memory_recall_query(raw))
}

fn normalize_memory_recall_query(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("my name") || lower == "name" || lower.contains("who am i") {
        "name".into()
    } else if lower.contains("about me") || lower == "me" || lower == "user" {
        "user".into()
    } else {
        raw.to_string()
    }
}

pub(super) fn tool_action_class(name: &str) -> ToolActionClass {
    match name {
        "home_control" | "home_undo" => ToolActionClass::HomeActuation,
        "play_media" => ToolActionClass::Media,
        "memory_recall" => ToolActionClass::MemoryRead,
        "memory_forget" | "memory_store" => ToolActionClass::MemoryWrite,
        "memory_status" | "system_info" | "action_history" => ToolActionClass::Diagnostic,
        "web_search" | "get_weather" => ToolActionClass::Network,
        "set_timer" => ToolActionClass::Timer,
        "home_status" | "get_time" | "calculate" => ToolActionClass::ReadOnly,
        _ => ToolActionClass::Skill,
    }
}

pub(super) fn tool_argument_keys(args: &serde_json::Value) -> Vec<String> {
    let Some(object) = args.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

pub(super) fn tool_origin_allowed(
    policy: &ToolPolicyConfig,
    origin: RequestOrigin,
    tool_name: &str,
) -> Result<()> {
    if !policy.enabled {
        return Ok(());
    }

    let origin_key = origin.as_policy_key();
    if tool_list_contains(&policy.denied_tools_by_origin, origin_key, tool_name) {
        anyhow::bail!("tool '{}' is denied for origin '{}'", tool_name, origin_key);
    }

    if let Some(allowed) = origin_tool_list(&policy.allowed_tools_by_origin, origin_key)
        && !tool_matches(allowed, tool_name)
    {
        anyhow::bail!(
            "tool '{}' is not in the allowlist for origin '{}'",
            tool_name,
            origin_key
        );
    }

    Ok(())
}

fn tool_list_contains(
    rules: &HashMap<String, Vec<String>>,
    origin_key: &str,
    tool_name: &str,
) -> bool {
    origin_tool_list(rules, origin_key)
        .map(|tools| tool_matches(tools, tool_name))
        .unwrap_or(false)
}

fn origin_tool_list<'a>(
    rules: &'a HashMap<String, Vec<String>>,
    origin_key: &str,
) -> Option<&'a Vec<String>> {
    rules.get(origin_key).or_else(|| rules.get("*"))
}

fn tool_matches(tools: &[String], tool_name: &str) -> bool {
    tools.iter().any(|tool| tool == "*" || tool == tool_name)
}
