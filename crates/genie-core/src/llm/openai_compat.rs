use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Raw HTTP client for local OpenAI-compatible chat completion backends.
///
/// Supports both blocking completion and streaming (SSE).
/// No reqwest/hyper — raw HTTP over TCP to localhost.
pub struct OpenAiCompatClient {
    backend_name: &'static str,
    host: String,
    port: u16,
    request_profile: RequestProfile,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RequestProfile {
    Generic,
    GenieAiRuntime,
}

#[derive(Debug)]
struct PreparedChatBody {
    body: String,
    compacted: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) think: Option<bool>,
    /// JSON schema constraint for backends that support OpenAI-compatible
    /// `response_format`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub format_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

impl ResponseFormat {
    /// Force JSON output (any valid JSON).
    pub fn json() -> Self {
        Self {
            format_type: "json_object".into(),
            schema: None,
        }
    }

    /// Force JSON output matching a specific schema.
    pub fn json_schema(schema: serde_json::Value) -> Self {
        Self {
            format_type: "json_schema".into(),
            schema: Some(schema),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatResponse {
    pub(crate) choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    pub(crate) message: Option<Message>,
    pub(crate) delta: Option<Delta>,
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Delta {
    pub(crate) content: Option<String>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: String,
}

impl RequestProfile {
    fn generic() -> Self {
        Self::Generic
    }

    pub(crate) fn genie_ai_runtime() -> Self {
        Self::GenieAiRuntime
    }

    fn prepare_body(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        stream: bool,
        response_format: Option<ResponseFormat>,
    ) -> Result<PreparedChatBody> {
        let body = self.serialize_body(messages, max_tokens, stream, response_format.clone())?;
        if !matches!(self, Self::GenieAiRuntime) || body.len() <= GENIE_RUNTIME_MAX_BODY_BYTES {
            return Ok(PreparedChatBody {
                body,
                compacted: false,
            });
        }

        let compacted_messages = self.compact_messages(messages, GENIE_RUNTIME_MAX_BODY_BYTES);
        let compacted_body =
            self.serialize_body(&compacted_messages, max_tokens, stream, response_format)?;
        Ok(PreparedChatBody {
            body: compacted_body,
            compacted: true,
        })
    }

    fn serialize_body(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        stream: bool,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        let request = ChatRequest {
            model: self.model().into(),
            messages: messages.to_vec(),
            max_tokens,
            temperature: Some(0.7),
            stream,
            think: self.think_override(),
            response_format,
        };
        Ok(serde_json::to_string(&request)?)
    }

    fn model(self) -> &'static str {
        match self {
            Self::Generic => "default",
            Self::GenieAiRuntime => "jetson-llm",
        }
    }

    fn think_override(self) -> Option<bool> {
        match self {
            Self::Generic => None,
            Self::GenieAiRuntime => Some(false),
        }
    }

    fn compact_messages(&self, messages: &[Message], max_body_bytes: usize) -> Vec<Message> {
        match self {
            Self::Generic => messages.to_vec(),
            Self::GenieAiRuntime => compact_genie_runtime_messages(messages, max_body_bytes),
        }
    }
}

const GENIE_RUNTIME_MAX_BODY_BYTES: usize = 24 * 1024;
const GENIE_RUNTIME_BODY_OVERHEAD_BYTES: usize = 768;
const GENIE_RUNTIME_COMPACT_SYSTEM: &str =
    "You are GeniePod Home. Answer the user's latest request directly and concisely.";

impl OpenAiCompatClient {
    pub fn new(backend_name: &'static str, host: &str, port: u16) -> Self {
        Self::new_with_profile(backend_name, host, port, RequestProfile::generic())
    }

    pub(crate) fn new_with_profile(
        backend_name: &'static str,
        host: &str,
        port: u16,
        request_profile: RequestProfile,
    ) -> Self {
        Self {
            backend_name,
            host: host.to_string(),
            port,
            request_profile,
        }
    }

    pub fn from_url(backend_name: &'static str, url: &str) -> Self {
        Self::from_url_with_profile(backend_name, url, RequestProfile::generic())
    }

    pub(crate) fn from_url_with_profile(
        backend_name: &'static str,
        url: &str,
        request_profile: RequestProfile,
    ) -> Self {
        let stripped = url.strip_prefix("http://").unwrap_or(url);
        let (host_port, _) = stripped.split_once('/').unwrap_or((stripped, ""));
        let (host, port_str) = host_port.split_once(':').unwrap_or((host_port, "8080"));
        let port = port_str.parse().unwrap_or(8080);
        Self {
            backend_name,
            host: host.to_string(),
            port,
            request_profile,
        }
    }

    pub fn backend_name(&self) -> &str {
        self.backend_name
    }

    /// Send a chat completion request, return the full response.
    pub async fn chat(&self, messages: &[Message], max_tokens: Option<u32>) -> Result<String> {
        self.chat_with_format(messages, max_tokens, None).await
    }

    /// Send a chat request forcing JSON output.
    /// Uses backend response-format support when available.
    /// Eliminates tool-calling parsing failures.
    pub async fn chat_json(&self, messages: &[Message], max_tokens: Option<u32>) -> Result<String> {
        self.chat_with_format(messages, max_tokens, Some(ResponseFormat::json()))
            .await
    }

    /// Send a chat request with optional response format constraint.
    pub async fn chat_with_format(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        match self
            .chat_with_format_once(messages, max_tokens, response_format.clone())
            .await
        {
            Ok(content) => Ok(content),
            Err(err) if should_retry_without_system_role(messages, &err.to_string()) => {
                let flattened = flatten_system_into_first_user(messages);
                self.chat_with_format_once(&flattened, max_tokens, response_format)
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn chat_with_format_once(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
    ) -> Result<String> {
        let prepared =
            self.request_profile
                .prepare_body(messages, max_tokens, false, response_format)?;
        if prepared.compacted {
            tracing::debug!(
                backend = self.backend_name,
                request_bytes = prepared.body.len(),
                "compacted OpenAI-compatible chat request"
            );
        }

        let response = self
            .http_post("/v1/chat/completions", &prepared.body)
            .await?;
        if response.status == 0 && response.body.trim().is_empty() {
            anyhow::bail!(
                "{} closed connection before HTTP status (request_bytes={})",
                self.backend_name,
                prepared.body.len()
            );
        }
        if response.status != 200 {
            anyhow::bail!(
                "{} {}: {}",
                self.backend_name,
                response.status,
                backend_error_message(&response.body)
            );
        }

        let chat_resp: ChatResponse = serde_json::from_str(&response.body).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse {} response: {}; body: {}",
                self.backend_name,
                e,
                truncate_body(&response.body)
            )
        })?;
        let content = chat_resp
            .choices
            .first()
            .and_then(|c| c.message.as_ref())
            .map(|m| m.content.clone())
            .unwrap_or_default();

        Ok(content)
    }

    /// Send a streaming chat request. Calls `on_token` for each token as it arrives.
    /// Returns the full assembled response.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        let prepared = self
            .request_profile
            .prepare_body(messages, max_tokens, true, None)?;
        if prepared.compacted {
            tracing::debug!(
                backend = self.backend_name,
                request_bytes = prepared.body.len(),
                "compacted OpenAI-compatible streaming chat request"
            );
        }

        let addr = format!("{}:{}", self.host, self.port);
        let stream = TcpStream::connect(&addr).await?;
        let (reader, mut writer) = stream.into_split();

        let http_req = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: text/event-stream\r\n\r\n{}",
            addr,
            prepared.body.len(),
            prepared.body,
        );

        writer.write_all(http_req.as_bytes()).await?;

        let mut lines = BufReader::new(reader).lines();
        let mut full_response = String::new();
        let mut status = 0;
        let mut headers_done = false;

        while let Some(line) = lines.next_line().await? {
            // Skip HTTP headers.
            if !headers_done {
                if line.starts_with("HTTP/") {
                    status = parse_status_line(&line);
                    continue;
                }
                if line.is_empty() {
                    headers_done = true;
                    if status != 200 {
                        let mut error_body = String::new();
                        while let Some(line) = lines.next_line().await? {
                            error_body.push_str(&line);
                        }
                        anyhow::bail!(
                            "{} {}: {}",
                            self.backend_name,
                            status,
                            backend_error_message(&error_body)
                        );
                    }
                }
                continue;
            }

            // Parse SSE data lines.
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    break;
                }

                if let Ok(chunk) = serde_json::from_str::<ChatResponse>(data)
                    && let Some(choice) = chunk.choices.first()
                {
                    if let Some(delta) = &choice.delta
                        && let Some(content) = &delta.content
                    {
                        on_token(content);
                        full_response.push_str(content);
                    }
                    if choice.finish_reason.is_some() {
                        break;
                    }
                }
            }
        }

