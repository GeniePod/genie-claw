use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::Mutex;

use crate::connectivity::{ConnectivityController, ConnectivityState};
use crate::conversation::ConversationStore;
use crate::llm::{LlmClient, Message};
use crate::memory::Memory;
use crate::prompt::ModelFamily;
use crate::reasoning::InteractionKind;
use crate::tools::ToolDispatcher;

/// HTTP chat server for genie-core.
///
/// Endpoints:
///   POST /api/chat              — send message, get response
///   POST /api/chat/stream       — send message, stream response
///   GET  /api/chat/history      — current conversation messages
///   POST /api/chat/clear        — clear current conversation
///   GET  /api/conversations     — list all conversations
///   GET  /api/chat/export?id=X  — export conversation as JSON
///   GET  /api/tools             — list available tools
///   POST /api/web-search        — direct web search tool execution
///   GET  /api/health            — health check
///   GET  /api/connectivity      — connectivity coprocessor status
///   POST /v1/chat/completions   — OpenAI-compatible (for local apps and adapters)
///
/// The local web UI and any first-party adapters connect here.
pub struct ChatServer {
    llm: LlmClient,
    tools: ToolDispatcher,
    connectivity: std::sync::Arc<dyn ConnectivityController>,
    memory: Memory,
    conversations: ConversationStore,
    current_conv_id: Mutex<String>,
    system_prompt: String,
    max_history: usize,
    model_family: ModelFamily,
}

pub struct ChatTurnResult {
    pub response: String,
    pub tool: Option<String>,
    pub conversation_id: String,
}

