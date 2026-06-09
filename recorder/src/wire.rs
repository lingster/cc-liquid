//! Serializes domain [`MarketEvent`]s back into Hyperliquid WebSocket JSON.
//!
//! This is the inverse of [`crate::parser`]. It lets the digital twin's
//! playback emit frames in the exact wire format the live API produces, so the
//! recorder ingests replayed data through the identical parse path as live data
//! (prices/sizes as strings, sides as `B`/`A`, books as `[bids, asks]`).

use serde_json::{json, Map, Value};

use crate::events::{Level, MarketEvent, Side};

/// Render a market event as a Hyperliquid channel message `Value`.
pub fn to_hl_message(event: &MarketEvent) -> Value {
    match event {
        MarketEvent::AllMids(m) => {
            let mut mids = Map::new();
            for (coin, px) in &m.mids {
                mids.insert(coin.clone(), Value::String(px.to_string()));
            }
            json!({ "channel": "allMids", "data": { "mids": Value::Object(mids) } })
        }
        MarketEvent::L2Book(b) => {
            json!({
                "channel": "l2Book",
                "data": {
                    "coin": b.coin,
                    "time": b.time_ms,
                    "levels": [levels_to_json(&b.bids), levels_to_json(&b.asks)],
                }
            })
        }
        MarketEvent::Trades(ts) => {
            let trades: Vec<Value> = ts
                .iter()
                .map(|t| {
                    json!({
                        "coin": t.coin,
                        "side": side_code(t.side),
                        "px": t.px.to_string(),
                        "sz": t.sz.to_string(),
                        "time": t.time_ms,
                    })
                })
                .collect();
            json!({ "channel": "trades", "data": trades })
        }
    }
}

/// Render a market event as a compact JSON string ready for the wire.
pub fn to_hl_string(event: &MarketEvent) -> String {
    to_hl_message(event).to_string()
}

fn levels_to_json(levels: &[Level]) -> Value {
    Value::Array(
        levels
            .iter()
            .map(|l| json!({ "px": l.px.to_string(), "sz": l.sz.to_string(), "n": l.n }))
            .collect(),
    )
}

fn side_code(side: Side) -> &'static str {
    match side {
        Side::Buy => "B",
        Side::Sell => "A",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Trade};
    use crate::parser::parse_message;

    /// The wire serializer must be the exact inverse of the parser.
    fn assert_round_trips(event: MarketEvent) {
        let raw = to_hl_string(&event);
        let parsed = parse_message(&raw)
            .expect("serialized message must parse")
            .expect("serialized message must be a data channel");
        assert_eq!(parsed, event, "round-trip mismatch for {raw}");
    }

    #[test]
    fn all_mids_round_trips() {
        assert_round_trips(MarketEvent::AllMids(AllMids {
            mids: vec![("BTC".into(), 95000.0), ("ETH".into(), 3200.5)],
        }));
    }

    #[test]
    fn l2_book_round_trips() {
        assert_round_trips(MarketEvent::L2Book(L2Book {
            coin: "BTC".into(),
            time_ms: 1700000000123,
            bids: vec![Level {
                px: 94999.0,
                sz: 1.5,
                n: 3,
            }],
            asks: vec![Level {
                px: 95001.0,
                sz: 2.0,
                n: 5,
            }],
        }));
    }

    #[test]
    fn trades_round_trip_with_sides() {
        assert_round_trips(MarketEvent::Trades(vec![
            Trade {
                coin: "BTC".into(),
                side: Side::Buy,
                px: 95000.0,
                sz: 0.1,
                time_ms: 1,
            },
            Trade {
                coin: "ETH".into(),
                side: Side::Sell,
                px: 3200.0,
                sz: 1.0,
                time_ms: 2,
            },
        ]));
    }

    #[test]
    fn small_increasing_prices_round_trip_exactly() {
        // The ramp uses k * 0.0001; ensure f64 string round-trips are exact.
        for k in 0..10u64 {
            let px = k as f64 * 0.0001;
            assert_round_trips(MarketEvent::AllMids(AllMids {
                mids: vec![("TEST".into(), px)],
            }));
        }
    }
}