        if !headers_done {
            anyhow::bail!(
                "{} closed streaming connection before HTTP status",
                self.backend_name
            );
        }

        Ok(full_response)
    }

    /// Check if the LLM server is reachable.
    pub async fn health(&self) -> bool {
        matches!(self.http_get("/health").await, Ok(resp) if resp.status == 200)
    }

    async fn http_post(&self, path: &str, body: &str) -> Result<HttpResponse> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            TcpStream::connect(&addr),
        )
        .await??;

        let (reader, mut writer) = stream.into_split();

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            path,
            addr,
            body.len(),
            body
        );

        writer.write_all(request.as_bytes()).await?;

        let mut buf_reader = BufReader::new(reader);
        let mut response = String::new();
        let mut headers_done = false;
        let mut content_length: usize = 0;
        let mut status = 0;

        // Read headers.
        loop {
            let mut line = String::new();
            buf_reader.read_line(&mut line).await?;
            if line.starts_with("HTTP/") {
                status = parse_status_line(&line);
                continue;
            }
            if line.trim().is_empty() {
                headers_done = true;
                break;
            }
            if let Some(val) = line.to_lowercase().strip_prefix("content-length: ") {
                content_length = val.trim().parse().unwrap_or(0);
            }
        }

        if headers_done && content_length > 0 {
            let mut buf = vec![0u8; content_length];
            tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut buf).await?;
            response = String::from_utf8_lossy(&buf).to_string();
        } else if headers_done {
            // Chunked or unknown length — read until EOF.
            tokio::io::AsyncReadExt::read_to_string(&mut buf_reader, &mut response).await?;
        }

        Ok(HttpResponse {
            status,
            body: response,
        })
    }

    async fn http_get(&self, path: &str) -> Result<HttpResponse> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream =
            tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr))
                .await??;

        let (reader, mut writer) = stream.into_split();

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, addr
        );
        writer.write_all(request.as_bytes()).await?;

        let mut buf_reader = BufReader::new(reader);
        let mut body = String::new();
        let mut status = 0;

        loop {
            let mut line = String::new();
            buf_reader.read_line(&mut line).await?;
            if line.starts_with("HTTP/") {
                status = parse_status_line(&line);
                continue;
            }
            if line.trim().is_empty() {
                break;
            }
        }

        tokio::io::AsyncReadExt::read_to_string(&mut buf_reader, &mut body).await?;
        Ok(HttpResponse { status, body })
    }
}

