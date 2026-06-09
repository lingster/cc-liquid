//! Tick navigation over a single coin's L2 snapshots.
//!
//! A [`Navigator`] holds the active coin and the current tick index into that
//! coin's snapshot list. Movement is always clamped to `[0, len-1]` so the UI
//! can wire raw button presses to `next`/`prev` without bounds checks. All
//! methods borrow [`SessionData`]; the navigator itself stores no books.

use crate::events::L2Book;
use crate::viewer::session_data::{SessionData, Snapshot};

/// Cursor over one coin's snapshot sequence.
#[derive(Debug, Clone)]
pub struct Navigator {
    coin: String,
    index: usize,
}

impl Navigator {
    /// Create a navigator positioned at the first tick of `coin` (index 0).
    ///
    /// `coin` may be unknown/empty; in that case [`Navigator::len`] is 0 and the
    /// accessors return `None`.
    pub fn new(coin: impl Into<String>) -> Self {
        Self {
            coin: coin.into(),
            index: 0,
        }
    }

    pub fn coin(&self) -> &str {
        &self.coin
    }

    /// Switch the active coin, resetting the cursor to the first tick.
    pub fn set_coin(&mut self, coin: impl Into<String>) {
        self.coin = coin.into();
        self.index = 0;
    }

    /// Current tick index (always within bounds when `len() > 0`).
    pub fn index(&self) -> usize {
        self.index
    }

    /// Number of ticks available for the active coin.
    pub fn len(&self, data: &SessionData) -> usize {
        data.tick_count(&self.coin)
    }

    pub fn is_empty(&self, data: &SessionData) -> bool {
        self.len(data) == 0
    }

    /// Advance one tick, clamped at the last index. No-op on empty sessions.
    pub fn next(&mut self, data: &SessionData) {
        let len = self.len(data);
        if len == 0 {
            return;
        }
        if self.index + 1 < len {
            self.index += 1;
        }
    }

    /// Step back one tick, clamped at index 0.
    pub fn prev(&mut self, _data: &SessionData) {
        self.index = self.index.saturating_sub(1);
    }

    /// Jump directly to `index`, clamped into range.
    pub fn seek_index(&mut self, data: &SessionData, index: usize) {
        let len = self.len(data);
        self.index = if len == 0 { 0 } else { index.min(len - 1) };
    }

