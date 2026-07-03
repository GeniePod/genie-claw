//! `play_media` tool: resolve the requested media (optionally via memory
//! playlists) and switch the device into media mode through the governor.

use anyhow::Result;

use super::{ToolDef, ToolDispatcher};

pub(super) fn tool_def() -> ToolDef {
    ToolDef {
        name: "play_media".into(),
        description:
            "Play media on the TV/HDMI output. Triggers media mode (unloads LLM, launches mpv)."
                .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to play (movie title, music, etc.)"}
            },
            "required": ["query"]
        }),
    }
}

fn parse_play_media_query(args: &serde_json::Value) -> Result<&str> {
    args.get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("play_media requires non-empty string argument 'query'"))
}

#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct ResolvedMediaQuery {
    pub(super) query: String,
    pub(super) provider: Option<String>,
    pub(super) target: Option<String>,
    source: String,
}

impl ResolvedMediaQuery {
    fn unresolved(query: &str) -> Self {
        Self {
            query: query.trim().to_string(),
            provider: None,
            target: None,
            source: "query".into(),
        }
    }

    pub(super) fn display(&self) -> String {
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

/// Send a JSON command to the governor's Unix control socket.
/// Returns parsed JSON response, or None if the governor is unreachable.
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

impl ToolDispatcher {
    pub(super) async fn exec_play_media(&self, args: &serde_json::Value) -> Result<String> {
        let query = parse_play_media_query(args)?;
        let resolved = self.resolve_media_query(query);
        tracing::info!(
            query,
            resolved_query = resolved.query.as_str(),
            provider = resolved.provider.as_deref().unwrap_or("unknown"),
            "triggering media mode via governor control socket"
        );
        write_media_request(&resolved).await;

        // Send media_start command to the governor via its Unix control socket.
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
                // Fallback: write file trigger if governor socket is unavailable.
                let _ = tokio::fs::create_dir_all("/run/geniepod").await;
                tokio::fs::write("/run/geniepod/media_mode", b"1").await?;
                Ok(format!(
                    "Playing: {}. Media mode triggered (file fallback).",
                    resolved.display()
                ))
            }
        }
    }

    pub(super) fn resolve_media_query(&self, query: &str) -> ResolvedMediaQuery {
        let Some(memory) = &self.memory else {
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
}
