use anyhow::Result;
use genie_common::config::WebSearchConfig;

use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct WebSearchTool {
    pub config: WebSearchConfig,
}

impl ToolEntry for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn schema(&self) -> ToolDef {
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

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let query = args
            .get("query")
            .or_else(|| args.get("q"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(3)
            .clamp(1, 5) as usize;
        let fresh = args
            .get("fresh")
            .or_else(|| args.get("cache_bypass"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let config = self.config.clone();
        Box::pin(async move {
            crate::tools::web_search::search_with_options(&query, limit, &config, fresh).await
        })
    }
}
