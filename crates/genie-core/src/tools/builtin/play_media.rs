use std::sync::Arc;

use anyhow::Result;

use crate::memory::Memory;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct PlayMediaTool {
    pub memory: Option<Arc<std::sync::Mutex<Memory>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvedMediaQuery {
    pub query: String,
    pub provider: Option<String>,
    pub target: Option<String>,
    pub source: String,
}

impl ResolvedMediaQuery {
    pub fn unresolved(query: &str) -> Self {
        Self {
            query: query.trim().to_string(),
            provider: None,
            target: None,
            source: "query".into(),
        }
    }

    pub fn display(&self) -> String {
        match (&self.provider, &self.target) {
            (Some(provider), Some(target))
                if target
                    .to_ascii_lowercase()
                    .starts_with(&format!("{provider}:")) =>
            {
                format!("{} ({target})", self.query)
            }
            (Some(provider), Some(target)) => format!("{} ({provider}: {target})", self.query),
            (_, Some(target)) => format!("{} ({target})", self.query),
            _ => self.query.clone(),
        }
    }
}

impl ToolEntry for PlayMediaTool {
    fn name(&self) -> &str {
        "play_media"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "play_media".into(),
            description: "Play media on the TV/HDMI output. Triggers media mode (unloads LLM, launches mpv).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to play (movie title, music, etc.)"}
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
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let resolved = resolve_media_query(&self.memory, &query);
        Box::pin(async move {
            tracing::info!(
                query,
                resolved_query = resolved.query.as_str(),
                provider = resolved.provider.as_deref().unwrap_or("unknown"),
                "triggering media mode via governor control socket"
            );
            write_media_request(&resolved).await;

            let response = governor_command(r#"{"cmd":"media_start"}"#).await;

            match response {
                Some(resp) => {
                    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                    if ok {
                        Ok(format!(
                            "Playing: {}. Switched to media mode — LLM unloaded, HDMI ready.",
                            resolved.display()
                        ))
                    } else {
                        let err = resp
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        Err(anyhow::anyhow!("governor rejected media mode: {}", err))
                    }
                }
                None => {
                    let _ = tokio::fs::create_dir_all("/run/geniepod").await;
                    tokio::fs::write("/run/geniepod/media_mode", b"1").await?;
                    Ok(format!(
                        "Playing: {}. Media mode triggered (file fallback).",
                        resolved.display()
                    ))
                }
            }
        })
    }
}

pub(crate) fn resolve_media_query(
    memory: &Option<Arc<std::sync::Mutex<Memory>>>,
    query: &str,
) -> ResolvedMediaQuery {
    let Some(memory) = memory else {
        return ResolvedMediaQuery::unresolved(query);
    };
    let Ok(memory) = memory.lock() else {
        return ResolvedMediaQuery::unresolved(query);
    };
    match memory.media_playlist_for_query(query).ok().flatten() {
        Some(item) => ResolvedMediaQuery {
            query: item.name,
            provider: item.provider,
            target: Some(item.target),
            source: "memory".into(),
        },
        None => ResolvedMediaQuery::unresolved(query),
    }
}

async fn governor_command(json_cmd: &str) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect("/run/geniepod/governor.sock")
        .await
        .ok()?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(json_cmd.as_bytes()).await.ok()?;
    writer.write_all(b"\n").await.ok()?;

    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .ok()?
        .ok()?;

    line.and_then(|l| serde_json::from_str(&l).ok())
}

async fn write_media_request(request: &ResolvedMediaQuery) {
    let result: Result<()> = async {
        tokio::fs::create_dir_all("/run/geniepod").await?;
        let json = serde_json::to_vec(request)?;
        tokio::fs::write("/run/geniepod/media_request.json", json).await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::debug!(error = %error, "media request sidecar write skipped");
    }
}
