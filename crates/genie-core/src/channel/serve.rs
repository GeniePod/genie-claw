//! Generic `recv -> handle -> send` driver for any [`Channel`] (#564, #700).
//!
//! Each transport only has to implement `Channel::recv`/`Channel::send`; the
//! per-turn agent logic is injected as a handler closure so `serve_channel`
//! stays decoupled from any specific agent-loop implementation and can be
//! shared across voice, HTTP, and Telegram as they port onto `Channel`.

use std::future::Future;

use anyhow::Result;

use super::{Channel, IncomingTurn, OutgoingResponse};

/// Drive `channel` until it closes (`recv` returns `None`) or delivering a
/// response fails (a transport error).
///
/// Per-turn handler errors are logged and the loop continues: a single bad
/// turn must not take down an otherwise-healthy channel. A `send` failure
/// ends the loop, since the transport itself is no longer usable.
pub async fn serve_channel<H, Fut>(channel: &mut dyn Channel, mut handle: H) -> Result<()>
where
    H: FnMut(IncomingTurn) -> Fut,
    Fut: Future<Output = Result<OutgoingResponse>>,
{
    let kind = channel.kind();
    while let Some(turn) = channel.recv().await {
        let session_id = turn.session_id.clone();
        let response = match handle(turn).await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    channel = kind.as_str(),
                    session_id,
                    error = %e,
                    "turn handler failed; skipping turn"
                );
                continue;
            }
        };
        if let Err(e) = channel.send(response).await {
            tracing::warn!(
                channel = kind.as_str(),
                error = %e,
                "channel send failed; ending loop"
            );
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use async_trait::async_trait;

    use super::*;
    use crate::channel::{ChannelKind, ScriptedChannel};

    #[tokio::test]
    async fn drives_every_queued_turn_through_handle_and_send() {
        let turns = vec![
            IncomingTurn::new("one", "s1", ChannelKind::Http),
            IncomingTurn::new("two", "s1", ChannelKind::Http),
        ];
        let mut channel = ScriptedChannel::new(ChannelKind::Http, turns);

        serve_channel(&mut channel, |turn| async move {
            Ok(OutgoingResponse::new(
                format!("echo:{}", turn.text),
                turn.session_id,
            ))
        })
        .await
        .unwrap();

        let sent = channel.sent_responses();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].text, "echo:one");
        assert_eq!(sent[1].text, "echo:two");
    }

    #[tokio::test]
    async fn handler_error_is_logged_and_skipped_not_fatal() {
        let turns = vec![
            IncomingTurn::new("bad", "s1", ChannelKind::Http),
            IncomingTurn::new("good", "s1", ChannelKind::Http),
        ];
        let mut channel = ScriptedChannel::new(ChannelKind::Http, turns);

        let result = serve_channel(&mut channel, |turn| async move {
            if turn.text == "bad" {
                anyhow::bail!("boom");
            }
            Ok(OutgoingResponse::new("ok", turn.session_id))
        })
        .await;

        assert!(result.is_ok(), "a handler error must not end the loop");
        assert_eq!(channel.sent_responses().len(), 1);
        assert_eq!(channel.sent_responses()[0].text, "ok");
    }

    /// Minimal `Channel` whose `send` always fails, so the test can assert
    /// that a transport error ends the loop instead of being swallowed.
    struct FailingSendChannel {
        inbox: VecDeque<IncomingTurn>,
    }

    #[async_trait]
    impl Channel for FailingSendChannel {
        fn kind(&self) -> ChannelKind {
            ChannelKind::Http
        }

        async fn recv(&mut self) -> Option<IncomingTurn> {
            self.inbox.pop_front()
        }

        async fn send(&mut self, _response: OutgoingResponse) -> Result<()> {
            anyhow::bail!("transport gone")
        }
    }

    #[tokio::test]
    async fn send_failure_ends_the_loop() {
        let mut channel = FailingSendChannel {
            inbox: VecDeque::from(vec![
                IncomingTurn::new("one", "s1", ChannelKind::Http),
                IncomingTurn::new("two", "s1", ChannelKind::Http),
            ]),
        };
        let mut handled = 0;

        let result = serve_channel(&mut channel, |_turn| {
            handled += 1;
            async move { Ok(OutgoingResponse::new("ok", "s1")) }
        })
        .await;

        assert!(result.is_err(), "a send failure must end the loop");
        assert_eq!(handled, 1, "the second queued turn must never be handled");
    }

    #[tokio::test]
    async fn empty_channel_returns_ok_immediately() {
        let mut channel = ScriptedChannel::new(ChannelKind::Http, []);
        let mut handled = 0;

        serve_channel(&mut channel, |_turn| {
            handled += 1;
            async move { Ok(OutgoingResponse::new("ok", "s1")) }
        })
        .await
        .unwrap();

        assert_eq!(handled, 0);
    }
}
