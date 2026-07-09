//! In-process reference `Channel` for tests and harnesses (#564).
//!
//! Queues scripted `IncomingTurn`s and records outbound `OutgoingResponse`s so
//! channel wiring can be exercised without sockets or subprocess I/O.

use std::collections::VecDeque;

use anyhow::Result;
use async_trait::async_trait;

use super::{Channel, ChannelKind, IncomingTurn, OutgoingResponse};

/// A deterministic `Channel` backed by a scripted inbox.
pub struct ScriptedChannel {
    kind: ChannelKind,
    inbox: VecDeque<IncomingTurn>,
    sent: Vec<OutgoingResponse>,
}

impl ScriptedChannel {
    pub fn new(kind: ChannelKind, inbox: impl IntoIterator<Item = IncomingTurn>) -> Self {
        Self {
            kind,
            inbox: inbox.into_iter().collect(),
            sent: Vec::new(),
        }
    }

    pub fn sent_responses(&self) -> &[OutgoingResponse] {
        &self.sent
    }

    pub fn drain_sent(&mut self) -> Vec<OutgoingResponse> {
        std::mem::take(&mut self.sent)
    }
}

#[async_trait]
impl Channel for ScriptedChannel {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recv_send_round_trip_records_outbound() {
        let turn = IncomingTurn::new("ping", "sess-1", ChannelKind::Http);
        let mut channel = ScriptedChannel::new(ChannelKind::Http, [turn]);

        let received = channel.recv().await.expect("queued turn");
        assert_eq!(received.text, "ping");
        assert!(channel.recv().await.is_none());

        channel
            .send(OutgoingResponse::new("pong", &received.session_id))
            .await
            .unwrap();
        assert_eq!(channel.sent_responses().len(), 1);
        assert_eq!(channel.sent_responses()[0].text, "pong");
    }
}
