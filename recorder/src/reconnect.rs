//! A self-healing [`EventSource`] wrapper.
//!
//! Live WebSocket connections drop — the server resets, the network blips. A
//! single disconnect must not end a long recording. [`ReconnectSource`] wraps an
//! inner source and a `connect` factory: when the inner source closes or errors,
//! it transparently reconnects (re-running the factory, which re-sends
//! subscriptions) with exponential backoff, and keeps yielding messages.
//!
//! It is generic over the source type and the factory future, so it is tested
//! with in-memory scripted sources — no network required. The recorder's
//! deadline still bounds total wall-clock: a reconnect backoff is just a pending
//! future the recorder's `select!` can drop when the timer fires.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{error, warn};

use crate::source::EventSource;

/// Backoff and give-up policy for reconnection.
#[derive(Debug, Clone, Copy)]
pub struct ReconnectPolicy {
    /// Delay before the first reconnect attempt (grows exponentially).
    pub base_backoff: Duration,
    /// Ceiling for the exponential backoff.
    pub max_backoff: Duration,
    /// Give up after this many *consecutive* failed connect attempts.
    pub max_consecutive_failures: usize,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(5),
            // High enough that the recording deadline, not the policy, is the
            // practical stop condition for transient outages.
            max_consecutive_failures: 1000,
        }
    }
}

/// Wraps an [`EventSource`] and reconnects via `connect` when it ends or errors.
pub struct ReconnectSource<F, Fut, S>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = anyhow::Result<S>> + Send,
    S: EventSource + Send,
{
    current: Option<S>,
    connect: F,
    policy: ReconnectPolicy,
    exhausted: bool,
}

impl<F, Fut, S> ReconnectSource<F, Fut, S>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = anyhow::Result<S>> + Send,
    S: EventSource + Send,
{
    /// Build from an already-connected `initial` source plus a `connect` factory
    /// used only for *re*connection. The initial connect is the caller's job, so
    /// a hard failure to connect at all still surfaces immediately upstream.
    pub fn new(initial: S, connect: F, policy: ReconnectPolicy) -> Self {
        Self {
            current: Some(initial),
            connect,
            policy,
            exhausted: false,
        }
    }

    /// Reconnect with exponential backoff. Returns `true` on success, `false`
    /// once `max_consecutive_failures` is reached.
    async fn reconnect(&mut self) -> bool {
        let mut delay = self.policy.base_backoff;
        let mut failures = 0usize;
        loop {
            // Pause before every (re)connect attempt — bounds both backoff and
            // the rate of a flapping connection.
            tokio::time::sleep(delay).await;
            match (self.connect)().await {
                Ok(source) => {
                    if failures > 0 {
                        warn!("reconnected after {failures} failed attempt(s)");
                    }
                    self.current = Some(source);
                    return true;
                }
                Err(e) => {
                    failures += 1;
                    warn!("reconnect attempt {failures} failed: {e}");
                    if failures >= self.policy.max_consecutive_failures {
                        error!("giving up after {failures} consecutive reconnect failures");
                        return false;
                    }
                    delay = (delay * 2).min(self.policy.max_backoff);
                }
            }
        }
    }
}

#[async_trait]
impl<F, Fut, S> EventSource for ReconnectSource<F, Fut, S>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = anyhow::Result<S>> + Send,
    S: EventSource + Send,
{
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        if self.exhausted {
            return None;
        }
        loop {
            if self.current.is_none() && !self.reconnect().await {
                self.exhausted = true;
                return None;
            }
            match self.current.as_mut().unwrap().next_message().await {
                Some(Ok(text)) => return Some(Ok(text)),
                Some(Err(e)) => {
                    warn!("stream error, reconnecting: {e}");
                    self.current = None;
                }
                None => {
                    warn!("stream closed, reconnecting");
                    self.current = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A source that yields scripted results (including errors), then ends.
    struct VecSource {
        items: VecDeque<anyhow::Result<String>>,
    }

    impl VecSource {
        fn ok(msgs: &[&str]) -> Self {
            Self {
                items: msgs.iter().map(|m| Ok(m.to_string())).collect(),
            }
        }
    }

    #[async_trait]
    impl EventSource for VecSource {
        async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
            self.items.pop_front()
        }
    }

    /// Fast test policy: no real waiting, give up after a single failure.
    fn instant_policy(max_failures: usize) -> ReconnectPolicy {
        ReconnectPolicy {
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            max_consecutive_failures: max_failures,
        }
    }

    async fn drain<S: EventSource>(src: &mut S) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(msg) = src.next_message().await {
            out.push(msg.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn transparently_reconnects_when_a_source_ends() {
        // initial drains, factory hands out a second source, then errors -> stop.
        let mut queue: VecDeque<anyhow::Result<VecSource>> =
            VecDeque::from(vec![Ok(VecSource::ok(&["c"]))]);
        let connect = move || {
            let next = queue
                .pop_front()
                .unwrap_or_else(|| Err(anyhow::anyhow!("no more sources")));
            async move { next }
        };

        let mut src = ReconnectSource::new(VecSource::ok(&["a", "b"]), connect, instant_policy(1));
        assert_eq!(drain(&mut src).await, vec!["a", "b", "c"]);
        // Terminal: stays ended.
        assert!(src.next_message().await.is_none());
    }

    #[tokio::test]
    async fn reconnects_on_a_stream_error() {
        let mut source1 = VecSource::ok(&["a"]);
        source1.items.push_back(Err(anyhow::anyhow!("boom")));

        let mut queue: VecDeque<anyhow::Result<VecSource>> =
            VecDeque::from(vec![Ok(VecSource::ok(&["b"]))]);
        let connect = move || {
            let next = queue
                .pop_front()
                .unwrap_or_else(|| Err(anyhow::anyhow!("no more sources")));
            async move { next }
        };

        let mut src = ReconnectSource::new(source1, connect, instant_policy(1));
        // The mid-stream error is swallowed; recording continues on source 2.
        assert_eq!(drain(&mut src).await, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn gives_up_after_max_consecutive_failures() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_in = attempts.clone();
        let connect = move || {
            attempts_in.fetch_add(1, Ordering::SeqCst);
            async move { Err::<VecSource, _>(anyhow::anyhow!("connect refused")) }
        };

        // Initial source ends immediately, forcing a reconnect that always fails.
        let mut src = ReconnectSource::new(VecSource::ok(&[]), connect, instant_policy(3));
        assert!(src.next_message().await.is_none());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        // Terminal and non-blocking on subsequent polls.
        assert!(src.next_message().await.is_none());
    }
}
