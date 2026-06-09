//! Parses raw Hyperliquid WebSocket JSON messages into domain [`MarketEvent`]s.
//!
//! Hyperliquid sends prices/sizes as JSON strings; this module is the single
//! place that knows the wire format, keeping the rest of the system in typed
//! domain values.

use serde_json::Value;
use thiserror::Error;

use crate::events::{AllMids, L2Book, Level, MarketEvent, Side, Trade};

#[derive(Debug, Error, PartialEq)]
pub enum ParseError {
    #[error("message is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("missing field `{0}`")]
    MissingField(&'static str),
    #[error("field `{field}` has unexpected type/value: {detail}")]
    BadField { field: &'static str, detail: String },
}

/// Parse a raw WebSocket text frame.
///
/// Returns:
/// - `Ok(Some(event))` for a recognized data message,
/// - `Ok(None)` for control/non-data channels (e.g. `pong`,
///   `subscriptionResponse`) which are intentionally not recorded,
/// - `Err(_)` for malformed data on a recognized data channel.
pub fn parse_message(raw: &str) -> Result<Option<MarketEvent>, ParseError> {
    let value: Value =
        serde_json::from_str(raw).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

    let channel = value
        .get("channel")
        .and_then(Value::as_str)
        .ok_or(ParseError::MissingField("channel"))?;

    let data = value.get("data");

    match channel {
        "allMids" => Ok(Some(parse_all_mids(require(data, "data")?)?)),
        "l2Book" => Ok(Some(parse_l2_book(require(data, "data")?)?)),
        "trades" => Ok(Some(parse_trades(require(data, "data")?)?)),
        // Control channels we acknowledge but do not record.
        _ => Ok(None),
    }
}

fn require<'a>(data: Option<&'a Value>, field: &'static str) -> Result<&'a Value, ParseError> {
    data.ok_or(ParseError::MissingField(field))
}

fn parse_all_mids(data: &Value) -> Result<MarketEvent, ParseError> {
    let obj = data
        .get("mids")
        .and_then(Value::as_object)
        .ok_or(ParseError::MissingField("mids"))?;

    let mut mids = Vec::with_capacity(obj.len());
    for (coin, px) in obj {
        mids.push((coin.clone(), parse_f64(px, "mids")?));
    }
    // Deterministic ordering regardless of JSON map iteration order.
    mids.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(MarketEvent::AllMids(AllMids { mids }))
}

fn parse_l2_book(data: &Value) -> Result<MarketEvent, ParseError> {
    let coin = data
        .get("coin")
        .and_then(Value::as_str)
        .ok_or(ParseError::MissingField("coin"))?
        .to_string();
    let time_ms = data
        .get("time")
        .and_then(Value::as_i64)
        .ok_or(ParseError::MissingField("time"))?;

    let levels = data
        .get("levels")
        .and_then(Value::as_array)
        .ok_or(ParseError::MissingField("levels"))?;
    if levels.len() != 2 {
        return Err(ParseError::BadField {
            field: "levels",
            detail: format!("expected [bids, asks], got {} arrays", levels.len()),
        });
    }

    let bids = parse_levels(&levels[0])?;
    let asks = parse_levels(&levels[1])?;
    Ok(MarketEvent::L2Book(L2Book {
        coin,
        time_ms,
        bids,
        asks,
    }))
}

fn parse_levels(value: &Value) -> Result<Vec<Level>, ParseError> {
    let arr = value.as_array().ok_or(ParseError::BadField {
        field: "levels",
        detail: "side is not an array".into(),
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for lvl in arr {
        out.push(Level {
            px: parse_f64(lvl.get("px").ok_or(ParseError::MissingField("px"))?, "px")?,
            sz: parse_f64(lvl.get("sz").ok_or(ParseError::MissingField("sz"))?, "sz")?,
            n: lvl
                .get("n")
                .and_then(Value::as_u64)
                .ok_or(ParseError::MissingField("n"))? as u32,
        });
    }
    Ok(out)
}

fn parse_trades(data: &Value) -> Result<MarketEvent, ParseError> {
    let arr = data.as_array().ok_or(ParseError::BadField {
        field: "data",
        detail: "trades payload is not an array".into(),
    })?;
    let mut trades = Vec::with_capacity(arr.len());
    for t in arr {
        let side_code = t
            .get("side")
            .and_then(Value::as_str)
            .ok_or(ParseError::MissingField("side"))?;
        let side = Side::from_hl(side_code).ok_or(ParseError::BadField {
            field: "side",
            detail: format!("unknown side code `{side_code}`"),
        })?;
        trades.push(Trade {
            coin: t
                .get("coin")
                .and_then(Value::as_str)
                .ok_or(ParseError::MissingField("coin"))?
                .to_string(),
            side,
            px: parse_f64(t.get("px").ok_or(ParseError::MissingField("px"))?, "px")?,
            sz: parse_f64(t.get("sz").ok_or(ParseError::MissingField("sz"))?, "sz")?,
            time_ms: t
                .get("time")
                .and_then(Value::as_i64)
                .ok_or(ParseError::MissingField("time"))?,
        });
    }
    Ok(MarketEvent::Trades(trades))
}

/// Parse a numeric value that Hyperliquid may send as either a JSON string or
/// number.
fn parse_f64(value: &Value, field: &'static str) -> Result<f64, ParseError> {
    if let Some(n) = value.as_f64() {
        return Ok(n);
    }
    if let Some(s) = value.as_str() {
        return s.parse::<f64>().map_err(|_| ParseError::BadField {
            field,
            detail: format!("`{s}` is not a number"),
        });
    }
    Err(ParseError::BadField {
        field,
        detail: format!("{value} is not a number"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_mids_with_string_prices() {
        let raw = r#"{"channel":"allMids","data":{"mids":{"ETH":"3200.5","BTC":"95000.0"}}}"#;
        let ev = parse_message(raw).unwrap().unwrap();
        match ev {
            MarketEvent::AllMids(m) => {
                // sorted by coin for determinism
                assert_eq!(m.mids[0], ("BTC".to_string(), 95000.0));
                assert_eq!(m.mids[1], ("ETH".to_string(), 3200.5));
            }
            _ => panic!("expected AllMids"),
        }
    }

    #[test]
    fn parses_l2_book_bids_and_asks() {
        let raw = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1700000000123,
            "levels":[[{"px":"94999","sz":"1.5","n":3}],[{"px":"95001","sz":"2.0","n":5}]]}}"#;
        let ev = parse_message(raw).unwrap().unwrap();
        match ev {
            MarketEvent::L2Book(b) => {
                assert_eq!(b.coin, "BTC");
                assert_eq!(b.time_ms, 1700000000123);
                assert_eq!(
                    b.bids,
                    vec![Level {
                        px: 94999.0,
                        sz: 1.5,
                        n: 3
                    }]
                );
                assert_eq!(
                    b.asks,
                    vec![Level {
                        px: 95001.0,
                        sz: 2.0,
                        n: 5
                    }]
                );
            }
            _ => panic!("expected L2Book"),
        }
    }

    #[test]
    fn parses_trades_with_sides() {
        let raw = r#"{"channel":"trades","data":[
            {"coin":"BTC","side":"B","px":"95000","sz":"0.1","time":1700000000000},
            {"coin":"BTC","side":"A","px":"94990","sz":"0.2","time":1700000000050}]}"#;
        let ev = parse_message(raw).unwrap().unwrap();
        match ev {
            MarketEvent::Trades(ts) => {
                assert_eq!(ts.len(), 2);
                assert_eq!(ts[0].side, Side::Buy);
                assert_eq!(ts[1].side, Side::Sell);
                assert_eq!(ts[0].px, 95000.0);
            }
            _ => panic!("expected Trades"),
        }
    }

    #[test]
    fn control_channels_are_ignored() {
        assert_eq!(parse_message(r#"{"channel":"pong"}"#).unwrap(), None);
        assert_eq!(
            parse_message(r#"{"channel":"subscriptionResponse","data":{}}"#).unwrap(),
            None
        );
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(matches!(
            parse_message("not json"),
            Err(ParseError::InvalidJson(_))
        ));
    }

    #[test]
    fn malformed_l2_book_is_an_error() {
        // Only one side array instead of [bids, asks].
        let raw = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1,"levels":[[]]}}"#;
        assert!(matches!(
            parse_message(raw),
            Err(ParseError::BadField {
                field: "levels",
                ..
            })
        ));
    }
}