fn parse_status_line(line: &str) -> u16 {
    line.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn compact_genie_runtime_messages(messages: &[Message], max_body_bytes: usize) -> Vec<Message> {
    let system_messages = messages
        .iter()
        .filter(|m| m.role == "system")
        .cloned()
        .collect::<Vec<_>>();
    let has_system_context = !system_messages.is_empty();

    let Some(latest_idx) = messages
        .iter()
        .rposition(|m| m.role == "user")
        .or_else(|| messages.iter().rposition(|m| m.role != "system"))
    else {
        return system_messages;
    };

    let latest_source = &messages[latest_idx];
    let latest = Message {
        role: "user".into(),
        content: if has_system_context {
            latest_source.content.clone()
        } else {
            format!(
                "{}\n\n{}",
                GENIE_RUNTIME_COMPACT_SYSTEM, latest_source.content
            )
        },
    };

    let mut compacted = system_messages;
    let body_budget = max_body_bytes.saturating_sub(GENIE_RUNTIME_BODY_OVERHEAD_BYTES);
    let mut estimated_bytes = estimate_messages_bytes(&compacted) + estimate_message_bytes(&latest);
    let mut retained_history = Vec::new();

    for message in messages[..latest_idx]
        .iter()
        .rev()
        .filter(|m| m.role != "system")
    {
        let message_bytes = estimate_message_bytes(message);
        if estimated_bytes + message_bytes > body_budget {
            break;
        }
        retained_history.push(message.clone());
        estimated_bytes += message_bytes;
    }

    retained_history.reverse();
    compacted.extend(retained_history);
    compacted.push(latest);
    compacted
}

fn estimate_messages_bytes(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_bytes).sum()
}

