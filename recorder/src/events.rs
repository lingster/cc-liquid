//! Domain event model for recorded Hyperliquid market data.
//!
//! These types are deliberately free of any I/O or transport concerns so they
//! can be unit-tested in isolation and reused by both the recorder (write path)
//! and any future replay engine (read path).

use serde::{Deserialize, Serialize};

/// A single price level in an L2 order book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Price of the level.
    pub px: f64,
    /// Total size resting at this level.
    pub sz: f64,
    /// Number of orders at this level.
    pub n: u32,
}

/// A full L2 book snapshot for a single coin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L2Book {
    pub coin: String,
    /// Exchange-provided event time in milliseconds.
    pub time_ms: i64,
    /// Bid levels, best-first.
    pub bids: Vec<Level>,
    /// Ask levels, best-first.
    pub asks: Vec<Level>,
}

/// A single public trade print.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub coin: String,
    /// Aggressor side: `Buy` or `Sell`.
    pub side: Side,
    pub px: f64,
    pub sz: f64,
    pub time_ms: i64,
}

/// Aggressor side of a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    /// Parse Hyperliquid's single-letter side encoding (`"B"`/`"A"`).
    ///
    /// Hyperliquid encodes the aggressor as `B` (buy) or `A` (ask/sell).
    pub fn from_hl(code: &str) -> Option<Self> {
        match code {
            "B" | "b" => Some(Side::Buy),
            "A" | "a" | "S" | "s" => Some(Side::Sell),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

/// Snapshot of all mid prices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllMids {
    /// Map of coin -> mid price.
    pub mids: Vec<(String, f64)>,
}

/// A parsed market event, independent of sequencing/transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MarketEvent {
    AllMids(AllMids),
    L2Book(L2Book),
    Trades(Vec<Trade>),
}

impl MarketEvent {
    /// Stream name this event belongs to (used for routing to storage tables).
    pub fn stream(&self) -> Stream {
        match self {
            MarketEvent::AllMids(_) => Stream::AllMids,
            MarketEvent::L2Book(_) => Stream::L2Book,
            MarketEvent::Trades(_) => Stream::Trades,
        }
    }
}

/// Logical stream identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    AllMids,
    L2Book,
    Trades,
}

/// A market event wrapped with deterministic ordering and timing metadata.
///
/// `seq` is a monotonic, gap-free counter assigned on receipt; `ts_recv_ms` is
/// the local receive time. Together they make replay deterministic regardless of
/// any clock skew in exchange timestamps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedEvent {
    pub seq: u64,
    /// Exchange event time in ms, when available (falls back to `ts_recv_ms`).
    pub ts_event_ms: i64,
    /// Local receive time in ms.
    pub ts_recv_ms: i64,
    pub payload: MarketEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_parses_hyperliquid_codes() {
        assert_eq!(Side::from_hl("B"), Some(Side::Buy));
        assert_eq!(Side::from_hl("A"), Some(Side::Sell));
        assert_eq!(Side::from_hl("x"), None);
    }

    #[test]
    fn market_event_reports_its_stream() {
        let ev = MarketEvent::AllMids(AllMids { mids: vec![] });
        assert_eq!(ev.stream(), Stream::AllMids);
        let ev = MarketEvent::Trades(vec![]);
        assert_eq!(ev.stream(), Stream::Trades);
    }
}
