//! The replay engine: drives an [`EventStream`] into a [`MarketState`].
//!
//! Two playback styles, both over the same stream + state:
//!
//! * **Tick mode** (`step` / `next_price`) — pull-based. Each call applies the
//!   next event and advances the cursor by one tick (Hyperliquid's resolution
//!   is 1 ms). The caller controls cadence; nothing sleeps.
//! * **Realtime mode** (`run_realtime`) — push-based. The engine walks the whole
//!   stream, pacing with a [`Clock`] so the gaps between events elapse in
//!   real (or scaled) wall-clock time, invoking a callback at each tick.

use crate::replay::clock::Clock;
use crate::replay::state::MarketState;
use crate::replay::stream::EventStream;

/// Replays recorded (or synthetic) events into market state.
pub struct ReplayEngine<S: EventStream> {
    stream: S,
    state: MarketState,
    started: bool,
    ended: bool,
    last_applied_ts: Option<i64>,
}

impl<S: EventStream> ReplayEngine<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            state: MarketState::new(),
            started: false,
            ended: false,
            last_applied_ts: None,
        }
    }

    /// Tick mode: apply the next event. Returns `false` once the stream is
    /// exhausted.
    pub fn step(&mut self) -> bool {
        if self.ended {
            return false;
        }
        match self.stream.next_event() {
            Some(ev) => {
                self.state.apply(&ev);
                self.started = true;
                self.last_applied_ts = Some(ev.ts_event_ms);
                true
            }
            None => {
                self.ended = true;
                false
            }
        }
    }

    /// Tick mode convenience: return the *current* price for `coin`, then
    /// advance one tick — matching "return the current tick price and then
    /// increment". The first call primes the cursor with the first event.
    pub fn next_price(&mut self, coin: &str) -> Option<f64> {
        if !self.started {
            self.step();
        }
        let current = self.state.price(coin);
        self.step();
        current
    }

    /// Realtime mode: replay the remaining stream, pacing with `clock` and
    /// invoking `on_tick` after each applied event. Returns the number of ticks
    /// emitted.
    pub async fn run_realtime<C, F>(&mut self, clock: &C, mut on_tick: F) -> u64
    where
        C: Clock,
        F: FnMut(&MarketState),
    {
        let mut ticks = 0;
        while !self.ended {
            match self.stream.next_event() {
                Some(ev) => {
                    // Wait out the gap since the previously revealed tick before
                    // making this one visible.
                    if let Some(prev) = self.last_applied_ts {
                        clock.sleep_ms((ev.ts_event_ms - prev).max(0)).await;
                    }
                    self.state.apply(&ev);
                    self.started = true;
                    self.last_applied_ts = Some(ev.ts_event_ms);
                    on_tick(&self.state);
                    ticks += 1;
                }
                None => self.ended = true,
            }
        }
        ticks
    }

    pub fn state(&self) -> &MarketState {
        &self.state
    }

    pub fn price(&self, coin: &str) -> Option<f64> {
        self.state.price(coin)
    }

    pub fn book(&self, coin: &str) -> Option<&crate::replay::state::BookState> {
        self.state.book(coin)
    }

    pub fn cursor_ts_ms(&self) -> Option<i64> {
        self.state.cursor_ts_ms()
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::clock::ManualClock;
    use crate::replay::synthetic::SyntheticEventStream;

    #[test]
    fn tick_mode_steps_through_events() {
        let mut engine = ReplayEngine::new(SyntheticEventStream::ramp("TEST", 3));
        assert!(engine.step());
        assert_eq!(engine.price("TEST"), Some(0.0));
        assert!(engine.step());
        assert!((engine.price("TEST").unwrap() - 0.0001).abs() < 1e-12);
        assert!(engine.step());
        assert!((engine.price("TEST").unwrap() - 0.0002).abs() < 1e-12);
        // Stream exhausted.
        assert!(!engine.step());
        assert!(engine.is_ended());
    }

    #[test]
    fn next_price_returns_current_then_increments() {
        let mut engine = ReplayEngine::new(SyntheticEventStream::ramp("TEST", 3));
        // First call primes with tick 0 and returns it.
        assert_eq!(engine.next_price("TEST"), Some(0.0));
        // Now the cursor sits on tick 1.
        assert!((engine.next_price("TEST").unwrap() - 0.0001).abs() < 1e-12);
        assert!((engine.next_price("TEST").unwrap() - 0.0002).abs() < 1e-12);
    }

    #[tokio::test]
    async fn realtime_mode_paces_by_timestamp_deltas() {
        // Custom spacing: ticks at t=0,10,30 ms.
        let events = vec![
            mids_ev(0, 0, 100.0),
            mids_ev(1, 10, 101.0),
            mids_ev(2, 30, 102.0),
        ];
        let mut engine = ReplayEngine::new(crate::replay::stream::VecEventStream::new(events));
        let clock = ManualClock::new();

        let mut seen = Vec::new();
        let ticks = engine
            .run_realtime(&clock, |st| seen.push(st.price("BTC").unwrap()))
            .await;

        assert_eq!(ticks, 3);
        assert_eq!(seen, vec![100.0, 101.0, 102.0]);
        // First tick has no preceding gap; then 10ms and 20ms gaps.
        assert_eq!(clock.sleeps(), vec![10, 20]);
    }

    fn mids_ev(seq: u64, ts: i64, px: f64) -> crate::events::RecordedEvent {
        use crate::events::{AllMids, MarketEvent, RecordedEvent};
        RecordedEvent {
            seq,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), px)],
            }),
        }
    }
}