fn estimate_message_bytes(message: &Message) -> usize {
    message.role.len() + message.content.len() + 32
}

fn should_retry_without_system_role(messages: &[Message], err: &str) -> bool {
    messages.iter().any(|m| m.role == "system") && system_role_not_supported(err)
}

fn system_role_not_supported(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("only user and assistant roles are supported")
        || lower.contains("system role not supported")
        || lower.contains("unsupported role") && lower.contains("system")
        || lower.contains("does not support") && lower.contains("system")
}

fn flatten_system_into_first_user(messages: &[Message]) -> Vec<Message> {
    let system_text = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.trim())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut flattened = messages
        .iter()
        .filter(|m| m.role != "system")
        .cloned()
        .collect::<Vec<_>>();

    if system_text.is_empty() {
        return flattened;
    }

    if let Some(first_user) = flattened.iter_mut().find(|m| m.role == "user") {
        first_user.content = format!(
            "System instructions:\n{}\n\n{}",
            system_text, first_user.content
        );
    } else {
        flattened.insert(
            0,
            Message {
                role: "user".into(),
                content: format!("System instructions:\n{}", system_text),
            },
        );
    }

    flattened
}

fn backend_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| {
            json.get("error")
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    json.get("message")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        })
        .unwrap_or_else(|| truncate_body(body))
}

