use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::conversation::ConversationStore;
use crate::llm::{LlmClient, Message};
use crate::memory::Memory;
use crate::tools::ToolDispatcher;

/// HTTP chat server for genie-core.
///
/// Endpoints:
///   POST /api/chat              — send message, get response
///   GET  /api/chat/history      — current conversation messages
///   POST /api/chat/clear        — clear current conversation
///   GET  /api/conversations     — list all conversations
///   GET  /api/chat/export?id=X  — export conversation as JSON
///   GET  /api/tools             — list available tools
///   GET  /api/health            — health check
///   POST /v1/chat/completions   — OpenAI-compatible (for local apps and adapters)
///
/// The local web UI and any first-party adapters connect here.
pub struct ChatServer {
    llm: LlmClient,
    tools: ToolDispatcher,
    memory: Memory,
    conversations: ConversationStore,
    current_conv_id: Mutex<String>,
    system_prompt: String,
    max_history: usize,
}

impl ChatServer {
    pub fn new(
        llm: LlmClient,
        tools: ToolDispatcher,
        memory: Memory,
        conversations: ConversationStore,
        system_prompt: String,
        max_history: usize,
    ) -> Result<Self> {
        // Create initial conversation.
        let conv_id = conversations.create()?;
        tracing::info!(conv_id = %conv_id, "created initial conversation");

        Ok(Self {
            llm,
            tools,
            memory,
            conversations,
            current_conv_id: Mutex::new(conv_id),
            system_prompt,
            max_history,
        })
    }