impl ChatServer {
    pub fn new(
        llm: LlmClient,
        tools: ToolDispatcher,
        connectivity: std::sync::Arc<dyn ConnectivityController>,
        memory: Memory,
        conversations: ConversationStore,
        system_prompt: String,
        max_history: usize,
        model_family: ModelFamily,
    ) -> Result<Self> {
        // Create initial conversation.
        let conv_id = conversations.create()?;
        tracing::info!(conv_id = %conv_id, "created initial conversation");

        Ok(Self {
            llm,
            tools,
            connectivity,
            memory,
            conversations,
            current_conv_id: Mutex::new(conv_id),
            system_prompt,
            max_history,
            model_family,
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
    let connectivity = ctx.connectivity.as_ref();
    let conversations = &ctx.conversations;
    let current_conv_id = &ctx.current_conv_id;
    let system_prompt = &ctx.system_prompt;
    let max_history = ctx.max_history;
    let model_family = ctx.model_family;
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
    if method == "POST" && path == "/api/chat/stream" {
        if let Err(e) = handle_chat_stream(
            &mut writer,
            body.as_deref(),
            llm,
            tools,
            memory,
            conversations,
            current_conv_id,
            system_prompt,
            max_history,
            model_family,
        )
        .await
        {
            tracing::error!(error = %e, "streaming chat failed");
        }
        return Ok(());
    }

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
                model_family,
            )
            .await
        }
        ("GET", "/api/chat/history") => handle_history(conversations, current_conv_id).await,
        ("POST", "/api/chat/clear") => handle_clear(conversations, current_conv_id).await,
        ("GET", "/api/conversations") => handle_list_conversations(conversations),
        ("GET", "/api/tools") => handle_list_tools(tools),
        ("POST", "/api/web-search") => handle_web_search(body.as_deref(), tools).await,
        ("GET", "/api/health") => {
            handle_health(llm, tools, connectivity, memory, conversations).await
        }
        ("GET", "/api/connectivity") => handle_connectivity(connectivity).await,
        ("POST", "/v1/chat/completions") => {
            handle_openai_chat(
                body.as_deref(),
                llm,
                tools,
                memory,
                system_prompt,
                max_history,
                model_family,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamMode {
    Undecided,
    Text,
    Tool,
}

struct StreamState {
    mode: StreamMode,
    pending: String,
    emitted_text: bool,
}

async fn handle_chat_stream(
    writer: &mut OwnedWriteHalf,
    body: Option<&str>,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    current_conv_id: &Mutex<String>,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
) -> Result<()> {
    let Some(body) = body else {
        write_stream_headers(writer, 400).await?;
        write_stream_event(
            writer,
            &serde_json::json!({"type":"error","message":"missing body"}),
        )
        .await?;
        return Ok(());
    };

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            write_stream_headers(writer, 400).await?;
            write_stream_event(
                writer,
                &serde_json::json!({"type":"error","message": format!("invalid JSON: {}", e)}),
            )
            .await?;
            return Ok(());
        }
    };

    let user_text = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if user_text.trim().is_empty() {
        write_stream_headers(writer, 400).await?;
        write_stream_event(
            writer,
            &serde_json::json!({"type":"error","message":"empty message"}),
        )
        .await?;
        return Ok(());
    }

    let conv_id = parsed
        .get("conversation_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| String::new());
    let conv_id = if conv_id.is_empty() {
        current_conv_id.lock().await.clone()
    } else {
        conv_id
    };

    conversations.ensure(&conv_id, "New conversation")?;
    conversations.append(&conv_id, "user", user_text, None)?;

    if let Some(call) = crate::tools::quick::route_for_available_tools(
        user_text,
        tools.has_home_automation(),
        tools.has_web_search(),
    ) {
        write_stream_headers(writer, 200).await?;
        write_stream_event(
            writer,
            &serde_json::json!({"type":"start","conversation_id": conv_id}),
        )
        .await?;

        let tool_result = tools.execute(&call).await;
        let final_response =
            finalize_direct_tool_turn(conversations, &conv_id, &call, &tool_result);
        write_stream_event(
            writer,
            &serde_json::json!({"type":"replace","content": final_response.clone(), "tool": tool_result.tool.clone()}),
        )
        .await?;
        crate::memory::extract::extract_and_store(memory, user_text);
        write_stream_event(
            writer,
            &serde_json::json!({
                "type":"done",
                "response": final_response,
                "tool": tool_result.tool.clone(),
                "conversation_id": conv_id
            }),
        )
        .await?;
        return Ok(());
    }

    let memory_context = crate::memory::inject::build_memory_context(memory, user_text);
    let full_prompt = format!(
        "{}\n\nRelevant household context:\n{}",
        system_prompt, memory_context
    );

    let history = conversations.get_recent(&conv_id, max_history)?;
    let mut messages = vec![Message {
        role: "system".into(),
        content: full_prompt,
    }];
    messages.extend(history);
    let (messages, decision) = crate::reasoning::apply_reasoning_mode(
        model_family,
        &messages,
        user_text,
        InteractionKind::Chat,
    );
    tracing::debug!(
        ?model_family,
        ?decision,
        "applied reasoning mode for streamed chat"
    );

    write_stream_headers(writer, 200).await?;
    write_stream_event(
        writer,
        &serde_json::json!({"type":"start","conversation_id": conv_id}),
    )
    .await?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let producer = llm.chat_stream(&messages, Some(512), move |token| {
        let _ = tx.send(token.to_string());
    });

    let consumer = async {
        let mut state = StreamState {
            mode: StreamMode::Undecided,
            pending: String::new(),
            emitted_text: false,
        };

        while let Some(token) = rx.recv().await {
            match state.mode {
                StreamMode::Text => {
                    write_stream_event(
                        writer,
                        &serde_json::json!({"type":"token","content": token}),
                    )
                    .await?;
                    state.emitted_text = true;
                }
                StreamMode::Undecided | StreamMode::Tool => {
                    state.pending.push_str(&token);

                    if state.mode == StreamMode::Undecided {
                        match detect_stream_mode(&state.pending) {
                            StreamMode::Text => {
                                write_stream_event(
                                    writer,
                                    &serde_json::json!({"type":"token","content": state.pending}),
                                )
                                .await?;
                                state.pending.clear();
                                state.mode = StreamMode::Text;
                                state.emitted_text = true;
                            }
                            StreamMode::Tool => state.mode = StreamMode::Tool,
                            StreamMode::Undecided => {}
                        }
                    }
                }
            }
        }

        Ok::<StreamState, anyhow::Error>(state)
    };

    let (llm_result, state_result) = tokio::join!(producer, consumer);
    let mut state = state_result?;
    let llm_response = llm_result?;

    let mut tool_name: Option<String> = None;
    let final_response =
        if let Some(tool_result) = crate::tools::try_tool_call(&llm_response, tools).await {
            tool_name = Some(tool_result.tool.clone());
            let summary = finalize_tool_turn(
                llm,
                conversations,
                &conv_id,
                &llm_response,
                &tool_result,
                model_family,
            )
            .await;

            if !state.emitted_text {
                write_stream_event(
                    writer,
                    &serde_json::json!({"type":"replace","content": summary, "tool": tool_name}),
                )
                .await?;
            }
            summary
        } else {
            let sanitized = crate::security::sandbox::sanitize_output(&llm_response);
            if !state.pending.is_empty() {
                if state.mode == StreamMode::Undecided {
                    write_stream_event(
                        writer,
                        &serde_json::json!({"type":"token","content": state.pending}),
                    )
                    .await?;
                    state.pending.clear();
                    state.emitted_text = true;
                }
            }
            let _ = conversations.append(&conv_id, "assistant", &sanitized, None);
            sanitized
        };

    crate::memory::extract::extract_and_store(memory, user_text);

    write_stream_event(
        writer,
        &serde_json::json!({
            "type":"done",
            "response": final_response,
            "tool": tool_name,
            "conversation_id": conv_id
        }),
    )
    .await?;

    Ok(())
}

/// POST /api/chat
pub async fn process_chat_turn(
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    conv_id: &str,
    user_text: &str,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
) -> Result<ChatTurnResult> {
    conversations.ensure(conv_id, "New conversation")?;
    conversations.append(conv_id, "user", user_text, None)?;

    if let Some(call) = crate::tools::quick::route_for_available_tools(
        user_text,
        tools.has_home_automation(),
        tools.has_web_search(),
    ) {
        let tool_result = tools.execute(&call).await;
        let final_response = finalize_direct_tool_turn(conversations, conv_id, &call, &tool_result);
        crate::memory::extract::extract_and_store(memory, user_text);
        return Ok(ChatTurnResult {
            response: final_response,
            tool: Some(tool_result.tool),
            conversation_id: conv_id.to_string(),
        });
    }

    let memory_context = crate::memory::inject::build_memory_context(memory, user_text);
    let full_prompt = format!(
        "{}\n\nRelevant household context:\n{}",
        system_prompt, memory_context
    );

    let history = conversations.get_recent(conv_id, max_history)?;
    let mut messages = vec![Message {
        role: "system".into(),
        content: full_prompt,
    }];
    messages.extend(history);
    let (messages, decision) = crate::reasoning::apply_reasoning_mode(
        model_family,
        &messages,
        user_text,
        InteractionKind::Chat,
    );
    tracing::debug!(
        ?model_family,
        ?decision,
        "applied reasoning mode for chat turn"
    );

    let llm_response = llm.chat(&messages, Some(512)).await?;

    let mut tool_name: Option<String> = None;
    let final_response =
        if let Some(tool_result) = crate::tools::try_tool_call(&llm_response, tools).await {
            tool_name = Some(tool_result.tool.clone());
            finalize_tool_turn(
                llm,
                conversations,
                conv_id,
                &llm_response,
                &tool_result,
                model_family,
            )
            .await
        } else {
            let sanitized = crate::security::sandbox::sanitize_output(&llm_response);
            let _ = conversations.append(conv_id, "assistant", &sanitized, None);
            sanitized
        };

    crate::memory::extract::extract_and_store(memory, user_text);

    Ok(ChatTurnResult {
        response: final_response,
        tool: tool_name,
        conversation_id: conv_id.to_string(),
    })
}

fn finalize_direct_tool_turn(
    conversations: &ConversationStore,
    conv_id: &str,
    call: &crate::tools::ToolCall,
    tool_result: &crate::tools::ToolResult,
) -> String {
    let tool_json = serde_json::json!({
        "tool": call.name,
        "arguments": call.arguments,
    })
    .to_string();
    let _ = conversations.append(conv_id, "assistant", &tool_json, Some(&tool_result.tool));
    let _ = conversations.append(
        conv_id,
        "system",
        &format!("Tool result: {}", tool_result.output),
        None,
    );

    let response = if tool_result.success {
        tool_result.output.clone()
    } else {
        format!("{} failed: {}", tool_result.tool, tool_result.output)
    };
    let sanitized = crate::security::sandbox::sanitize_output(&response);
    let _ = conversations.append(conv_id, "assistant", &sanitized, None);
    sanitized
}

async fn finalize_tool_turn(
    llm: &LlmClient,
    conversations: &ConversationStore,
    conv_id: &str,
    llm_response: &str,
    tool_result: &crate::tools::ToolResult,
    model_family: ModelFamily,
) -> String {
    let _ = conversations.append(conv_id, "assistant", llm_response, Some(&tool_result.tool));
    let _ = conversations.append(
        conv_id,
        "system",
        &format!("Tool result: {}", tool_result.output),
        None,
    );

    let summary = if should_summarize_tool_result(&tool_result.tool) {
        let recent = conversations.get_recent(conv_id, 6).unwrap_or_default();
        let mut summary_msgs = vec![Message {
            role: "system".into(),
            content:
                "Summarize the tool result in one natural sentence without changing numbers, measurements, or facts."
                    .into(),
        }];
        summary_msgs.extend(recent);
        let (summary_msgs, _) = crate::reasoning::apply_reasoning_mode(
            model_family,
            &summary_msgs,
            "",
            InteractionKind::ToolSummary,
        );

        llm.chat(&summary_msgs, Some(128))
            .await
            .unwrap_or_else(|_| tool_result.output.clone())
    } else {
        tool_result.output.clone()
    };
    let sanitized_summary = crate::security::sandbox::sanitize_output(&summary);

    let _ = conversations.append(conv_id, "assistant", &sanitized_summary, None);
    sanitized_summary
}

async fn write_stream_headers(writer: &mut OwnedWriteHalf, status: u16) -> Result<()> {
    let http = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: POST, GET, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n",
        status,
        status_text(status),
    );
    writer.write_all(http.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn write_stream_event(writer: &mut OwnedWriteHalf, event: &serde_json::Value) -> Result<()> {
    writer.write_all(event.to_string().as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn detect_stream_mode(buffer: &str) -> StreamMode {
    let trimmed = buffer.trim_start();
    if trimmed.is_empty() {
        return StreamMode::Undecided;
    }

    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        let inner = inner.trim_start();
        if inner.is_empty() {
            return StreamMode::Undecided;
        }
        if inner.starts_with('{') {
            if looks_like_tool_json(inner) {
                return StreamMode::Tool;
            }
            if inner.len() < 96 {
                return StreamMode::Undecided;
            }
        }
        return StreamMode::Text;
    }

    if trimmed.starts_with('{') {
        if looks_like_tool_json(trimmed) {
            return StreamMode::Tool;
        }
        if trimmed.len() < 96 {
            return StreamMode::Undecided;
        }
    }

    StreamMode::Text
}

fn looks_like_tool_json(text: &str) -> bool {
    text.contains("\"tool\"")
        || text.contains("\"arguments\"")
        || text.contains("\"get_time\"")
        || text.contains("\"get_weather\"")
        || text.contains("\"system_info\"")
        || text.contains("\"home_control\"")
        || text.contains("\"set_timer\"")
}

async fn handle_chat(
    body: Option<&str>,
    llm: &LlmClient,
    tools: &ToolDispatcher,
    memory: &Memory,
    conversations: &ConversationStore,
    current_conv_id: &Mutex<String>,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
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

    let conv_id = parsed
        .get("conversation_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| String::new());
    let conv_id = if conv_id.is_empty() {
        current_conv_id.lock().await.clone()
    } else {
        conv_id
    };

    let turn = match process_chat_turn(
        llm,
        tools,
        memory,
        conversations,
        &conv_id,
        user_text,
        system_prompt,
        max_history,
        model_family,
    )
    .await
    {
        Ok(turn) => turn,
        Err(e) => {
            tracing::error!(error = %e, "chat turn failed");
            return (
                500,
                "application/json",
                format!(r#"{{"error":"chat: {}"}}"#, e),
            );
        }
    };

    let response = serde_json::json!({
        "response": turn.response,
        "tool": turn.tool,
        "conversation_id": turn.conversation_id,
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
    tools: &ToolDispatcher,
    connectivity: &dyn ConnectivityController,
    memory: &Memory,
    conversations: &ConversationStore,
) -> (u16, &'static str, String) {
    let llm_ok = llm.health().await;
    let connectivity_health = connectivity.health().await;
    let mem_count = memory.count().unwrap_or(0);
    let conv_count = conversations.list().map(|l| l.len()).unwrap_or(0);
    let mem_avail = genie_common::tegrastats::mem_available_mb().unwrap_or(0);

    let status = overall_health_status(llm_ok, connectivity_health.state);

    let resp = serde_json::json!({
        "status": status,
        "llm": if llm_ok { "connected" } else { "offline" },
        "memories": mem_count,
        "conversations": conv_count,
        "mem_available_mb": mem_avail,
        "connectivity": connectivity_health,
        "web_search": tools.web_search_status(),
        "version": env!("CARGO_PKG_VERSION"),
    });

    (200, "application/json", resp.to_string())
}

fn overall_health_status(llm_ok: bool, connectivity_state: ConnectivityState) -> &'static str {
    if llm_ok
        && matches!(
            connectivity_state,
            ConnectivityState::Disabled | ConnectivityState::Ready
        )
    {
        "ok"
    } else {
        "degraded"
    }
}

/// GET /api/connectivity — connectivity coprocessor health and capabilities.
async fn handle_connectivity(
    connectivity: &dyn ConnectivityController,
) -> (u16, &'static str, String) {
    let health = connectivity.health().await;
    let capabilities = connectivity.capabilities().await;

    let resp = serde_json::json!({
        "health": health,
        "capabilities": capabilities,
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

/// POST /api/web-search
async fn handle_web_search(
    body: Option<&str>,
    tools: &ToolDispatcher,
) -> (u16, &'static str, String) {
    if !tools.has_web_search() {
        return (
            503,
            "application/json",
            r#"{"error":"web search disabled"}"#.into(),
        );
    }

    let Some(body) = body else {
        return (
            400,
            "application/json",
            r#"{"error":"missing body"}"#.into(),
        );
    };

    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(e) => {
            return (
                400,
                "application/json",
                format!(r#"{{"error":"invalid JSON: {}"}}"#, e),
            );
        }
    };

    let query = parsed
        .get("query")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if query.trim().is_empty() {
        return (
            400,
            "application/json",
            r#"{"error":"missing query"}"#.into(),
        );
    }

    let limit = parsed
        .get("limit")
        .and_then(|value| value.as_u64())
        .unwrap_or(3)
        .clamp(1, 5);
    let fresh = parsed
        .get("fresh")
        .or_else(|| parsed.get("cache_bypass"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let call = crate::tools::ToolCall {
        name: "web_search".into(),
        arguments: serde_json::json!({
            "query": query,
            "limit": limit,
            "fresh": fresh,
        }),
    };
    let result = tools.execute(&call).await;
    let status = if result.success { 200 } else { 502 };
    let body = serde_json::json!({
        "tool": result.tool,
        "success": result.success,
        "response": result.output,
    });

    (status, "application/json", body.to_string())
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
    model_family: ModelFamily,
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

    if let Some(call) = crate::tools::quick::route_for_available_tools(
        &user_text,
        tools.has_home_automation(),
        tools.has_web_search(),
    ) {
        let tool_result = tools.execute(&call).await;
        let response = if tool_result.success {
            tool_result.output
        } else {
            format!("{} failed: {}", tool_result.tool, tool_result.output)
        };
        let sanitized = crate::security::sandbox::sanitize_output(&response);
        crate::memory::extract::extract_and_store(memory, &user_text);
        return openai_chat_response(model, &sanitized);
    }

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
    let (llm_messages, decision) = crate::reasoning::apply_reasoning_mode(
        model_family,
        &llm_messages,
        &user_text,
        InteractionKind::OpenAiBridge,
    );
    tracing::debug!(
        ?model_family,
        ?decision,
        "applied reasoning mode for OpenAI bridge"
    );

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
            let (summary_msgs, _) = crate::reasoning::apply_reasoning_mode(
                model_family,
                &summary_msgs,
                "",
                InteractionKind::ToolSummary,
            );

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

    openai_chat_response(model, &sanitized)
}

fn openai_chat_response(model: &str, content: &str) -> (u16, &'static str, String) {
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
                "content": content,
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
    !matches!(
        tool_name,
        "system_info" | "memory_recall" | "memory_status" | "memory_store" | "memory_forget"
    )
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
    use super::{
        ConnectivityState, StreamMode, detect_stream_mode, handle_web_search,
        overall_health_status, should_summarize_tool_result,
    };
    use crate::tools::ToolDispatcher;
    use genie_common::config::WebSearchConfig;

    #[test]
    fn system_info_tool_preserves_raw_output() {
        assert!(!should_summarize_tool_result("system_info"));
    }

    #[test]
    fn memory_tools_preserve_raw_output() {
        assert!(!should_summarize_tool_result("memory_recall"));
        assert!(!should_summarize_tool_result("memory_status"));
        assert!(!should_summarize_tool_result("memory_store"));
        assert!(!should_summarize_tool_result("memory_forget"));
    }

    #[test]
    fn other_tools_can_still_be_summarized() {
        assert!(should_summarize_tool_result("home_control"));
        assert!(should_summarize_tool_result("hello_world"));
    }

    #[test]
    fn plain_text_streams_immediately() {
        assert_eq!(detect_stream_mode("Hello there"), StreamMode::Text);
    }

    #[test]
    fn tool_json_is_buffered_for_dispatch() {
        assert_eq!(
            detect_stream_mode(r#"{"tool":"get_time","arguments":{}}"#),
            StreamMode::Tool
        );
    }

    #[test]
    fn short_json_waits_for_more_context() {
        assert_eq!(detect_stream_mode(r#"{"fo"#), StreamMode::Undecided);
    }

    #[test]
    fn overall_health_is_ok_when_llm_is_up_and_connectivity_is_disabled() {
        assert_eq!(
            overall_health_status(true, ConnectivityState::Disabled),
            "ok"
        );
    }

    #[test]
    fn overall_health_is_ok_when_llm_is_up_and_connectivity_is_ready() {
        assert_eq!(overall_health_status(true, ConnectivityState::Ready), "ok");
    }

    #[test]
    fn overall_health_is_degraded_when_connectivity_is_offline() {
        assert_eq!(
            overall_health_status(true, ConnectivityState::Offline),
            "degraded"
        );
    }

    #[tokio::test]
    async fn web_search_endpoint_rejects_empty_query() {
        let tools = ToolDispatcher::new(None);
        let (status, _, body) = handle_web_search(Some(r#"{"query":""}"#), &tools).await;

        assert_eq!(status, 400);
        assert!(body.contains("missing query"));
    }

    #[tokio::test]
    async fn web_search_endpoint_respects_disabled_config() {
        let mut config = WebSearchConfig::default();
        config.enabled = false;
        let tools = ToolDispatcher::new(None).with_web_search_config(config);
        let (status, _, body) =
            handle_web_search(Some(r#"{"query":"ESP32-C6 Thread"}"#), &tools).await;

        assert_eq!(status, 503);
        assert!(body.contains("web search disabled"));
    }
}