fn truncate_body(body: &str) -> String {
    const MAX_LEN: usize = 240;
    let trimmed = body.trim();
    if trimmed.len() <= MAX_LEN {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..MAX_LEN])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url() {
        let client = OpenAiCompatClient::from_url("test-backend", "http://127.0.0.1:8080/v1");
        assert_eq!(client.host, "127.0.0.1");
        assert_eq!(client.port, 8080);
    }

    #[test]
    fn serialize_chat_request() {
        let req = ChatRequest {
            model: "nemotron-4b".into(),
            messages: vec![Message {
                role: "user".into(),
                content: "turn on the lights".into(),
            }],
            max_tokens: Some(256),
            temperature: Some(0.7),
            stream: false,
            think: None,
            response_format: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("nemotron-4b"));
        assert!(json.contains("turn on the lights"));
    }

    #[test]
    fn generic_request_profile_keeps_full_default_body() {
        let profile = RequestProfile::generic();
        let messages = vec![
            Message {
                role: "system".into(),
                content: "keep this full instruction".repeat(128),
            },
            Message {
                role: "user".into(),
                content: "turn on the lights".into(),
            },
        ];

        let prepared = profile
            .prepare_body(&messages, Some(64), false, None)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();

        assert!(!prepared.compacted);
        assert_eq!(json["model"], "default");
        assert!(
            json["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("keep this full instruction")
        );
    }

    #[test]
    fn genie_runtime_profile_compacts_large_core_prompt() {
        let profile = RequestProfile::genie_ai_runtime();
        let messages = vec![
            Message {
                role: "system".into(),
                content: "tool manifest memory_recall household context ".repeat(128),
            },
            Message {
                role: "assistant".into(),
                content: "older assistant turn ".repeat(2_000),
            },
            Message {
                role: "user".into(),
                content: "Say hello from the GeniePod web UI.".into(),
            },
        ];

        let prepared = profile
            .prepare_body(&messages, Some(64), false, None)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        let serialized_messages = json["messages"].to_string();

        assert!(prepared.compacted);
        assert_eq!(json["model"], "jetson-llm");
        assert_eq!(json["think"], false);
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);
        assert_eq!(json["messages"][0]["role"], "system");
        assert!(serialized_messages.contains("memory_recall"));
        assert!(serialized_messages.contains("household context"));
        assert!(serialized_messages.contains("Say hello from the GeniePod web UI."));
        assert!(!serialized_messages.contains("GeniePod Home"));
        assert!(!serialized_messages.contains("older assistant turn"));
        assert!(prepared.body.len() < GENIE_RUNTIME_MAX_BODY_BYTES);
    }

    #[test]
    fn genie_runtime_profile_keeps_runtime_prompt_under_expanded_budget() {
        let profile = RequestProfile::genie_ai_runtime();
        let messages = vec![
            Message {
                role: "system".into(),
                content: "memory_recall tool manifest household preference ".repeat(160),
            },
            Message {
                role: "user".into(),
                content: "What is my name?".into(),
            },
        ];

        let prepared = profile
            .prepare_body(&messages, Some(64), false, None)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        let serialized_messages = json["messages"].to_string();

        assert!(!prepared.compacted);
        assert!(prepared.body.len() > 4 * 1024);
        assert!(prepared.body.len() < GENIE_RUNTIME_MAX_BODY_BYTES);
        assert!(serialized_messages.contains("memory_recall"));
        assert!(serialized_messages.contains("What is my name?"));
    }

    #[test]
    fn genie_runtime_compaction_falls_back_to_latest_non_system_message() {
        let messages = vec![
            Message {
                role: "system".into(),
                content: "system".into(),
            },
            Message {
                role: "assistant".into(),
                content: "assistant fallback".into(),
            },
        ];

        let compacted = compact_genie_runtime_messages(&messages, GENIE_RUNTIME_MAX_BODY_BYTES);
        assert_eq!(compacted.len(), 2);
        assert_eq!(compacted[0].role, "system");
        assert_eq!(compacted[1].role, "user");
        assert!(compacted[1].content.contains("assistant fallback"));
    }

    #[test]
    fn deserialize_chat_response() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"Done! Lights are on."},"finish_reason":"stop"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.as_ref().unwrap().content,
            "Done! Lights are on."
        );
    }

    #[test]
    fn parse_http_status() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK"), 200);
        assert_eq!(parse_status_line("HTTP/1.1 503 Service Unavailable"), 503);
    }

    #[test]
    fn flatten_system_prompt_into_first_user_message() {
        let messages = vec![
            Message {
                role: "system".into(),
                content: "Be concise.".into(),
            },
            Message {
                role: "user".into(),
                content: "What time is it?".into(),
            },
        ];

        let flattened = flatten_system_into_first_user(&messages);
        assert_eq!(flattened.len(), 1);
        assert_eq!(flattened[0].role, "user");
        assert!(flattened[0].content.contains("Be concise."));
        assert!(flattened[0].content.contains("What time is it?"));
    }

    #[test]
    fn detect_system_role_error_message() {
        assert!(system_role_not_supported(
            "llama.cpp 400: Only user and assistant roles are supported!"
        ));
        assert!(system_role_not_supported(
            "Error rendering prompt: system role not supported"
        ));
    }
}
