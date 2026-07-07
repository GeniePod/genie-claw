use std::time::Duration;

use anyhow::Result;
use genie_common::http::{HttpReadError, HttpResponse, HttpResponseLimits, read_response};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::LlmRequestHints;

/// Network timeouts applied to every backend read, write, and connect.
///
/// Before issue #181 the only bound was a `connect` timeout on the
/// non-streaming path; the streaming `connect` and *all* reads were unbounded,
/// so a backend that accepted a connection and then stalled mid-response held
/// the caller (and the server's chat turn lock) forever. These timeouts bound
/// each I/O step so a hung backend fails the turn instead of wedging the daemon.
#[derive(Debug, Clone, Copy)]
pub struct LlmTimeouts {
    /// Maximum time to establish the TCP connection.
    pub connect: Duration,
    /// Maximum idle time for an incremental read (an SSE token line or an HTTP
    /// header line) or a write before it is abandoned.
    pub read: Duration,
    /// Maximum time for a non-streaming completion's single response-body read,
    /// which spans the whole generation. Streaming relies on `read` (idle)
    /// instead, so a long but live stream is never cut off.
    pub request: Duration,
}

impl Default for LlmTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(60),
            request: Duration::from_secs(120),
        }
    }
}

impl LlmTimeouts {
    /// Build from whole-second config values, clamping zeros to a 1s floor so a
    /// misconfigured `0` can never disable the bound (which would reintroduce
    /// the issue #181 hang).
    pub fn from_secs(connect: u64, read: u64, request: u64) -> Self {
        Self {
            connect: Duration::from_secs(connect.max(1)),
            read: Duration::from_secs(read.max(1)),
            request: Duration::from_secs(request.max(1)),
        }
    }
}

/// Raw HTTP client for local OpenAI-compatible chat completion backends.
///
/// Supports both blocking completion and streaming (SSE).
/// No reqwest/hyper — raw HTTP over TCP to localhost.
pub struct OpenAiCompatClient {
    backend_name: &'static str,
    host: String,
    port: u16,
    request_profile: RequestProfile,
    timeouts: LlmTimeouts,
    auth: Option<AuthorizationHeader>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AuthorizationHeader {
    value: String,
}

impl std::fmt::Debug for AuthorizationHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizationHeader([redacted])")
    }
}

impl AuthorizationHeader {
    pub(crate) fn bearer(token: impl AsRef<str>) -> Option<Self> {
        let token = token.as_ref().trim();
        if token.is_empty() || token.contains(['\r', '\n']) {
            return None;
        }
        Some(Self {
            value: format!("Bearer {token}"),
        })
    }

    fn header_line(&self) -> String {
        format!("Authorization: {}\r\n", self.value)
    }
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
    pub(crate) conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) think: Option<bool>,
    /// JSON schema constraint for backends that support OpenAI-compatible
    /// `response_format`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) nvext: Option<NvExt>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct NvExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent_hints: Option<AgentHints>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentHints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) osl: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speculative_prefill: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CacheControl {
    #[serde(rename = "type")]
    pub(crate) cache_type: &'static str,
    pub(crate) ttl: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prefix_bytes: Option<usize>,
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

