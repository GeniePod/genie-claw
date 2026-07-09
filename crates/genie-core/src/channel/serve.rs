//! Driving a [`Channel`] end to end (#564).
//!
//! [`ChannelRegistry`](super::ChannelRegistry) tracks channels but deliberately
//! does not own their recv loop, and [`ScriptedChannel`](super::ScriptedChannel)
//! exists so that wiring can be exercised without sockets — yet nothing tied the
//! two together. `serve_channel` is that missing primitive: it owns the
//! `recv → handle → send` loop for a single [`Channel`], so a transport only has
//! to implement the trait and hand the agent a per-turn handler.
//!
//! Error handling splits along the transport / application seam. A handler
//! (application) error is per-turn: it is logged and the turn is skipped, so one
//! bad turn does not tear the channel down. A `send` (transport) error is fatal:
//! a dead socket or closed pipe will not recover, so the loop stops and returns
//! the error to whatever owns the channel.

use anyhow::{Context, Result};
use std::future::Future;

use super::{Channel, IncomingTurn, OutgoingResponse};

/// Drive `channel` until it closes, routing each inbound turn through `handler`.
///
/// Loops [`Channel::recv`], passes each [`IncomingTurn`] to `handler`, and
/// delivers the resulting [`OutgoingResponse`] with [`Channel::send`]. Returns
/// the number of responses successfully delivered once `recv` reports the
/// channel closed (`None`).
///
/// A handler error is logged and its turn skipped; a `send` error is treated as
/// a dead transport and ends the loop with that error.
///
/// ```
/// use genie_core::channel::{
///     serve_channel, ChannelKind, IncomingTurn, OutgoingResponse, ScriptedChannel,
/// };
///
/// # async fn demo() {
/// let mut channel = ScriptedChannel::new(
///     ChannelKind::Http,
///     [IncomingTurn::new("ping", "sess-1", ChannelKind::Http)],
/// );
/// let delivered = serve_channel(&mut channel, |turn| async move {
///     Ok(OutgoingResponse::new(format!("echo: {}", turn.text), turn.session_id))
/// })
/// .await
/// .unwrap();
/// assert_eq!(delivered, 1);
/// # }
/// ```
pub async fn serve_channel<C, H, Fut>(channel: &mut C, mut handler: H) -> Result<usize>
where
    C: Channel + ?Sized,
    H: FnMut(IncomingTurn) -> Fut,
    Fut: Future<Output = Result<OutgoingResponse>>,
{
    let mut delivered = 0usize;
    while let Some(turn) = channel.recv().await {
        // Keep the routing fields before the turn is moved into the handler, so
        // both the skip log and the fatal-send context can name the turn.
        let kind = turn.channel;
        let session = turn.session_id.clone();
        match handler(turn).await {
            Ok(response) => {
                channel.send(response).await.with_context(|| {
                    format!(
                        "channel {} failed to deliver a response for session {session}",
                        kind.as_str()
                    )
                })?;
                delivered += 1;
            }
            Err(error) => {
                tracing::warn!(
                    channel = kind.as_str(),
                    session = %session,
                    error = %error,
                    "channel handler failed for turn; skipping"
                );
            }
        }
    }
    Ok(delivered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{ChannelKind, ScriptedChannel};

    fn turn(text: &str) -> IncomingTurn {
        IncomingTurn::new(text, "sess-1", ChannelKind::Http)
    }

    async fn echo(turn: IncomingTurn) -> Result<OutgoingResponse> {
        Ok(OutgoingResponse::new(
            format!("echo: {}", turn.text),
            turn.session_id,
        ))
    }

    #[tokio::test]
    async fn drives_every_queued_turn_and_records_responses() {
        let mut channel =
            ScriptedChannel::new(ChannelKind::Http, [turn("one"), turn("two"), turn("three")]);

        let delivered = serve_channel(&mut channel, echo).await.unwrap();

        assert_eq!(delivered, 3);
        let sent: Vec<_> = channel.sent_responses().iter().map(|r| &r.text).collect();
        assert_eq!(sent, ["echo: one", "echo: two", "echo: three"]);
    }

    #[tokio::test]
    async fn closed_channel_delivers_nothing() {
        let mut channel = ScriptedChannel::new(ChannelKind::Http, []);

        let delivered = serve_channel(&mut channel, echo).await.unwrap();

        assert_eq!(delivered, 0);
        assert!(channel.sent_responses().is_empty());
    }

    #[tokio::test]
    async fn handler_error_skips_the_turn_without_stopping_the_channel() {
        let mut channel = ScriptedChannel::new(
            ChannelKind::Http,
            [turn("good"), turn("boom"), turn("good")],
        );

        let delivered = serve_channel(&mut channel, |turn| async move {
            if turn.text == "boom" {
                anyhow::bail!("handler blew up");
            }
            Ok(OutgoingResponse::new("ok", turn.session_id))
        })
        .await
        .unwrap();

        // The failing turn is skipped; the two good turns are still delivered.
        assert_eq!(delivered, 2);
        assert_eq!(channel.sent_responses().len(), 2);
    }

    /// A `Channel` whose `send` always fails, to exercise the fatal-transport path.
    struct FailingSendChannel {
        inbox: std::collections::VecDeque<IncomingTurn>,
        send_attempts: usize,
    }

    #[async_trait::async_trait]
    impl Channel for FailingSendChannel {
        fn kind(&self) -> ChannelKind {
            ChannelKind::Http
        }

        async fn recv(&mut self) -> Option<IncomingTurn> {
            self.inbox.pop_front()
        }

        async fn send(&mut self, _response: OutgoingResponse) -> Result<()> {
            self.send_attempts += 1;
            anyhow::bail!("transport is dead")
        }
    }

    #[tokio::test]
    async fn send_error_is_fatal_and_stops_the_loop() {
        let mut channel = FailingSendChannel {
            inbox: [turn("first"), turn("second")].into_iter().collect(),
            send_attempts: 0,
        };

        let result = serve_channel(&mut channel, echo).await;

        assert!(
            result.is_err(),
            "a send failure must end the loop with an error"
        );
        // Only the first turn was attempted; the loop stopped before the second.
        assert_eq!(channel.send_attempts, 1);
    }
}
