//! Channel abstraction: decouples the agent loop from transport.
//!
//! Defines the `IncomingTurn` / `OutgoingResponse` boundary and the `Channel`
//! trait a transport (voice, HTTP, Telegram, ...) implements to plug into a
//! shared turn-processing entry point, plus a small `ChannelRegistry` for
//! tracking active channels.
//!
//! Reference adapters for #564 live in [`scripted`] and [`http`]; porting the
//! existing voice loop, HTTP routes, and Telegram handler onto `Channel` is
//! tracked in #564.

pub mod http;
pub mod scripted;

pub use http::incoming_turn_from_chat_json;
pub use scripted::ScriptedChannel;

use anyhow::Result;
use async_trait::async_trait;

use crate::memory::policy::{IdentityConfidence, MemoryReadContext};

/// Which transport a turn arrived on / a response should be delivered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Voice,
    Http,
    Telegram,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Voice => "voice",
            ChannelKind::Http => "http",
            ChannelKind::Telegram => "telegram",
        }
    }
}

/// Resolved speaker identity for a turn, transport-agnostic.
///
/// Mirrors `voice::identity::SpeakerIdentity`'s shape without depending on
/// the `voice` feature, so `IncomingTurn` stays usable in chat-only
/// (`--no-default-features`) builds that have no biometric pipeline at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerInfo {
    pub name: Option<String>,
    pub confidence: IdentityConfidence,
}

impl Default for SpeakerInfo {
    fn default() -> Self {
        Self {
            name: None,
            confidence: IdentityConfidence::Unknown,
        }
    }
}

impl SpeakerInfo {
    /// True when a transport supplied a resolved speaker (name or confidence).
    pub fn is_resolved(&self) -> bool {
        self.name.is_some() || self.confidence != IdentityConfidence::Unknown
    }

    /// Memory policy inputs for this speaker.
    pub fn memory_read_context(&self, text: &str, shared_space_voice: bool) -> MemoryReadContext {
        crate::memory::policy::memory_read_context_from_text(
            text,
            self.confidence,
            shared_space_voice,
        )
    }
}

/// Stable session key for per-(channel, speaker) continuity (#565).
///
/// When no resolved speaker name is present, returns `fallback_session_id` unchanged.
pub fn session_key(
    channel: ChannelKind,
    speaker: &SpeakerInfo,
    fallback_session_id: &str,
) -> String {
    match speaker
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(name) => format!("{}:{}", channel.as_str(), name.to_ascii_lowercase()),
        None => fallback_session_id.to_string(),
    }
}

/// A single inbound turn, normalized across transports.
#[derive(Debug, Clone)]
pub struct IncomingTurn {
    pub text: String,
    pub speaker: SpeakerInfo,
    pub session_id: String,
    pub channel: ChannelKind,
}

impl IncomingTurn {
    /// Build a turn with no resolved speaker (the common case for HTTP/Telegram).
    pub fn new(
        text: impl Into<String>,
        session_id: impl Into<String>,
        channel: ChannelKind,
    ) -> Self {
        Self {
            text: text.into(),
            speaker: SpeakerInfo::default(),
            session_id: session_id.into(),
            channel,
        }
    }

    pub fn with_speaker(mut self, speaker: SpeakerInfo) -> Self {
        self.speaker = speaker;
        self
    }

    /// Resolved conversation id for per-(channel, speaker) continuity (#565).
    pub fn conversation_id(&self, fallback_session_id: &str) -> String {
        session_key(self.channel, &self.speaker, fallback_session_id)
    }

    /// Speaker name for conversation turn tagging (#560).
    pub fn speaker_name(&self) -> Option<&str> {
        self.speaker
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// Memory policy inputs for this turn's text and resolved speaker.
    pub fn memory_read_context(&self, shared_space_voice: bool) -> MemoryReadContext {
        self.speaker
            .memory_read_context(&self.text, shared_space_voice)
    }

    /// Tool execution context wired from session-layer speaker identity (#566).
    pub fn tool_execution_context(
        &self,
        request_origin: crate::tools::RequestOrigin,
        shared_space_voice: bool,
    ) -> crate::tools::ToolExecutionContext {
        crate::tools::ToolExecutionContext {
            request_origin,
            memory_read_context: self
                .speaker
                .is_resolved()
                .then(|| self.memory_read_context(shared_space_voice)),
            ..crate::tools::ToolExecutionContext::default()
        }
    }
}

/// The agent's reply to a single turn, ready for a `Channel` to deliver.
#[derive(Debug, Clone)]
pub struct OutgoingResponse {
    pub text: String,
    pub tool: Option<String>,
    pub session_id: String,
}

impl OutgoingResponse {
    pub fn new(text: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool: None,
            session_id: session_id.into(),
        }
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }
}

/// A transport that can receive turns and deliver responses.
///
/// Implementations own their transport-specific I/O (sockets, subprocess
/// pipes, long-polling, ...); the agent loop only ever sees
/// `IncomingTurn`/`OutgoingResponse`. `recv`/`send` take `&mut self` because
/// most transports (a wakeword listener, a Telegram long-poll loop, an HTTP
/// connection) are inherently stateful/sequential per channel instance.
#[async_trait]
pub trait Channel: Send + Sync {
    fn kind(&self) -> ChannelKind;

    /// Wait for and return the next inbound turn, or `None` once the channel has closed.
    async fn recv(&mut self) -> Option<IncomingTurn>;

    /// Deliver a response back through this channel.
    async fn send(&mut self, response: OutgoingResponse) -> Result<()>;
}