impl RequestProfile {
    pub(crate) fn generic() -> Self {
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
        hints: Option<&LlmRequestHints>,
    ) -> Result<PreparedChatBody> {
        let body =
            self.serialize_body(messages, max_tokens, stream, response_format.clone(), hints)?;
        if !matches!(self, Self::GenieAiRuntime) || body.len() <= GENIE_RUNTIME_MAX_BODY_BYTES {
            return Ok(PreparedChatBody {
                body,
                compacted: false,
            });
        }

        let compacted_messages = self.compact_messages(messages, GENIE_RUNTIME_MAX_BODY_BYTES);
        let compacted_body = self.serialize_body(
            &compacted_messages,
            max_tokens,
            stream,
            response_format,
            hints,
        )?;
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
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        let conversation_id = self.runtime_session_id(hints);
        let request = ChatRequest {
            model: self.model().into(),
            messages: messages.to_vec(),
            max_tokens,
            temperature: Some(0.7),
            stream,
            conversation_id,
            think: self.think_override(),
            response_format,
            nvext: self.nvext(messages, hints),
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

    fn runtime_session_id(&self, hints: Option<&LlmRequestHints>) -> Option<String> {
        match self {
            Self::Generic => None,
            Self::GenieAiRuntime => hints
                .and_then(|h| h.session_id.as_deref())
                .and_then(normalize_runtime_session_id),
        }
    }

    fn nvext(&self, messages: &[Message], hints: Option<&LlmRequestHints>) -> Option<NvExt> {
        match self {
            Self::Generic => None,
            Self::GenieAiRuntime => build_nvext(messages, hints),
        }
    }
}

const GENIE_RUNTIME_MAX_BODY_BYTES: usize = 4 * 1024;
const GENIE_RUNTIME_BODY_OVERHEAD_BYTES: usize = 512;
const GENIE_RUNTIME_CONTEXT_MAX_BYTES: usize = 900;
/// Cap outbound LLM HTTP bodies so a buggy backend cannot OOM genie-core.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Cap a single SSE/header line so a backend cannot stream an endless line
/// between newlines and grow RSS while the idle read timeout ticks (issue #195
/// class, outbound path).
const MAX_SSE_LINE_BYTES: usize = 64 * 1024;
const GENIE_RUNTIME_COMPACT_SYSTEM: &str = "You are GenieClaw, a local home AI native to NVIDIA Jetson Orin 8GB. Answer the user's latest request directly and concisely.";
const GENIE_RUNTIME_COMPACT_SYSTEM_PREFIX: &str = "You are GenieClaw, a local home AI native to NVIDIA Jetson Orin 8GB. Reply briefly for voice. Use a tool only when required. Tool calls must be ONLY JSON: {\"tool\":\"tool_name\",\"arguments\":{}}. No markdown.";
const SYSTEM_PROMPT_PREFIX_CACHE_SCOPE: &str = "system_prompt_prefix";
const SYSTEM_PROMPT_PREFIX_CACHE_KEY_PREFIX: &str = "system-prompt-prefix:";
const SYSTEM_PROMPT_PREFIX_CACHE_MARKERS: [&str; 2] = [
    "\n\nRelevant household context:\n",
    "\n\nHousehold context:\n",
];

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
        Self::new_with_profile_and_timeouts(
            backend_name,
            host,
            port,
            request_profile,
            LlmTimeouts::default(),
        )
    }

    pub(crate) fn new_with_profile_and_timeouts(
        backend_name: &'static str,
        host: &str,
        port: u16,
        request_profile: RequestProfile,
        timeouts: LlmTimeouts,
    ) -> Self {
        Self {
            backend_name,
            host: host.to_string(),
            port,
            request_profile,
            timeouts,
            auth: None,
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
        Self::from_url_with_profile_and_timeouts(
            backend_name,
            url,
            request_profile,
            LlmTimeouts::default(),
        )
    }

    pub(crate) fn from_url_with_profile_and_timeouts(
        backend_name: &'static str,
        url: &str,
        request_profile: RequestProfile,
        timeouts: LlmTimeouts,
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
            timeouts,
            auth: None,
        }
    }

    pub fn backend_name(&self) -> &str {
        self.backend_name
    }

    pub(crate) fn with_bearer_token(mut self, token: impl AsRef<str>) -> Self {
        self.auth = AuthorizationHeader::bearer(token);
        self
    }

    pub(crate) fn with_bearer_token_env(mut self, env_var: impl AsRef<str>) -> Self {
        let env_var = env_var.as_ref().trim();
        if let Ok(token) = std::env::var(env_var) {
            self.auth = AuthorizationHeader::bearer(token);
        }
        self
    }

    fn authorization_header(&self) -> String {
        self.auth
            .as_ref()
            .map(AuthorizationHeader::header_line)
            .unwrap_or_default()
    }

    /// Connect to the backend, bounded by the configured connect timeout.
    ///
    /// The streaming path previously called `TcpStream::connect` with no
    /// timeout (issue #181); routing every connect through here guarantees a
    /// bound on all paths.
    async fn connect(&self, addr: &str) -> Result<TcpStream> {
        match tokio::time::timeout(self.timeouts.connect, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => Err(anyhow::anyhow!(
                "{} connect failed ({addr}): {e}",
                self.backend_name
            )),
            Err(_) => Err(anyhow::anyhow!(
                "{} connect timed out after {}s ({addr})",
                self.backend_name,
                self.timeouts.connect.as_secs()
            )),
        }
    }