    /// Jump to the snapshot whose `ts_event_ms` is nearest to `ts`.
    ///
    /// On ties the earlier snapshot wins. No-op on empty sessions.
    pub fn seek_to_time(&mut self, data: &SessionData, ts: i64) {
        let snaps = data.snapshots(&self.coin);
        if snaps.is_empty() {
            self.index = 0;
            return;
        }
        let mut best = 0usize;
        let mut best_dist = i64::MAX;
        for (i, s) in snaps.iter().enumerate() {
            let dist = s.ts_event_ms.saturating_sub(ts).saturating_abs();
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
        self.index = best;
    }

    /// Current snapshot, or `None` when the active coin has no ticks.
    pub fn current_snapshot<'a>(&self, data: &'a SessionData) -> Option<&'a Snapshot> {
        data.snapshots(&self.coin).get(self.index)
    }

    /// Current L2 book, or `None` when the active coin has no ticks.
    pub fn current_book<'a>(&self, data: &'a SessionData) -> Option<&'a L2Book> {
        self.current_snapshot(data).map(|s| &s.book)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Level, MarketEvent, RecordedEvent};

    fn book_event(seq: u64, ts: i64, coin: &str, bid: f64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::L2Book(L2Book {
                coin: coin.into(),
                time_ms: ts,
                bids: vec![Level {
                    px: bid,
                    sz: 1.0,
                    n: 1,
                }],
                asks: vec![Level {
                    px: bid + 1.0,
                    sz: 1.0,
                    n: 1,
                }],
            }),
        }
    }

    fn three_tick_btc() -> SessionData {
        SessionData::from_events(&[
            book_event(0, 100, "BTC", 10.0),
            book_event(1, 200, "BTC", 20.0),
            book_event(2, 300, "BTC", 30.0),
        ])
    }

    #[test]
    fn next_advances_then_clamps_at_end() {
        let data = three_tick_btc();
        let mut nav = Navigator::new("BTC");
        assert_eq!(nav.index(), 0);
        nav.next(&data);
        assert_eq!(nav.index(), 1);
        nav.next(&data);
        assert_eq!(nav.index(), 2);
        // Clamp: no wrap, no panic.
        nav.next(&data);
        assert_eq!(nav.index(), 2);
        assert_eq!(nav.current_book(&data).unwrap().bids[0].px, 30.0);
    }

    #[test]
    fn prev_steps_back_then_clamps_at_start() {
        let data = three_tick_btc();
        let mut nav = Navigator::new("BTC");
        nav.seek_index(&data, 2);
        nav.prev(&data);
        assert_eq!(nav.index(), 1);
        nav.prev(&data);
        assert_eq!(nav.index(), 0);
        nav.prev(&data);
        assert_eq!(nav.index(), 0);
    }

    #[test]
    fn empty_coin_is_safe() {
        let data = three_tick_btc();
        let mut nav = Navigator::new("ETH"); // not present
        assert!(nav.is_empty(&data));
        assert_eq!(nav.len(&data), 0);
        nav.next(&data);
        nav.prev(&data);
        nav.seek_index(&data, 5);
        nav.seek_to_time(&data, 12345);
        assert_eq!(nav.index(), 0);
        assert!(nav.current_book(&data).is_none());
    }

    #[test]
    fn single_tick_clamps_both_directions() {
        let data = SessionData::from_events(&[book_event(0, 100, "BTC", 10.0)]);
        let mut nav = Navigator::new("BTC");
        assert_eq!(nav.len(&data), 1);
        nav.next(&data);
        assert_eq!(nav.index(), 0);
        nav.prev(&data);
        assert_eq!(nav.index(), 0);
    }

    #[test]
    fn set_coin_resets_cursor() {
        let data = SessionData::from_events(&[
            book_event(0, 100, "BTC", 10.0),
            book_event(1, 200, "BTC", 20.0),
            book_event(2, 150, "ETH", 5.0),
        ]);
        let mut nav = Navigator::new("BTC");
        nav.next(&data);
        assert_eq!(nav.index(), 1);
        nav.set_coin("ETH");
        assert_eq!(nav.coin(), "ETH");
        assert_eq!(nav.index(), 0);
    }

    #[test]
    fn seek_index_clamps_into_range() {
        let data = three_tick_btc();
        let mut nav = Navigator::new("BTC");
        nav.seek_index(&data, 99);
        assert_eq!(nav.index(), 2);
        nav.seek_index(&data, 1);
        assert_eq!(nav.index(), 1);
    }

    #[test]
    fn seek_to_time_picks_nearest_snapshot() {
        let data = three_tick_btc(); // ts 100, 200, 300
        let mut nav = Navigator::new("BTC");
        nav.seek_to_time(&data, 180);
        assert_eq!(nav.index(), 1); // 200 nearer than 100
        nav.seek_to_time(&data, 0);
        assert_eq!(nav.index(), 0);
        nav.seek_to_time(&data, 100_000);
        assert_eq!(nav.index(), 2);
    }

    #[test]
    fn seek_to_time_tie_prefers_earlier() {
        let data = three_tick_btc(); // ts 100, 200, 300
        let mut nav = Navigator::new("BTC");
        // 150 is equidistant from 100 and 200 -> earlier (index 0) wins.
        nav.seek_to_time(&data, 150);
        assert_eq!(nav.index(), 0);
    }

    #[test]
    fn seek_to_time_extreme_timestamps_do_not_overflow() {
        // Malicious parquet can carry i64::MAX / i64::MIN ts_event_ms; the
        // distance computation must saturate, never panic or wrap (H1).
        let data = SessionData::from_events(&[
            book_event(0, i64::MIN, "BTC", 10.0),
            book_event(1, 0, "BTC", 20.0),
            book_event(2, i64::MAX, "BTC", 30.0),
        ]);
        let mut nav = Navigator::new("BTC");
        let len = nav.len(&data);
        assert_eq!(len, 3);

        for ts in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
            nav.seek_to_time(&data, ts);
            assert!(nav.index() < len, "index out of range for ts={ts}");
            assert!(nav.current_book(&data).is_some());
        }

        // Sanity: extreme positive ts lands on the i64::MAX snapshot.
        nav.seek_to_time(&data, i64::MAX);
        assert_eq!(nav.index(), 2);
        // Extreme negative ts lands on the i64::MIN snapshot.
        nav.seek_to_time(&data, i64::MIN);
        assert_eq!(nav.index(), 0);
    }

    #[test]
    fn seek_to_time_mid_session_tie_prefers_earlier() {
        let data = three_tick_btc(); // ts 100, 200, 300
        let mut nav = Navigator::new("BTC");
        // 250 is equidistant from tick 1 (200) and tick 2 (300) -> earlier wins.
        nav.seek_to_time(&data, 250);
        assert_eq!(nav.index(), 1);
    }
}