    /// Serve HTTP requests sequentially.
    ///
    /// Single-threaded by design: home appliance with <10 concurrent users.
    /// LLM calls are the bottleneck (seconds), not HTTP handling (microseconds).
    pub async fn serve(&self, port: u16) -> Result<()> {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        tracing::info!(addr = %addr, "genie-core HTTP server listening");

        loop {
            let (stream, _) = listener.accept().await?;
            if let Err(e) = handle_request(stream, self).await {
                tracing::debug!(error = %e, "request error");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_request(stream: tokio::net::TcpStream, ctx: &ChatServer) -> Result<()> {
    let llm = &ctx.llm;
    let tools = &ctx.tools;
    let memory = &ctx.memory;
    let conversations = &ctx.conversations;
    let current_conv_id = &ctx.current_conv_id;
    let system_prompt = &ctx.system_prompt;
    let max_history = ctx.max_history;
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // Parse request line.
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    // Read headers.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(val) = line.to_lowercase().strip_prefix("content-length: ") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    // Read body.
    let body = if content_length > 0 && content_length < 65536 {
        let mut buf = vec![0u8; content_length];
        tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut buf).await?;
        Some(String::from_utf8_lossy(&buf).to_string())
    } else {
        None
    };

    // Route.
    let (status, content_type, response_body) = match (method, path) {
        ("GET", "/" | "/index.html") => (
            200,
            "text/html; charset=utf-8",
            include_str!("chat_ui.html").into(),
        ),
        ("POST", "/api/chat") => {
            handle_chat(
                body.as_deref(),
                llm,
                tools,
                memory,
                conversations,
                current_conv_id,
                system_prompt,
                max_history,
            )
            .await
        }
        ("GET", "/api/chat/history") => handle_history(conversations, current_conv_id).await,
        ("POST", "/api/chat/clear") => handle_clear(conversations, current_conv_id).await,
        ("GET", "/api/conversations") => handle_list_conversations(conversations),
        ("GET", "/api/tools") => handle_list_tools(tools),
        ("GET", "/api/health") => handle_health(llm, memory, conversations).await,
        ("POST", "/v1/chat/completions") => {
            handle_openai_chat(
                body.as_deref(),
                llm,
                tools,
                memory,
                system_prompt,
                max_history,
            )
            .await
        }
        ("GET", "/v1/models") => handle_list_models(),
        ("OPTIONS", _) => (200, "text/plain", String::new()),
        _ => {
            // Check for query params: /api/chat/export?id=X
            if method == "GET" && path.starts_with("/api/chat/export") {
                let conv_id = path.split("id=").nth(1).unwrap_or("");
                handle_export(conversations, conv_id)
            } else {
                (404, "application/json", r#"{"error":"not found"}"#.into())
            }
        }
    };

    let http = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: POST, GET, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n",
        status,
        status_text(status),
        content_type,
        response_body.len(),
    );

    writer.write_all(http.as_bytes()).await?;
    writer.write_all(response_body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// POST /api/chat
async fn handle_chat(
    body: Option<&str>,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    current_conv_id: &Mutex<String>,
    system_prompt: &str,
    max_history: usize,
) -> (u16, &'static str, String) {
    let Some(body) = body else {
        return (
            400,
            "application/json",
            r#"{"error":"missing body"}"#.into(),
        );
    };

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return (400, "application/json", format!(r#"{{"error":"{}"}}"#, e)),
    };

    let user_text = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if user_text.trim().is_empty() {
        return (
            400,
            "application/json",
            r#"{"error":"empty message"}"#.into(),
        );
    }

    let conv_id = current_conv_id.lock().await.clone();

    // Persist user message.
    let _ = conversations.append(&conv_id, "user", user_text, None);

    // Build LLM context with per-query memory injection.
    let memory_context = crate::memory::inject::build_memory_context(memory, user_text);
    let full_prompt = format!(
        "{}\n\nRelevant household context:\n{}",
        system_prompt, memory_context
    );

    let history = conversations
        .get_recent(&conv_id, max_history)
        .unwrap_or_default();
    let mut messages = vec![Message {
        role: "system".into(),
        content: full_prompt,
    }];
    messages.extend(history);

    // Call LLM.
    let llm_response = match llm.chat(&messages, Some(512)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "LLM error");
            return (
                500,
                "application/json",
                format!(r#"{{"error":"LLM: {}"}}"#, e),
            );
        }
    };

    // Check for tool call.
    let mut tool_name: Option<String> = None;
    let final_response = if let Some(tool_result) =
        crate::tools::try_tool_call(&llm_response, tools).await
    {
        tool_name = Some(tool_result.tool.clone());

        let _ = conversations.append(&conv_id, "assistant", &llm_response, tool_name.as_deref());
        let _ = conversations.append(
            &conv_id,
            "system",
            &format!("Tool result: {}", tool_result.output),
            None,
        );

        let summary = if should_summarize_tool_result(&tool_result.tool) {
            let recent = conversations.get_recent(&conv_id, 6).unwrap_or_default();
            let mut summary_msgs = vec![Message {
                role: "system".into(),
                content: "Summarize the tool result in one natural sentence without changing numbers, measurements, or facts.".into(),
            }];
            summary_msgs.extend(recent);

            llm.chat(&summary_msgs, Some(128))
                .await
                .unwrap_or_else(|_| tool_result.output.clone())
        } else {
            tool_result.output.clone()
        };
        let sanitized_summary = crate::security::sandbox::sanitize_output(&summary);

        let _ = conversations.append(&conv_id, "assistant", &sanitized_summary, None);
        sanitized_summary
    } else {
        let sanitized = crate::security::sandbox::sanitize_output(&llm_response);
        let _ = conversations.append(&conv_id, "assistant", &sanitized, None);
        sanitized
    };

    // Auto-capture facts from user message.
    crate::memory::extract::extract_and_store(memory, user_text);

    let response = serde_json::json!({
        "response": final_response,
        "tool": tool_name,
        "conversation_id": conv_id,
    });
    (200, "application/json", response.to_string())
}

/// GET /api/chat/history
async fn handle_history(
    conversations: &ConversationStore,
    current_conv_id: &Mutex<String>,
) -> (u16, &'static str, String) {
    let conv_id = current_conv_id.lock().await.clone();
    let messages = conversations.get_messages(&conv_id).unwrap_or_default();
    let json = serde_json::to_string(&messages).unwrap_or_else(|_| "[]".into());
    (200, "application/json", json)
}

/// POST /api/chat/clear — start a new conversation.
async fn handle_clear(
    conversations: &ConversationStore,
    current_conv_id: &Mutex<String>,
) -> (u16, &'static str, String) {
    match conversations.create() {
        Ok(new_id) => {
            *current_conv_id.lock().await = new_id.clone();
            let resp = serde_json::json!({"ok": true, "conversation_id": new_id});
            (200, "application/json", resp.to_string())
        }
        Err(e) => (500, "application/json", format!(r#"{{"error":"{}"}}"#, e)),
    }
}

/// GET /api/health — rich system status.
async fn handle_health(
    llm: &LlmClient,
    memory: &Memory,
    conversations: &ConversationStore,
) -> (u16, &'static str, String) {
    let llm_ok = llm.health().await;
    let mem_count = memory.count().unwrap_or(0);
    let conv_count = conversations.list().map(|l| l.len()).unwrap_or(0);
    let mem_avail = genie_common::tegrastats::mem_available_mb().unwrap_or(0);

    let status = if llm_ok { "ok" } else { "degraded" };

    let resp = serde_json::json!({
        "status": status,
        "llm": if llm_ok { "connected" } else { "offline" },
        "memories": mem_count,
        "conversations": conv_count,
        "mem_available_mb": mem_avail,
        "version": env!("CARGO_PKG_VERSION"),
    });

    (200, "application/json", resp.to_string())
}

/// GET /api/conversations
fn handle_list_conversations(conversations: &ConversationStore) -> (u16, &'static str, String) {
    let list = conversations.list().unwrap_or_default();
    let json = serde_json::to_string(&list).unwrap_or_else(|_| "[]".into());
    (200, "application/json", json)
}

/// GET /api/chat/export?id=X
fn handle_export(conversations: &ConversationStore, conv_id: &str) -> (u16, &'static str, String) {
    match conversations.export_json(conv_id) {
        Ok(json) => (200, "application/json", json),
        Err(e) => (404, "application/json", format!(r#"{{"error":"{}"}}"#, e)),
    }
}

/// GET /api/tools
fn handle_list_tools(tools: &ToolDispatcher) -> (u16, &'static str, String) {
    let defs = tools.tool_defs();
    let json = serde_json::to_string(&defs).unwrap_or_else(|_| "[]".into());
    (200, "application/json", json)
}

/// POST /v1/chat/completions — OpenAI-compatible endpoint.
///
/// Local apps and any compatible adapter can use this.
/// Routes through the full intelligence pipeline:
///   1. Prompt injection scanning
///   2. Memory injection (identity + query-relevant)
///   3. Tool dispatch (11 built-in + loaded skills)
///   4. Auto-capture (15+ patterns)
///   5. Output sanitization
///
/// This endpoint is request-scoped: the caller supplies the message history it wants
/// the model to see. It does not reuse the web UI's shared conversation state.
async fn handle_openai_chat(
    body: Option<&str>,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    system_prompt: &str,
    max_history: usize,
) -> (u16, &'static str, String) {
    let Some(body) = body else {
        return (
            400,
            "application/json",
            r#"{"error":{"message":"missing body"}}"#.into(),
        );
    };

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return (
                400,
                "application/json",
                format!(r#"{{"error":{{"message":"{}"}}}}"#, e),
            );
        }
    };

    let messages_arr = parsed.get("messages").and_then(|v| v.as_array());
    let incoming_messages = messages_arr
        .map(|msgs| parse_openai_messages(msgs, max_history))
        .unwrap_or_default();
    let user_text = incoming_messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    if user_text.trim().is_empty() {
        return (
            400,
            "application/json",
            r#"{"error":{"message":"no user message found"}}"#.into(),
        );
    }

    let max_tokens: u32 = parsed
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(256) as u32;

    let model = parsed
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("nemotron-4b");

    // Security: scan for prompt injection.
    crate::security::injection::scan_and_warn(&user_text, "openai-bridge");

    // Build context with per-query memory injection.
    let memory_context = crate::memory::inject::build_memory_context(memory, &user_text);
    let full_prompt = format!(
        "{}\n\nRelevant household context:\n{}",
        system_prompt, memory_context
    );

    let mut llm_messages = vec![Message {
        role: "system".into(),
        content: full_prompt,
    }];
    llm_messages.extend(incoming_messages);

    // Call LLM.
    let llm_response = match llm.chat(&llm_messages, Some(max_tokens)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "LLM error in OpenAI bridge");
            return (
                500,
                "application/json",
                format!(
                    r#"{{"error":{{"message":"LLM error: {}","type":"server_error"}}}}"#,
                    e
                ),
            );
        }
    };

    // Handle tool calls.
    let final_response = if let Some(tool_result) =
        crate::tools::try_tool_call(&llm_response, tools).await
    {
        tracing::info!(
            tool = %tool_result.tool,
            success = tool_result.success,
            "tool executed via OpenAI bridge"
        );

        if should_summarize_tool_result(&tool_result.tool) {
            let mut summary_msgs = llm_messages.clone();
            summary_msgs.push(Message {
                role: "assistant".into(),
                content: llm_response.clone(),
            });
            summary_msgs.push(Message {
                role: "system".into(),
                content: format!("Tool result: {}", tool_result.output),
            });
            summary_msgs.push(Message {
                    role: "system".into(),
                    content:
                        "Summarize the tool result in one natural sentence without changing numbers, measurements, or facts.".into(),
                });

            llm.chat(&summary_msgs, Some(128))
                .await
                .unwrap_or_else(|_| tool_result.output.clone())
        } else {
            tool_result.output
        }
    } else {
        llm_response
    };

    // Security: sanitize output (redact secrets).
    let sanitized = crate::security::sandbox::sanitize_output(&final_response);

    // Auto-capture facts from user message.
    crate::memory::extract::extract_and_store(memory, &user_text);

    // Return an OpenAI-compatible response.
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let response = serde_json::json!({
        "id": format!("chatcmpl-{}", timestamp),
        "object": "chat.completion",
        "created": timestamp,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": sanitized,
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    });

    (200, "application/json", response.to_string())
}

fn parse_openai_messages(messages: &[serde_json::Value], max_history: usize) -> Vec<Message> {
    let start = messages.len().saturating_sub(max_history);

    messages[start..]
        .iter()
        .filter_map(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str())?;
            match role {
                "system" | "user" | "assistant" => Some(Message {
                    role: role.to_string(),
                    content: message_content_to_string(msg.get("content")?)?,
                }),
                _ => None,
            }
        })
        .collect()
}

fn message_content_to_string(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }

    let parts = content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| {
            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                part.get("text").and_then(|t| t.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// GET /v1/models — list available models (OpenAI-compatible).
///
/// Compatible local clients probe this to discover available models.
fn handle_list_models() -> (u16, &'static str, String) {
    let response = serde_json::json!({
        "object": "list",
        "data": [{
            "id": "nemotron-4b",
            "object": "model",
            "created": 1700000000_u64,
            "owned_by": "geniepod",
            "permission": [],
            "root": "nemotron-4b",
            "parent": null,
        }]
    });
    (200, "application/json", response.to_string())
}

fn should_summarize_tool_result(tool_name: &str) -> bool {
    !matches!(tool_name, "system_info")
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::should_summarize_tool_result;

    #[test]
    fn system_info_tool_preserves_raw_output() {
        assert!(!should_summarize_tool_result("system_info"));
    }

    #[test]
    fn other_tools_can_still_be_summarized() {
        assert!(should_summarize_tool_result("home_control"));
        assert!(should_summarize_tool_result("hello_world"));
    }
}