    /// Run a single read/write future under a timeout, mapping both the I/O
    /// error and the elapsed-timeout into a descriptive `anyhow` error.
    ///
    /// `op` labels the step in the resulting error (e.g. "stream read") so a
    /// wedged backend is diagnosable from logs.
    async fn timed<F, T>(&self, budget: Duration, op: &'static str, fut: F) -> Result<T>
    where
        F: std::future::Future<Output = std::io::Result<T>>,
    {
        match tokio::time::timeout(budget, fut).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(anyhow::anyhow!("{} {op} failed: {e}", self.backend_name)),
            Err(_) => Err(anyhow::anyhow!(
                "{} {op} timed out after {}s",
                self.backend_name,
                budget.as_secs()
            )),
        }
    }

    async fn timed_result<F, T>(&self, budget: Duration, op: &'static str, fut: F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        match tokio::time::timeout(budget, fut).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "{} {op} timed out after {}s",
                self.backend_name,
                budget.as_secs()
            )),
        }
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
        let prepared = self.request_profile.prepare_body(
            messages,
            max_tokens,
            false,
            response_format,
            None,
        )?;
        self.chat_with_prepared_body(prepared).await
    }

    pub async fn chat_with_format_and_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        match self
            .chat_with_format_once_and_hints(messages, max_tokens, response_format.clone(), hints)
            .await
        {
            Ok(content) => Ok(content),
            Err(err) if should_retry_without_system_role(messages, &err.to_string()) => {
                let flattened = flatten_system_into_first_user(messages);
                self.chat_with_format_once_and_hints(&flattened, max_tokens, response_format, hints)
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn chat_with_format_once_and_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        response_format: Option<ResponseFormat>,
        hints: Option<&LlmRequestHints>,
    ) -> Result<String> {
        let prepared = self.request_profile.prepare_body(
            messages,
            max_tokens,
            false,
            response_format,
            hints,
        )?;
        self.chat_with_prepared_body(prepared).await
    }

    async fn chat_with_prepared_body(&self, prepared: PreparedChatBody) -> Result<String> {
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
            .prepare_body(messages, max_tokens, true, None, None)?;
        self.chat_stream_with_prepared_body(prepared, on_token)
            .await
    }

    pub async fn chat_stream_with_hints(
        &self,
        messages: &[Message],
        max_tokens: Option<u32>,
        hints: Option<&LlmRequestHints>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        let prepared = self
            .request_profile
            .prepare_body(messages, max_tokens, true, None, hints)?;
        self.chat_stream_with_prepared_body(prepared, on_token)
            .await
    }

    async fn chat_stream_with_prepared_body(
        &self,
        prepared: PreparedChatBody,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        if prepared.compacted {
            tracing::debug!(
                backend = self.backend_name,
                request_bytes = prepared.body.len(),
                "compacted OpenAI-compatible streaming chat request"
            );
        }

        let addr = format!("{}:{}", self.host, self.port);
        let stream = self.connect(&addr).await?;
        let (reader, mut writer) = stream.into_split();

        let http_req = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: text/event-stream\r\n{}\r\n{}",
            addr,
            prepared.body.len(),
            self.authorization_header(),
            prepared.body,
        );

        self.timed(
            self.timeouts.read,
            "stream request write",
            writer.write_all(http_req.as_bytes()),
        )
        .await?;

        let mut lines = BufReader::new(reader).lines();
        let mut full_response = String::new();
        let mut status = 0;
        let mut headers_done = false;

        // Each `next_line()` is bounded by the idle (`read`) timeout: if the
        // backend stalls between SSE chunks, the read fails instead of hanging
        // forever and holding the chat turn lock (issue #181).
        while let Some(line) = self
            .timed(self.timeouts.read, "stream read", lines.next_line())
            .await?
        {
            if line.len() > MAX_SSE_LINE_BYTES {
                anyhow::bail!(
                    "{} streaming line exceeded {} bytes",
                    self.backend_name,
                    MAX_SSE_LINE_BYTES
                );
            }
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
                        while let Some(line) = self
                            .timed(self.timeouts.read, "stream error read", lines.next_line())
                            .await?
                        {
                            if line.len() > MAX_SSE_LINE_BYTES {
                                anyhow::bail!(
                                    "{} streaming error line exceeded {} bytes",
                                    self.backend_name,
                                    MAX_SSE_LINE_BYTES
                                );
                            }
                            if error_body.len().saturating_add(line.len())
                                > DEFAULT_MAX_RESPONSE_BYTES
                            {
                                anyhow::bail!(
                                    "{} streaming error body exceeded {} bytes",
                                    self.backend_name,
                                    DEFAULT_MAX_RESPONSE_BYTES
                                );
                            }
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
                        if full_response.len().saturating_add(content.len())
                            > DEFAULT_MAX_RESPONSE_BYTES
                        {
                            anyhow::bail!(
                                "{} streaming response exceeded {} bytes",
                                self.backend_name,
                                DEFAULT_MAX_RESPONSE_BYTES
                            );
                        }
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
        let stream = self.connect(&addr).await?;

        let (reader, mut writer) = stream.into_split();

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
            path,
            addr,
            body.len(),
            self.authorization_header(),
            body
        );

        self.timed(
            self.timeouts.read,
            "request write",
            writer.write_all(request.as_bytes()),
        )
        .await?;

        self.timed_result(
            self.timeouts.request,
            "response read",
            read_bounded_http_response(reader, DEFAULT_MAX_RESPONSE_BYTES),
        )
        .await
    }

    async fn http_get(&self, path: &str) -> Result<HttpResponse> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = self.connect(&addr).await?;

        let (reader, mut writer) = stream.into_split();

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\n{}Connection: close\r\n\r\n",
            path,
            addr,
            self.authorization_header()
        );
        self.timed(
            self.timeouts.read,
            "health request write",
            writer.write_all(request.as_bytes()),
        )
        .await?;

        self.timed_result(
            self.timeouts.read,
            "health response read",
            read_bounded_http_response(reader, DEFAULT_MAX_RESPONSE_BYTES),
        )
        .await
    }
}