/// Tracks the channels active in a process so responses can be routed back
/// to the transport they came from.
///
/// Deliberately minimal: registration and lookup only. Owning/driving each
/// channel's recv loop and routing turns to/from the agent is the job of
/// whatever wires a real transport onto `Channel` (#564), not this registry.
#[derive(Default)]
pub struct ChannelRegistry {
    channels: Vec<Box<dyn Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, channel: Box<dyn Channel>) {
        self.channels.push(channel);
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn kinds(&self) -> Vec<ChannelKind> {
        self.channels.iter().map(|c| c.kind()).collect()
    }

    /// All registered channels of a given kind.
    pub fn by_kind(&self, kind: ChannelKind) -> Vec<&dyn Channel> {
        self.channels
            .iter()
            .filter(|c| c.kind() == kind)
            .map(|c| c.as_ref())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_key_uses_channel_and_speaker_when_name_present() {
        let speaker = SpeakerInfo {
            name: Some("Maya".into()),
            confidence: IdentityConfidence::High,
        };
        assert_eq!(
            session_key(ChannelKind::Http, &speaker, "default"),
            "http:maya"
        );
    }

    #[test]
    fn session_key_falls_back_without_resolved_name() {
        assert_eq!(
            session_key(ChannelKind::Voice, &SpeakerInfo::default(), "sess-1"),
            "sess-1"
        );
    }

    #[test]
    fn speaker_memory_read_context_detects_named_person_request() {
        let speaker = SpeakerInfo {
            name: Some("dana".into()),
            confidence: IdentityConfidence::High,
        };
        let ctx = speaker.memory_read_context("what does Maya like to drink", false);
        assert!(ctx.explicit_named_person);
        assert!(!ctx.shared_space_voice);
        assert_eq!(ctx.identity_confidence, IdentityConfidence::High);
    }

    #[test]
    fn incoming_turn_tool_context_is_channel_invariant() {
        use crate::tools::RequestOrigin;

        let speaker = SpeakerInfo {
            name: Some("dana".into()),
            confidence: IdentityConfidence::High,
        };
        let text = "what does Maya like to drink";
        let contexts: Vec<_> = [ChannelKind::Http, ChannelKind::Voice, ChannelKind::Telegram]
            .into_iter()
            .map(|channel| {
                IncomingTurn::new(text, "sess-fallback", channel)
                    .with_speaker(speaker.clone())
                    .tool_execution_context(RequestOrigin::Api, false)
            })
            .collect();
        assert!(
            contexts
                .windows(2)
                .all(|pair| pair[0].memory_read_context == pair[1].memory_read_context),
            "resolved speaker must produce identical tool context across channels"
        );
    }

    #[test]
    fn incoming_turn_quick_route_is_channel_invariant() {
        let text = "set a timer for 5 minutes";
        let routes: Vec<_> = [ChannelKind::Http, ChannelKind::Voice, ChannelKind::Telegram]
            .into_iter()
            .map(|channel| {
                crate::tools::quick::route_for_available_tools(
                    &IncomingTurn::new(text, "sess-1", channel).text,
                    false,
                    false,
                )
                .map(|call| call.name)
            })
            .collect();
        assert!(
            routes.windows(2).all(|pair| pair[0] == pair[1]),
            "quick router must not depend on transport channel"
        );
    }

    #[test]
    fn incoming_turn_new_defaults_to_unresolved_speaker() {
        let turn = IncomingTurn::new("hello", "sess-1", ChannelKind::Http);
        assert_eq!(turn.speaker, SpeakerInfo::default());
        assert_eq!(turn.speaker.name, None);
        assert_eq!(turn.speaker.confidence, IdentityConfidence::Unknown);
    }

    #[test]
    fn incoming_turn_with_speaker_overrides_default() {
        let speaker = SpeakerInfo {
            name: Some("dana".into()),
            confidence: IdentityConfidence::High,
        };
        let turn =
            IncomingTurn::new("hello", "sess-1", ChannelKind::Voice).with_speaker(speaker.clone());
        assert_eq!(turn.speaker, speaker);
    }

    #[test]
    fn outgoing_response_with_tool_sets_tool_name() {
        let response = OutgoingResponse::new("done", "sess-1").with_tool("set_timer");
        assert_eq!(response.tool.as_deref(), Some("set_timer"));
    }

    #[tokio::test]
    async fn scripted_channel_recv_send_round_trip() {
        let turn = IncomingTurn::new("what's the weather?", "sess-1", ChannelKind::Telegram);
        let mut channel = ScriptedChannel::new(ChannelKind::Telegram, [turn]);

        let received = channel.recv().await.expect("turn should be queued");
        assert_eq!(received.text, "what's the weather?");
        assert!(channel.recv().await.is_none(), "inbox should drain to None");

        channel
            .send(OutgoingResponse::new("sunny", &received.session_id))
            .await
            .unwrap();
        assert_eq!(channel.sent_responses().len(), 1);
        assert_eq!(channel.sent_responses()[0].text, "sunny");
    }

    #[test]
    fn registry_tracks_registered_channels_by_kind() {
        let mut registry = ChannelRegistry::new();
        assert!(registry.is_empty());

        registry.register(Box::new(ScriptedChannel::new(ChannelKind::Voice, [])));
        registry.register(Box::new(ScriptedChannel::new(ChannelKind::Telegram, [])));
        registry.register(Box::new(ScriptedChannel::new(ChannelKind::Telegram, [])));

        assert_eq!(registry.len(), 3);
        assert_eq!(registry.by_kind(ChannelKind::Telegram).len(), 2);
        assert_eq!(registry.by_kind(ChannelKind::Http).len(), 0);
        assert_eq!(
            registry.kinds(),
            vec![
                ChannelKind::Voice,
                ChannelKind::Telegram,
                ChannelKind::Telegram
            ]
        );
    }
}
