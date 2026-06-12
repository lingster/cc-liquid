//! Replayed market state: the deterministic fold of recorded events.
//!
//! `MarketState` is the event-sourced projection — applying all events up to the
//! cursor in `seq` order yields the market as it was at that instant. This is
//! the substrate the digital twin queries for mids and L2 books.

use std::collections::HashMap;

use crate::events::{Level, MarketEvent, RecordedEvent};

/// Reconstructed L2 book for a single coin (levels best-first).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookState {
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub time_ms: i64,
}

impl BookState {
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.px)
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.px)
    }

    /// Mid derived from the top of book, when both sides exist.
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) / 2.0),
            _ => None,
        }
    }
}

/// The market as of the current replay cursor.
#[derive(Debug, Clone, Default)]
pub struct MarketState {
    mids: HashMap<String, f64>,
    books: HashMap<String, BookState>,
    last_trade: HashMap<String, f64>,
    cursor_seq: Option<u64>,
    cursor_ts_ms: Option<i64>,
}

impl MarketState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one event, advancing the cursor. Idempotent ordering is the
    /// caller's responsibility (events must arrive in `seq` order).
    pub fn apply(&mut self, event: &RecordedEvent) {
        match &event.payload {
            MarketEvent::AllMids(m) => {
                for (coin, px) in &m.mids {
                    self.mids.insert(coin.clone(), *px);
                }
            }
            MarketEvent::L2Book(b) => {
                self.books.insert(
                    b.coin.clone(),
                    BookState {
                        bids: b.bids.clone(),
                        asks: b.asks.clone(),
                        time_ms: b.time_ms,
                    },
                );
            }
            MarketEvent::Trades(ts) => {
                for t in ts {
                    self.last_trade.insert(t.coin.clone(), t.px);
                }
            }
        }
        self.cursor_seq = Some(event.seq);
        self.cursor_ts_ms = Some(event.ts_event_ms);
    }

    /// Best available price for a coin: explicit mid, else book mid, else last
    /// trade.
    pub fn price(&self, coin: &str) -> Option<f64> {
        if let Some(px) = self.mids.get(coin) {
            return Some(*px);
        }
        if let Some(mid) = self.books.get(coin).and_then(BookState::mid) {
            return Some(mid);
        }
        self.last_trade.get(coin).copied()
    }

    pub fn book(&self, coin: &str) -> Option<&BookState> {
        self.books.get(coin)
    }

    /// Snapshot of all mids known at the cursor (sorted for determinism).
    pub fn all_mids(&self) -> Vec<(String, f64)> {
        let mut out: Vec<_> = self.mids.iter().map(|(k, v)| (k.clone(), *v)).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn cursor_seq(&self) -> Option<u64> {
        self.cursor_seq
    }

    pub fn cursor_ts_ms(&self) -> Option<i64> {
        self.cursor_ts_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Side, Trade};

    fn ev(seq: u64, ts: i64, payload: MarketEvent) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload,
        }
    }

    #[test]
    fn all_mids_event_updates_prices_and_cursor() {
        let mut s = MarketState::new();
        s.apply(&ev(
            7,
            1234,
            MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), 95000.0), ("ETH".into(), 3200.0)],
            }),
        ));
        assert_eq!(s.price("BTC"), Some(95000.0));
        assert_eq!(s.cursor_seq(), Some(7));
        assert_eq!(s.cursor_ts_ms(), Some(1234));
    }

    #[test]
    fn later_event_overwrites_earlier_price() {
        let mut s = MarketState::new();
        s.apply(&ev(
            0,
            1,
            MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), 100.0)],
            }),
        ));
        s.apply(&ev(
            1,
            2,
            MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), 101.0)],
            }),
        ));
        assert_eq!(s.price("BTC"), Some(101.0));
    }

    #[test]
    fn book_reconstructs_and_derives_mid() {
        let mut s = MarketState::new();
        s.apply(&ev(
            0,
            1,
            MarketEvent::L2Book(L2Book {
                coin: "BTC".into(),
                time_ms: 1,
                bids: vec![Level {
                    px: 100.0,
                    sz: 1.0,
                    n: 1,
                }],
                asks: vec![Level {
                    px: 102.0,
                    sz: 1.0,
                    n: 1,
                }],
            }),
        ));
        let book = s.book("BTC").unwrap();
        assert_eq!(book.best_bid(), Some(100.0));
        assert_eq!(book.best_ask(), Some(102.0));
        // No explicit mid -> price falls back to book mid.
        assert_eq!(s.price("BTC"), Some(101.0));
    }

    #[test]
    fn price_falls_back_to_last_trade() {
        let mut s = MarketState::new();
        s.apply(&ev(
            0,
            1,
            MarketEvent::Trades(vec![Trade {
                coin: "SOL".into(),
                side: Side::Buy,
                px: 150.0,
                sz: 1.0,
                time_ms: 1,
            }]),
        ));
        assert_eq!(s.price("SOL"), Some(150.0));
    }

    #[test]
    fn unknown_coin_has_no_price() {
        let s = MarketState::new();
        assert_eq!(s.price("DOGE"), None);
    }
}