async fn read_bounded_http_response(
    reader: tokio::net::tcp::OwnedReadHalf,
    max_response_bytes: usize,
) -> Result<HttpResponse> {
    let mut buf_reader = BufReader::new(reader);
    let limits = HttpResponseLimits::with_body_cap(max_response_bytes);
    read_response(&mut buf_reader, &limits)
        .await
        .map_err(map_llm_http_read_error)
}

fn map_llm_http_read_error(err: HttpReadError) -> anyhow::Error {
    match err {
        HttpReadError::BodyTooLarge => anyhow::anyhow!("LLM response exceeded body cap"),
        other => anyhow::anyhow!("{other}"),
    }
}

fn build_nvext(messages: &[Message], hints: Option<&LlmRequestHints>) -> Option<NvExt> {
    let session_id = hints
        .and_then(|h| h.session_id.as_deref())
        .and_then(normalize_runtime_session_id);
    let agent_hints = hints.and_then(|hints| {
        if session_id.is_some()
            || hints.priority.is_some()
            || hints.output_sequence_length.is_some()
            || hints.speculative_prefill
        {
            Some(AgentHints {
                session_id: session_id.clone(),
                priority: hints.priority,
                osl: hints.output_sequence_length,
                speculative_prefill: hints.speculative_prefill.then_some(true),
            })
        } else {
            None
        }
    });

    let cache_control = hints
        .and_then(|h| h.cache_ttl_secs)
        .map(|ttl| cache_control(ttl, system_prompt_prefix_cache(messages)));

    if agent_hints.is_some() || cache_control.is_some() {
        Some(NvExt {
            agent_hints,
            cache_control,
        })
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemPromptPrefixCache {
    cache_key: String,
    prefix_bytes: usize,
}

fn cache_control(ttl_secs: u32, prefix_cache: Option<SystemPromptPrefixCache>) -> CacheControl {
    CacheControl {
        cache_type: "ephemeral",
        ttl: format_ttl(ttl_secs),
        scope: prefix_cache
            .as_ref()
            .map(|_| SYSTEM_PROMPT_PREFIX_CACHE_SCOPE),
        cache_key: prefix_cache.as_ref().map(|cache| cache.cache_key.clone()),
        prefix_bytes: prefix_cache.map(|cache| cache.prefix_bytes),
    }
}

fn system_prompt_prefix_cache(messages: &[Message]) -> Option<SystemPromptPrefixCache> {
    let system = messages
        .iter()
        .find(|message| message.role == "system")?
        .content
        .as_str();

    let prefix_end = SYSTEM_PROMPT_PREFIX_CACHE_MARKERS
        .iter()
        .filter_map(|marker| system.find(marker).map(|pos| pos + marker.len()))
        .min()?;
    let prefix = system.get(..prefix_end)?;
    if prefix.trim().is_empty() {
        return None;
    }

    Some(SystemPromptPrefixCache {
        cache_key: format!(
            "{}{}",
            SYSTEM_PROMPT_PREFIX_CACHE_KEY_PREFIX,
            crate::prompt_sha::sha256_hex(prefix)
        ),
        prefix_bytes: prefix.len(),
    })
}

fn normalize_runtime_session_id(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len().min(64));
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
        if out.len() == 64 {
            break;
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() { None } else { Some(out) }
}

fn format_ttl(secs: u32) -> String {
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn parse_status_line(line: &str) -> u16 {
    line.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn compact_genie_runtime_messages(messages: &[Message], max_body_bytes: usize) -> Vec<Message> {
    let system_text = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.trim())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut compacted = if system_text.is_empty() {
        Vec::new()
    } else {
        vec![Message {
            role: "system".into(),
            content: compact_genie_runtime_system(&system_text),
        }]
    };
    let has_system_context = !compacted.is_empty();

    let Some(latest_idx) = messages
        .iter()
        .rposition(|m| m.role == "user")
        .or_else(|| messages.iter().rposition(|m| m.role != "system"))
    else {
        return compacted;
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

fn compact_genie_runtime_system(system_text: &str) -> String {
    let mut sections = vec![GENIE_RUNTIME_COMPACT_SYSTEM_PREFIX.to_string()];
    let tool_lines = compact_genie_runtime_tool_lines(system_text);
    if !tool_lines.is_empty() {
        sections.push(format!("Tools:\n{}", tool_lines.join("\n")));
    }

    sections.push(compact_genie_runtime_rules(system_text));

    if let Some(context) = compact_household_context(system_text) {
        sections.push(format!("Household context:\n{context}"));
    }

    sections.join("\n\n")
}

fn compact_genie_runtime_tool_lines(system_text: &str) -> Vec<String> {
    let specs = [
        (
            "home_control",
            "home_control {entity, action, value?} - control safe home devices/scenes",
        ),
        (
            "home_status",
            "home_status {entity} - query Home Assistant state",
        ),
        (
            "home_undo",
            "home_undo {} - undo last reversible home action",
        ),
        (
            "action_history",
            "action_history {} - recent actions/pending confirmations",
        ),
        ("set_timer", "set_timer {seconds, label?}"),
        ("get_time", "get_time {}"),
        ("get_weather", "get_weather {location, forecast?}"),
        ("web_search", "web_search {query, limit?, fresh?}"),
        ("system_info", "system_info {}"),
        ("calculate", "calculate {expression}"),
        ("play_media", "play_media {query}"),
        ("memory_recall", "memory_recall {query}"),
        ("memory_status", "memory_status {}"),
        ("memory_forget", "memory_forget {query}"),
        ("memory_store", "memory_store {content, category?}"),
        ("hello_world", "hello_world {name?} - demo greeting only"),
    ];

    specs
        .iter()
        .filter(|(name, _)| system_text.contains(name))
        .map(|(_, line)| format!("- {line}"))
        .collect()
}

fn compact_genie_runtime_rules(system_text: &str) -> String {
    let mut rules = vec![
        "If no tool is needed, answer naturally in 1-3 short sentences.",
        "Use calculate for math, get_weather for weather, get_time for time, and system_info for system/Home Assistant/memory diagnostics.",
        "Use memory_recall when the user asks what you remember, what you know about them, or asks for their name.",
        "Use memory_store only when explicitly asked; never store secrets, access codes, lock combinations, or sensitive locations.",
    ];

    if system_text.contains("web_search") {
        rules.push("Use web_search only for current/recent public facts or explicit web lookup; never send private secrets.");
    }
    if system_text.contains("home_control") {
        rules.push("Use home_control/home_status for smart-home requests; risky actions may require local confirmation.");
    } else if system_text.contains("Home control is currently unavailable") {
        rules.push("Home control is unavailable; say Home Assistant is not connected if asked to control a device.");
    }
    if system_text.contains("hello_world") {
        rules.push("Use hello_world only for explicit hello_world demo requests.");
    }

    format!("Rules:\n- {}", rules.join("\n- "))
}

fn compact_household_context(system_text: &str) -> Option<String> {
    let markers = ["Relevant household context:\n", "## Household Context\n"];
    let (marker_pos, marker_len) = markers
        .iter()
        .filter_map(|marker| system_text.rfind(marker).map(|pos| (pos, marker.len())))
        .max_by_key(|(pos, _)| *pos)?;

    let tail = &system_text[marker_pos + marker_len..];
    let context = tail
        .split("\n## ")
        .next()
        .unwrap_or(tail)
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && *line != "(no household context yet)"
                && *line != "Relevant household context:"
        })
        .collect::<Vec<_>>()
        .join("\n");

    if context.is_empty() {
        None
    } else {
        Some(truncate_utf8(&context, GENIE_RUNTIME_CONTEXT_MAX_BYTES).to_string())
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }

    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end()
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
        // Slice on a UTF-8 char boundary; a fixed byte offset can land mid-character
        // and panic, which aborts the whole daemon under `panic = "abort"`.
        format!("{}...", truncate_utf8(trimmed, MAX_LEN))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

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
            conversation_id: None,
            think: None,
            response_format: None,
            nvext: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("nemotron-4b"));
        assert!(json.contains("turn on the lights"));
    }

    #[test]
    fn generic_request_profile_omits_runtime_hints() {
        let profile = RequestProfile::generic();
        let hints = LlmRequestHints::agent_turn("conv-abc", 256);
        let prepared = profile
            .prepare_body(
                &[Message {
                    role: "user".into(),
                    content: "hello".into(),
                }],
                Some(64),
                false,
                None,
                Some(&hints),
            )
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();

        assert!(json.get("conversation_id").is_none());
        assert!(json.get("nvext").is_none());
    }

    #[test]
    fn genie_runtime_profile_serializes_session_and_cache_hints() {
        let profile = RequestProfile::genie_ai_runtime();
        let hints = LlmRequestHints::agent_turn("conv-abc", 512);
        let prepared = profile
            .prepare_body(
                &[Message {
                    role: "user".into(),
                    content: "turn on the lights".into(),
                }],
                Some(512),
                false,
                None,
                Some(&hints),
            )
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();

        assert_eq!(json["conversation_id"], "conv-abc");
        assert_eq!(json["nvext"]["agent_hints"]["session_id"], "conv-abc");
        assert_eq!(json["nvext"]["agent_hints"]["priority"], 50);
        assert_eq!(json["nvext"]["agent_hints"]["osl"], 512);
        assert_eq!(json["nvext"]["cache_control"]["type"], "ephemeral");
        assert_eq!(json["nvext"]["cache_control"]["ttl"], "15m");
        assert!(json["nvext"]["cache_control"].get("scope").is_none());
        assert!(json["nvext"]["cache_control"].get("cache_key").is_none());
        assert!(json["nvext"]["cache_control"].get("prefix_bytes").is_none());
    }

    #[test]
    fn genie_runtime_profile_serializes_system_prompt_prefix_cache_hint() {
        let profile = RequestProfile::genie_ai_runtime();
        let hints = LlmRequestHints::agent_turn("conv-abc", 512);
        let static_prompt = "You are GenieClaw, a local home AI native to NVIDIA Jetson Orin 8GB.\nUse tools safely.";
        let marker = "\n\nRelevant household context:\n";
        let full_prompt = format!("{static_prompt}{marker}Jared lives here.");
        let prepared = profile
            .prepare_body(
                &[
                    Message {
                        role: "system".into(),
                        content: full_prompt.clone(),
                    },
                    Message {
                        role: "user".into(),
                        content: "hello".into(),
                    },
                ],
                Some(512),
                false,
                None,
                Some(&hints),
            )
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        let cache = &json["nvext"]["cache_control"];
        let expected_prefix = format!("{static_prompt}{marker}");

        assert_eq!(cache["type"], "ephemeral");
        assert_eq!(cache["ttl"], "15m");
        assert_eq!(cache["scope"], SYSTEM_PROMPT_PREFIX_CACHE_SCOPE);
        assert_eq!(cache["prefix_bytes"], expected_prefix.len());
        assert_eq!(
            cache["cache_key"],
            format!(
                "{}{}",
                SYSTEM_PROMPT_PREFIX_CACHE_KEY_PREFIX,
                crate::prompt_sha::sha256_hex(&expected_prefix)
            )
        );
    }

    #[test]
    fn system_prompt_prefix_cache_key_ignores_dynamic_household_context() {
        let marker = "\n\nRelevant household context:\n";
        let first = system_prompt_prefix_cache(&[Message {
            role: "system".into(),
            content: format!("stable prompt{marker}Jared likes tea."),
        }])
        .unwrap();
        let second = system_prompt_prefix_cache(&[Message {
            role: "system".into(),
            content: format!("stable prompt{marker}Maya likes oat milk."),
        }])
        .unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn compacted_system_prompt_prefix_cache_uses_household_context_boundary() {
        let marker = "\n\nHousehold context:\n";
        let cache = system_prompt_prefix_cache(&[Message {
            role: "system".into(),
            content: format!("stable compacted prompt{marker}Jared lives here."),
        }])
        .unwrap();

        assert_eq!(
            cache.prefix_bytes,
            format!("stable compacted prompt{marker}").len()
        );
    }

    #[test]
    fn runtime_session_ids_are_sanitized_for_cache_files() {
        assert_eq!(
            normalize_runtime_session_id("voice/session 1").as_deref(),
            Some("voice_session_1")
        );
        assert_eq!(normalize_runtime_session_id("///"), None);
        assert_eq!(
            normalize_runtime_session_id(&"x".repeat(80)).unwrap().len(),
            64
        );
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
            .prepare_body(&messages, Some(64), false, None, None)
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
                content: format!(
                    "{}\n\nRelevant household context:\nJared lives here.\n",
                    "tool manifest memory_recall household context ".repeat(128)
                ),
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
            .prepare_body(&messages, Some(64), false, None, None)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        let serialized_messages = json["messages"].to_string();

        assert!(prepared.compacted);
        assert_eq!(json["model"], "jetson-llm");
        assert_eq!(json["think"], false);
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);
        assert_eq!(json["messages"][0]["role"], "system");
        assert!(serialized_messages.contains("memory_recall"));
        assert!(serialized_messages.contains("Jared lives here"));
        assert!(serialized_messages.contains("Say hello from the GeniePod web UI."));
        assert!(serialized_messages.contains("NVIDIA Jetson Orin 8GB"));
        assert!(!serialized_messages.contains("older assistant turn"));
        assert!(prepared.body.len() < GENIE_RUNTIME_MAX_BODY_BYTES);
    }

    #[test]
    fn genie_runtime_profile_compacts_runtime_prompt_under_4k_budget() {
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
            .prepare_body(&messages, Some(64), false, None, None)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        let serialized_messages = json["messages"].to_string();

        assert!(prepared.compacted);
        assert!(prepared.body.len() < GENIE_RUNTIME_MAX_BODY_BYTES);
        assert!(serialized_messages.contains("memory_recall"));
        assert!(serialized_messages.contains("What is my name?"));
        assert!(serialized_messages.contains("NVIDIA Jetson Orin 8GB"));
        assert!(!serialized_messages.contains("tool manifest tool manifest"));
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

    #[test]
    fn truncate_body_does_not_panic_on_multibyte_boundary() {
        // A multi-byte char ('é' is 2 bytes) straddling the 240-byte cutoff used to
        // panic via `&trimmed[..240]`. The result must be valid UTF-8 ending in "...".
        let body = format!("{}é{}", "a".repeat(239), "b".repeat(50));
        let out = truncate_body(&body);
        assert!(out.ends_with("..."));
        assert!(out.len() <= 240 + 3);
        // The truncated prefix never includes a partial 'é'.
        assert!(!out.trim_end_matches("...").ends_with('\u{fffd}'));
    }

    #[test]
    fn truncate_body_returns_short_bodies_untouched() {
        assert_eq!(truncate_body("  short error  "), "short error");
    }

    #[test]
    fn backend_error_message_truncates_non_json_unicode_body() {
        // Localized HTML/plain error pages reach the truncate_body fallback.
        let body = format!("<html>错误页面 {}</html>", "字".repeat(200));
        let msg = backend_error_message(&body);
        assert!(msg.ends_with("..."));
    }

    #[test]
    fn backend_error_message_extracts_nested_and_top_level_message() {
        assert_eq!(
            backend_error_message(r#"{"error":{"message":"rate limited"}}"#),
            "rate limited"
        );
        assert_eq!(
            backend_error_message(r#"{"message":"bad request"}"#),
            "bad request"
        );
    }

    async fn spawn_test_listener<F, Fut>(handler: F) -> std::net::SocketAddr
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((conn, _)) = listener.accept().await {
                handler(conn).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn rejects_oversize_content_length() {
        let addr = spawn_test_listener(move |mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n";
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.shutdown().await;
        })
        .await;

        let stream = TcpStream::connect(addr).await.unwrap();
        let (reader, _) = stream.into_split();
        let err = read_bounded_http_response(reader, 16 * 1024)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exceeded body cap"),
            "expected cap error, got: {err}"
        );
    }

    #[tokio::test]
    async fn read_to_eof_response_is_size_capped() {
        let addr = spawn_test_listener(|mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf).await;
            let header =
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
            let _ = conn.write_all(header.as_bytes()).await;
            let filler = vec![b'x'; 4096];
            for _ in 0..16 {
                if conn.write_all(&filler).await.is_err() {
                    break;
                }
            }
            let _ = conn.shutdown().await;
        })
        .await;

        let stream = TcpStream::connect(addr).await.unwrap();
        let (reader, _) = stream.into_split();
        let err = read_bounded_http_response(reader, 16 * 1024)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exceeded body cap"),
            "expected EOF cap error, got: {err}"
        );
    }

    #[tokio::test]
    async fn accepts_valid_content_length_response() {
        let addr = spawn_test_listener(|mut conn| async move {
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf).await;
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.shutdown().await;
        })
        .await;

        let stream = TcpStream::connect(addr).await.unwrap();
        let (reader, _) = stream.into_split();
        let response = read_bounded_http_response(reader, 16 * 1024).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn bearer_auth_header_is_sent_on_chat_requests() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let addr = spawn_test_listener(|mut conn| async move {
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = tx.send(request);
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = conn.write_all(response.as_bytes()).await;
            let _ = conn.shutdown().await;
        })
        .await;

        let client = OpenAiCompatClient::from_url_with_profile_and_timeouts(
            "test-backend",
            &format!("http://{addr}"),
            RequestProfile::generic(),
            LlmTimeouts {
                connect: Duration::from_secs(2),
                read: Duration::from_secs(2),
                request: Duration::from_secs(2),
            },
        )
        .with_bearer_token("oauth-access-token");

        let response = client
            .chat(
                &[Message {
                    role: "user".into(),
                    content: "hi".into(),
                }],
                Some(8),
            )
            .await
            .unwrap();
        let request = rx.await.unwrap();

        assert_eq!(response, "ok");
        assert!(request.contains("Authorization: Bearer oauth-access-token\r\n"));
    }

    #[test]
    fn default_timeouts_are_bounded() {
        let t = LlmTimeouts::default();
        assert!(t.connect.as_secs() > 0);
        assert!(t.read.as_secs() > 0);
        assert!(t.request.as_secs() > 0);
    }

    #[test]
    fn from_secs_clamps_zero_to_keep_a_bound() {
        // A misconfigured 0 must never disable the bound (issue #181 regression).
        let t = LlmTimeouts::from_secs(0, 0, 0);
        assert_eq!(t.connect, Duration::from_secs(1));
        assert_eq!(t.read, Duration::from_secs(1));
        assert_eq!(t.request, Duration::from_secs(1));
    }

    /// A backend that accepts the connection, sends 200 headers promising a
    /// body, then never sends it must make a *non-streaming* completion fail
    /// fast on the body read timeout instead of hanging forever (issue #181).
    #[tokio::test]
    async fn non_streaming_body_read_times_out_on_stalled_backend() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                    .await;
                // Promise 100 body bytes that never arrive.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });

        let client = OpenAiCompatClient::from_url_with_profile_and_timeouts(
            "test-backend",
            &format!("http://{addr}"),
            RequestProfile::generic(),
            LlmTimeouts {
                connect: Duration::from_secs(2),
                read: Duration::from_millis(200),
                request: Duration::from_millis(200),
            },
        );

        let start = std::time::Instant::now();
        let result = client
            .chat(
                &[Message {
                    role: "user".into(),
                    content: "hi".into(),
                }],
                Some(8),
            )
            .await;
        let elapsed = start.elapsed();
        server.abort();

        let err = result.expect_err("stalled body read must error, not hang");
        assert!(
            err.to_string().contains("timed out"),
            "error should mention timeout: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must fail fast on read timeout, not hang: took {elapsed:?}"
        );
    }

    /// A backend that completes the SSE headers then stalls mid-stream must make
    /// a *streaming* completion fail on the idle read timeout (issue #181).
    #[tokio::test]
    async fn streaming_read_times_out_on_stalled_backend() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                    .await;
                // Headers done, but no `data:` lines ever follow.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });

        let client = OpenAiCompatClient::from_url_with_profile_and_timeouts(
            "test-backend",
            &format!("http://{addr}"),
            RequestProfile::generic(),
            LlmTimeouts {
                connect: Duration::from_secs(2),
                read: Duration::from_millis(200),
                request: Duration::from_secs(2),
            },
        );

        let start = std::time::Instant::now();
        let mut tokens = String::new();
        let result = client
            .chat_stream(
                &[Message {
                    role: "user".into(),
                    content: "hi".into(),
                }],
                Some(8),
                &mut |t| tokens.push_str(t),
            )
            .await;
        let elapsed = start.elapsed();
        server.abort();

        let err = result.expect_err("stalled stream read must error, not hang");
        assert!(
            err.to_string().contains("timed out"),
            "error should mention timeout: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must fail fast on idle read timeout, not hang: took {elapsed:?}"
        );
        assert!(tokens.is_empty(), "no tokens should have been produced");
    }
}
