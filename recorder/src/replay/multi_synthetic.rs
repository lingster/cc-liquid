//! Multi-coin synthetic FX generator.
//!
//! Broadcasts mids for an arbitrary set of pairs in a single `allMids` event per
//! tick — exactly how Hyperliquid's `allMids` channel carries the whole
//! universe. Each coin follows its own arithmetic ramp so the prices are
//! distinguishable, making this useful for exercising the twin's multi-currency
//! paths at scale without a recording.

use crate::events::{AllMids, MarketEvent, RecordedEvent};
use crate::replay::stream::EventStream;
use crate::replay::synthetic::{DEFAULT_STEP, TICK_UNIT_MS};

/// Per-coin price ramp specification.
#[derive(Debug, Clone, PartialEq)]
pub struct CoinRamp {
    pub coin: String,
    pub start: f64,
    pub step: f64,
}

/// Emits one `allMids` event per tick covering every configured coin.
pub struct MultiCoinSyntheticStream {
    coins: Vec<CoinRamp>,
    start_ts_ms: i64,
    interval_ms: i64,
    next_tick: u64,
    max_ticks: Option<u64>,
}

impl MultiCoinSyntheticStream {
    /// Build from explicit per-coin ramps.
    pub fn new(
        coins: Vec<CoinRamp>,
        start_ts_ms: i64,
        interval_ms: i64,
        max_ticks: Option<u64>,
    ) -> Self {
        Self {
            coins,
            start_ts_ms,
            interval_ms: interval_ms.max(1),
            next_tick: 0,
            max_ticks,
        }
    }

    /// Convenience: ramp `coins` where coin *i* starts at `i` and rises by
    /// `DEFAULT_STEP` each tick (1 ms per tick), bounded to `max_ticks`.
    pub fn ramp(coins: &[&str], max_ticks: u64) -> Self {
        let ramps = coins
            .iter()
            .enumerate()
            .map(|(i, c)| CoinRamp {
                coin: (*c).to_string(),
                start: i as f64,
                step: DEFAULT_STEP,
            })
            .collect();
        Self::new(ramps, 0, TICK_UNIT_MS, Some(max_ticks))
    }

    pub fn coin_count(&self) -> usize {
        self.coins.len()
    }
}

impl EventStream for MultiCoinSyntheticStream {
    fn next_event(&mut self) -> Option<RecordedEvent> {
        if let Some(max) = self.max_ticks {
            if self.next_tick >= max {
                return None;
            }
        }
        let tick = self.next_tick;
        self.next_tick += 1;

        let ts = self.start_ts_ms + (tick as i64) * self.interval_ms;
        let mids = self
            .coins
            .iter()
            .map(|c| (c.coin.clone(), c.start + tick as f64 * c.step))
            .collect();

        Some(RecordedEvent {
            seq: tick,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::AllMids(AllMids { mids }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mids(e: &RecordedEvent) -> Vec<(String, f64)> {
        match &e.payload {
            MarketEvent::AllMids(m) => m.mids.clone(),
            _ => panic!("expected AllMids"),
        }
    }

    #[test]
    fn broadcasts_all_coins_in_one_event_per_tick() {
        let mut s = MultiCoinSyntheticStream::ramp(&["BTC", "ETH", "SOL"], 2);
        let e0 = s.next_event().unwrap();
        let got = mids(&e0);
        assert_eq!(got.len(), 3, "one event carries every coin");
        // coin i starts at i.
        assert_eq!(got[0], ("BTC".to_string(), 0.0));
        assert_eq!(got[1], ("ETH".to_string(), 1.0));
        assert_eq!(got[2], ("SOL".to_string(), 2.0));
    }

    #[test]
    fn each_coin_advances_by_its_own_step() {
        let mut s = MultiCoinSyntheticStream::ramp(&["BTC", "ETH"], 3);
        let _ = s.next_event();
        let e1 = s.next_event().unwrap();
        let got = mids(&e1);
        assert!((got[0].1 - 0.0001).abs() < 1e-12); // BTC: 0 + 0.0001
        assert!((got[1].1 - 1.0001).abs() < 1e-12); // ETH: 1 + 0.0001
    }

    #[test]
    fn is_bounded_by_max_ticks() {
        let mut s = MultiCoinSyntheticStream::ramp(&["BTC"], 2);
        assert!(s.next_event().is_some());
        assert!(s.next_event().is_some());
        assert!(s.next_event().is_none());
    }
}
