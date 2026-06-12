//! Synthetic ("test mode") event stream.
//!
//! Instead of reading a recorded session, this generates a deterministic ramp:
//! the price starts at `start` (default 0.0) and increases by `step`
//! (default 0.0001) every tick, with the logical clock advancing by one
//! Hyperliquid time unit (1 ms) per tick. Useful for exercising the twin's
//! plumbing and order logic against perfectly predictable prices.

use crate::events::{AllMids, MarketEvent, RecordedEvent};
use crate::replay::stream::EventStream;

/// Hyperliquid's smallest advertised time unit, in milliseconds.
pub const TICK_UNIT_MS: i64 = 1;

/// Default per-tick price increment (4 dp, as requested).
pub const DEFAULT_STEP: f64 = 0.0001;

/// Generates an arithmetic price ramp as `allMids` events for a single coin.
pub struct SyntheticEventStream {
    coin: String,
    start: f64,
    step: f64,
    start_ts_ms: i64,
    next_tick: u64,
    max_ticks: Option<u64>,
}

impl SyntheticEventStream {
    /// Create a bounded ramp of `max_ticks` ticks for `coin`, starting at 0.0
    /// and rising by `DEFAULT_STEP` each tick.
    pub fn ramp(coin: impl Into<String>, max_ticks: u64) -> Self {
        Self {
            coin: coin.into(),
            start: 0.0,
            step: DEFAULT_STEP,
            start_ts_ms: 0,
            next_tick: 0,
            max_ticks: Some(max_ticks),
        }
    }

    /// Fully configurable ramp. `max_ticks = None` yields an unbounded stream.
    pub fn new(
        coin: impl Into<String>,
        start: f64,
        step: f64,
        start_ts_ms: i64,
        max_ticks: Option<u64>,
    ) -> Self {
        Self {
            coin: coin.into(),
            start,
            step,
            start_ts_ms,
            next_tick: 0,
            max_ticks,
        }
    }

    /// Price at a given tick index, exposed for assertions/documentation.
    pub fn price_at(&self, tick: u64) -> f64 {
        self.start + (tick as f64) * self.step
    }
}

impl EventStream for SyntheticEventStream {
    fn next_event(&mut self) -> Option<RecordedEvent> {
        if let Some(max) = self.max_ticks {
            if self.next_tick >= max {
                return None;
            }
        }
        let tick = self.next_tick;
        self.next_tick += 1;

        let price = self.price_at(tick);
        let ts = self.start_ts_ms + (tick as i64) * TICK_UNIT_MS;
        Some(RecordedEvent {
            seq: tick,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![(self.coin.clone(), price)],
            }),
        })
    }
}

/// Convert a millisecond timestamp into a `seconds.milliseconds` price.
///
/// The seconds component wraps every minute (0–59) and the millisecond
/// component is the fractional part, e.g. `12_345 ms -> 12.345`,
/// `61_000 ms -> 1.000`. Negative inputs are handled via Euclidean remainder.
pub fn time_to_price(ts_ms: i64) -> f64 {
    let ms = ts_ms.rem_euclid(1000);
    let seconds = ts_ms.div_euclid(1000).rem_euclid(60);
    seconds as f64 + ms as f64 / 1000.0
}

/// "Time mode" event stream: emits `allMids` events whose price encodes the
/// event's own timestamp as `seconds.milliseconds` (see [`time_to_price`]).
///
/// Pairs naturally with **realtime** playback: as wall-clock time advances, the
/// reported price tracks the live clock. (The arithmetic [`SyntheticEventStream`]
/// ramp pairs naturally with **tick** mode.)
pub struct TimeRampEventStream {
    coin: String,
    start_ts_ms: i64,
    interval_ms: i64,
    next_tick: u64,
    max_ticks: Option<u64>,
}

impl TimeRampEventStream {
    /// Emit a tick every `interval_ms`, starting at `start_ts_ms`.
    pub fn new(
        coin: impl Into<String>,
        start_ts_ms: i64,
        interval_ms: i64,
        max_ticks: Option<u64>,
    ) -> Self {
        Self {
            coin: coin.into(),
            start_ts_ms,
            interval_ms: interval_ms.max(1),
            next_tick: 0,
            max_ticks,
        }
    }

    /// Timestamp of a given tick index.
    pub fn ts_at(&self, tick: u64) -> i64 {
        self.start_ts_ms + (tick as i64) * self.interval_ms
    }
}

impl EventStream for TimeRampEventStream {
    fn next_event(&mut self) -> Option<RecordedEvent> {
        if let Some(max) = self.max_ticks {
            if self.next_tick >= max {
                return None;
            }
        }
        let tick = self.next_tick;
        self.next_tick += 1;

        let ts = self.ts_at(tick);
        Some(RecordedEvent {
            seq: tick,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![(self.coin.clone(), time_to_price(ts))],
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_to_price_encodes_seconds_and_millis() {
        assert!((time_to_price(12_345) - 12.345).abs() < 1e-12);
        assert!((time_to_price(61_000) - 1.000).abs() < 1e-12);
        assert!((time_to_price(59_999) - 59.999).abs() < 1e-12);
        assert!((time_to_price(0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn time_ramp_price_tracks_its_timestamp() {
        // Tick every 100ms starting at 12_000ms -> 12.000, 12.100, 12.200 ...
        let mut s = TimeRampEventStream::new("TEST", 12_000, 100, Some(3));
        let e0 = s.next_event().unwrap();
        let e1 = s.next_event().unwrap();
        let e2 = s.next_event().unwrap();
        assert_eq!(e0.ts_event_ms, 12_000);
        assert!((price_of(&e0) - 12.000).abs() < 1e-12);
        assert!((price_of(&e1) - 12.100).abs() < 1e-12);
        assert!((price_of(&e2) - 12.200).abs() < 1e-12);
        assert!(s.next_event().is_none());
    }

    #[test]
    fn first_tick_is_zero_then_rises_by_step() {
        let mut s = SyntheticEventStream::ramp("TEST", 3);
        let e0 = s.next_event().unwrap();
        let e1 = s.next_event().unwrap();
        let e2 = s.next_event().unwrap();

        assert_eq!(price_of(&e0), 0.0);
        assert!((price_of(&e1) - 0.0001).abs() < 1e-12);
        assert!((price_of(&e2) - 0.0002).abs() < 1e-12);
        assert!(s.next_event().is_none(), "ramp is bounded by max_ticks");
    }

    #[test]
    fn clock_advances_one_ms_per_tick() {
        let mut s = SyntheticEventStream::ramp("TEST", 2);
        assert_eq!(s.next_event().unwrap().ts_event_ms, 0);
        assert_eq!(s.next_event().unwrap().ts_event_ms, 1);
    }

    #[test]
    fn seq_is_monotonic() {
        let mut s = SyntheticEventStream::ramp("TEST", 2);
        assert_eq!(s.next_event().unwrap().seq, 0);
        assert_eq!(s.next_event().unwrap().seq, 1);
    }

    fn price_of(e: &RecordedEvent) -> f64 {
        match &e.payload {
            MarketEvent::AllMids(m) => m.mids[0].1,
            _ => panic!("expected AllMids"),
        }
    }
}
