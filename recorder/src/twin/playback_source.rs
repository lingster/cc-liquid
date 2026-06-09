//! [`PlaybackSource`]: replays a [`EventStream`] as a live-looking
//! [`EventSource`] of Hyperliquid wire frames.
//!
//! This is the bridge that closes the digital-twin loop: a playback stream (a
//! recorded session or a synthetic generator) is serialized to Hyperliquid JSON
//! and handed to the recorder through the very same `EventSource` trait the live
//! WebSocket client implements. The recorder cannot tell the difference.
//!
//! The source also publishes its current **logical timestamp** via a shared
//! atomic, so the recorder can stamp received events with playback time rather
//! than wall-clock time. That keeps replay deterministic and makes
//! time-encoded prices line up exactly with their recorded timestamps.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::replay::clock::{Clock, NoopClock, RealtimeClock};
use crate::replay::stream::EventStream;
use crate::source::EventSource;
use crate::wire::to_hl_string;

/// Wraps a playback [`EventStream`], emitting Hyperliquid frames and pacing them
/// with a [`Clock`].
pub struct PlaybackSource<S: EventStream, C: Clock> {
    stream: S,
    clock: C,
    ts: Arc<AtomicI64>,
    prev_ts: Option<i64>,
}

impl<S: EventStream> PlaybackSource<S, NoopClock> {
    /// Tick mode: emit frames as fast as the consumer pulls them (no pacing).
    pub fn tick(stream: S) -> Self {
        Self::new(stream, NoopClock)
    }
}

impl<S: EventStream> PlaybackSource<S, RealtimeClock> {
    /// Realtime mode: pace frames by their timestamp deltas, scaled by `speed`.
    pub fn realtime(stream: S, speed: f64) -> Self {
        Self::new(stream, RealtimeClock::new(speed))
    }
}

impl<S: EventStream, C: Clock> PlaybackSource<S, C> {
    pub fn new(stream: S, clock: C) -> Self {
        Self {
            stream,
            clock,
            ts: Arc::new(AtomicI64::new(0)),
            prev_ts: None,
        }
    }

    /// Shared handle to the current logical timestamp (ms), updated as each
    /// frame is emitted.
    pub fn ts_handle(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.ts)
    }

    /// Build a `now_ms` clock function for the recorder that reads this source's
    /// logical time, so recorded `ts_recv_ms` equals the emitted event time.
    pub fn now_ms_fn(&self) -> impl FnMut() -> i64 {
        let handle = self.ts_handle();
        move || handle.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl<S, C> EventSource for PlaybackSource<S, C>
where
    S: EventStream + Send,
    C: Clock + Send + Sync,
{
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        let event = self.stream.next_event()?;
        let ts = event.ts_event_ms;

        // Pace by the gap since the previous frame (no-op in tick mode).
        if let Some(prev) = self.prev_ts {
            self.clock.sleep_ms((ts - prev).max(0)).await;
        }
        self.prev_ts = Some(ts);
        self.ts.store(ts, Ordering::SeqCst);

        Some(Ok(to_hl_string(&event.payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_message;
    use crate::replay::synthetic::SyntheticEventStream;

    #[tokio::test]
    async fn emits_parseable_hyperliquid_frames_in_order() {
        let mut src = PlaybackSource::tick(SyntheticEventStream::ramp("BTC", 3));

        let m0 = src.next_message().await.unwrap().unwrap();
        let parsed = parse_message(&m0).unwrap().unwrap();
        // Frame is a valid allMids message carrying the ramp price 0.0.
        match parsed {
            crate::events::MarketEvent::AllMids(m) => assert_eq!(m.mids[0].1, 0.0),
            _ => panic!("expected allMids"),
        }

        // Logical timestamp tracks the emitted frame.
        assert_eq!(src.ts_handle().load(Ordering::SeqCst), 0);
        let _ = src.next_message().await.unwrap().unwrap();
        assert_eq!(src.ts_handle().load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn drains_then_ends() {
        let mut src = PlaybackSource::tick(SyntheticEventStream::ramp("BTC", 2));
        assert!(src.next_message().await.is_some());
        assert!(src.next_message().await.is_some());
        assert!(src.next_message().await.is_none());
    }
}
