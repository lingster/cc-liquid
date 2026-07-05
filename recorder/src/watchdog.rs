//! Idle watchdog for [`EventSource`]s: turns a silent stall into an error.
//!
//! A TCP connection can go half-dead without a FIN/RST (peer host vanishes,
//! NAT state expires). The socket stays ESTABLISHED and a read pends forever,
//! so [`crate::reconnect::ReconnectSource`] never sees the failure — exactly
//! how a multi-week recording silently stopped. [`IdleTimeoutSource`] bounds
//! the wait: if the inner source yields nothing for `idle_timeout`, it returns
//! an error, which the reconnect layer treats like any stream error and
//! replaces the connection.
//!
//! The live client sends an application-level ping on a shorter period (see
//! [`crate::client`]), so a healthy connection always has traffic and the
//! watchdog only fires on genuinely dead links.

use std::time::Duration;

use async_trait::async_trait;

use crate::source::EventSource;

/// Default idle timeout: generous vs. the ~50s client ping cadence, so only a
/// dead connection (not a quiet market) can trip it.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Wraps an [`EventSource`], erroring if it stays silent for `idle_timeout`.
pub struct IdleTimeoutSource<S> {
    inner: S,
    idle_timeout: Duration,
}

impl<S: EventSource + Send> IdleTimeoutSource<S> {
    pub fn new(inner: S, idle_timeout: Duration) -> Self {
        Self {
            inner,
            idle_timeout,
        }
    }
}

#[async_trait]
impl<S: EventSource + Send> EventSource for IdleTimeoutSource<S> {
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        match tokio::time::timeout(self.idle_timeout, self.inner.next_message()).await {
            Ok(msg) => msg,
            Err(_) => Some(Err(anyhow::anyhow!(
                "idle timeout: no message in {:?} (connection presumed dead)",
                self.idle_timeout
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconnect::{ReconnectPolicy, ReconnectSource};
    use crate::source::ScriptedSource;
    use std::collections::VecDeque;

    /// Yields its queued messages, then pends forever — a half-dead connection.
    struct DrainThenHang {
        queue: VecDeque<String>,
    }

    impl DrainThenHang {
        fn new(msgs: &[&str]) -> Self {
            Self {
                queue: msgs.iter().map(|m| m.to_string()).collect(),
            }
        }
    }

    #[async_trait]
    impl EventSource for DrainThenHang {
        async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
            match self.queue.pop_front() {
                Some(m) => Some(Ok(m)),
                None => std::future::pending().await,
            }
        }
    }

    #[tokio::test]
    async fn passes_messages_and_end_through() {
        let mut src = IdleTimeoutSource::new(
            ScriptedSource::new(vec!["a".into(), "b".into()]),
            Duration::from_secs(90),
        );
        assert_eq!(src.next_message().await.unwrap().unwrap(), "a");
        assert_eq!(src.next_message().await.unwrap().unwrap(), "b");
        assert!(src.next_message().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_source_errors_after_the_idle_timeout() {
        let mut src =
            IdleTimeoutSource::new(DrainThenHang::new(&["a"]), Duration::from_secs(90));
        assert_eq!(src.next_message().await.unwrap().unwrap(), "a");
        // The hang: with the clock paused tokio auto-advances to the timeout.
        let err = src.next_message().await.unwrap().unwrap_err();
        assert!(err.to_string().contains("idle timeout"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_layer_replaces_a_stalled_connection() {
        // The June-14 failure mode, end to end: a connection goes silent, the
        // watchdog errors, and ReconnectSource swaps in a fresh connection.
        let mut replacements: VecDeque<anyhow::Result<IdleTimeoutSource<DrainThenHang>>> =
            VecDeque::from(vec![Ok(IdleTimeoutSource::new(
                DrainThenHang::new(&["after-reconnect"]),
                Duration::from_secs(90),
            ))]);
        let connect = move || {
            let next = replacements
                .pop_front()
                .unwrap_or_else(|| Err(anyhow::anyhow!("no more sources")));
            async move { next }
        };

        let initial =
            IdleTimeoutSource::new(DrainThenHang::new(&["before-stall"]), Duration::from_secs(90));
        let policy = ReconnectPolicy {
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            max_consecutive_failures: 1,
        };
        let mut src = ReconnectSource::new(initial, connect, policy);

        assert_eq!(src.next_message().await.unwrap().unwrap(), "before-stall");
        // Stall -> watchdog error -> transparent reconnect -> new data flows.
        assert_eq!(
            src.next_message().await.unwrap().unwrap(),
            "after-reconnect"
        );
    }
}
