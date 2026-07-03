//! `web_search` tool: parse the query/limit/fresh args and search the public web
//! via [`crate::tools::web_search`].

use anyhow::Result;
use genie_common::config::WebSearchConfig;

use super::{ToolDef, ToolDispatcher};

pub(super) fn tool_def() -> ToolDef {
    ToolDef {
        name: "web_search".into(),
        description: "Search the public web using a free no-key provider. Use for current or recent public facts, online lookup requests, and explicit web search requests. Do not use for private memory, local system status, or Home Assistant state.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 5, "description": "Maximum number of results to return"},
                "fresh": {"type": "boolean", "description": "Bypass cached results and fetch fresh results"}
            },
            "required": ["query"]
        }),
    }
}

fn parse_web_search_query(args: &serde_json::Value) -> Result<&str> {
    args.get("query")
        .or_else(|| args.get("q"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("web_search requires non-empty string argument 'query'"))
}

/// `limit` stays optional — an absent or null value defaults to 3 results. But a
/// *provided* value must be a non-negative integer. The old
/// `args.get("limit").and_then(|v| v.as_u64()).unwrap_or(3)` silently coerced a
/// malformed limit (a stringified `"5"`, a float `2.5`, or a negative) to the
/// default 3, quietly returning a different result count than the caller asked
/// for. Reject the malformed value at the boundary the same way home_control
/// rejects a non-numeric `value` (PR #414); a valid integer outside 1..=5 still
/// clamps into range rather than erroring.
pub(super) fn parse_web_search_limit(args: &serde_json::Value) -> Result<usize> {
    match args.get("limit") {
        None | Some(serde_json::Value::Null) => Ok(3),
        Some(provided) => {
            let limit = provided.as_u64().ok_or_else(|| {
                anyhow::anyhow!("web_search 'limit' must be an integer when provided")
            })?;
            Ok(limit.clamp(1, 5) as usize)
        }
    }
}

/// `fresh` / `cache_bypass` stay optional — absent or null defaults to false. A
/// provided value must be a real boolean (same boundary as `get_weather` forecast).
pub(super) fn parse_web_search_fresh(args: &serde_json::Value) -> Result<bool> {
    match args.get("fresh").or_else(|| args.get("cache_bypass")) {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(anyhow::anyhow!(
            "web_search 'fresh' must be a boolean when provided"
        )),
    }
}

pub(crate) fn parse_web_search_args(args: &serde_json::Value) -> Result<(String, usize, bool)> {
    Ok((
        parse_web_search_query(args)?.to_string(),
        parse_web_search_limit(args)?,
        parse_web_search_fresh(args)?,
    ))
}

pub(super) async fn exec_web_search(
    args: &serde_json::Value,
    config: &WebSearchConfig,
) -> Result<String> {
    let (query, limit, fresh) = parse_web_search_args(args)?;
    crate::tools::web_search::search_with_options(&query, limit, config, fresh).await
}

impl ToolDispatcher {
    pub(crate) async fn web_search_response(
        &self,
        query: &str,
        limit: usize,
        fresh: bool,
    ) -> Result<crate::tools::web_search::SearchResponse> {
        crate::tools::web_search::search_response_with_options(
            query,
            limit,
            &self.web_search,
            fresh,
        )
        .await
    }
}
