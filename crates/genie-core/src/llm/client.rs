use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Client for llama.cpp `--server` OpenAI-compatible API.
///
/// Supports both blocking completion and streaming (SSE).
/// No reqwest/hyper — raw HTTP over TCP to localhost.
pub struct LlmClient {
    host: String,
    port: u16,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub stream: bool,
    /// JSON schema constraint — forces the LLM to output valid JSON matching this schema.
    /// Supported by llama.cpp `--server` via the `response_format` parameter.
    /// When set, eliminates tool-calling parsing failures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
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
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: Option<Message>,
    pub delta: Option<Delta>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Delta {
    pub content: Option<String>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: String,
}

impl LlmClient {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
        }
    }

    pub fn from_url(url: &str) -> Self {
        let stripped = url.strip_prefix("http://").unwrap_or(url);
        let (host_port, _) = stripped.split_once('/').unwrap_or((stripped, ""));
        let (host, port_str) = host_port.split_once(':').unwrap_or((host_port, "8080"));
        let port = port_str.parse().unwrap_or(8080);
        Self {
            host: host.to_string(),
            port,
        }
    }

    /// Send a chat completion request, return the full response.
    pub async fn chat(&self, messages: &[Message], max_tokens: Option<u32>) -> Result<String> {
        self.chat_with_format(messages, max_tokens, None).await
    }

    /// Send a chat request forcing JSON output.
    /// Uses llama.cpp's grammar constraint to guarantee valid JSON.
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
        let request = ChatRequest {
            model: "default".into(),
            messages: messages.to_vec(),
            max_tokens,
            temperature: Some(0.7),
            stream: false,
            response_format,
        };

        let body = serde_json::to_string(&request)?;
        let response = self.http_post("/v1/chat/completions", &body).await?;
        if response.status != 200 {
            anyhow::bail!(
                "llama.cpp {}: {}",
                response.status,
                llama_error_message(&response.body)
            );
        }

        let chat_resp: ChatResponse = serde_json::from_str(&response.body).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse llama.cpp response: {}; body: {}",
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
    pub async fn chat_stream<F>(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let request = ChatRequest {
            model: "default".into(),
            messages: messages.to_vec(),
            max_tokens,
            temperature: Some(0.7),
            stream: true,
            response_format: None,
        };

        let body = serde_json::to_string(&request)?;
        let addr = format!("{}:{}", self.host, self.port);
        let stream = TcpStream::connect(&addr).await?;
        let (reader, mut writer) = stream.into_split();

        let http_req = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: text/event-stream\r\n\r\n{}",
            addr,
            body.len(),
            body,
        );

        writer.write_all(http_req.as_bytes()).await?;

        let mut lines = BufReader::new(reader).lines();
        let mut full_response = String::new();
        let mut status = 200;
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
                        anyhow::bail!("llama.cpp {}: {}", status, llama_error_message(&error_body));
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

fn llama_error_message(body: &str) -> String {
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
        let client = LlmClient::from_url("http://127.0.0.1:8080/v1");
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
            response_format: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("nemotron-4b"));
        assert!(json.contains("turn on the lights"));
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
