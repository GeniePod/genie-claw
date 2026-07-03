//! Channel abstraction: decouples the agent loop from transport.
//!
//! Defines the `IncomingTurn` / `OutgoingResponse` boundary and the `Channel`
//! trait a transport (voice, HTTP, Telegram, ...) implements to plug into a
//! shared turn-processing entry point, plus a small `ChannelRegistry` for
//! tracking active channels.
//!
//! This module only defines the types, the trait, and the registry (#563).
//! Porting the existing voice loop, HTTP routes, and Telegram handler onto
//! `Channel` — and actually funneling them through one agent entry point —
//! is tracked separately (#564); no existing call sites are touched here.

use anyhow::Result;
use async_trait::async_trait;

use crate::memory::policy::IdentityConfidence;

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
    use std::collections::VecDeque;

    struct MockChannel {
        kind: ChannelKind,
        inbox: VecDeque<IncomingTurn>,
        pub sent: Vec<OutgoingResponse>,
    }

    impl MockChannel {
        fn new(kind: ChannelKind, inbox: Vec<IncomingTurn>) -> Self {
            Self {
                kind,
                inbox: inbox.into(),
                sent: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn kind(&self) -> ChannelKind {
            self.kind
        }

        async fn recv(&mut self) -> Option<IncomingTurn> {
            self.inbox.pop_front()
        }

        async fn send(&mut self, response: OutgoingResponse) -> Result<()> {
            self.sent.push(response);
            Ok(())
        }
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
    async fn mock_channel_recv_send_round_trip() {
        let turn = IncomingTurn::new("what's the weather?", "sess-1", ChannelKind::Telegram);
        let mut channel = MockChannel::new(ChannelKind::Telegram, vec![turn]);

        let received = channel.recv().await.expect("turn should be queued");
        assert_eq!(received.text, "what's the weather?");
        assert!(channel.recv().await.is_none(), "inbox should drain to None");

        channel
            .send(OutgoingResponse::new("sunny", &received.session_id))
            .await
            .unwrap();
        assert_eq!(channel.sent.len(), 1);
        assert_eq!(channel.sent[0].text, "sunny");
    }

    #[test]
    fn registry_tracks_registered_channels_by_kind() {
        let mut registry = ChannelRegistry::new();
        assert!(registry.is_empty());

        registry.register(Box::new(MockChannel::new(ChannelKind::Voice, vec![])));
        registry.register(Box::new(MockChannel::new(ChannelKind::Telegram, vec![])));
        registry.register(Box::new(MockChannel::new(ChannelKind::Telegram, vec![])));

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
