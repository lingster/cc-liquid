//! Order domain types and Hyperliquid wire parsing (PRD §7, §B.3).
//!
//! The wire format is what the SDK signs and POSTs to `/exchange`:
//! `{"action":{"type":"order","orders":[{"a":asset,"b":isBuy,"p":px,"s":sz,
//! "r":reduceOnly,"t":{"limit":{"tif":...}}|{"trigger":{...}}}]}, ...}`.
//! Asset indices map to coins by position in the `meta` universe — the same
//! universe the playback market serves, so both sides always agree.

use serde_json::Value;

/// Time-in-force for limit orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tif {
    /// Immediate-or-cancel: fill what crosses, cancel the rest.
    Ioc,
    /// Good-till-cancel: fill what crosses, rest the remainder.
    Gtc,
    /// Add-liquidity-only (post-only): reject if it would cross.
    Alo,
}

impl Tif {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Ioc" => Some(Tif::Ioc),
            "Gtc" => Some(Tif::Gtc),
            "Alo" => Some(Tif::Alo),
            _ => None,
        }
    }
}

/// Trigger flavour for stop-loss / take-profit orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    StopLoss,
    TakeProfit,
}

/// How an order executes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderType {
    Limit {
        tif: Tif,
    },
    /// Activates when the mark crosses `trigger_px`, then executes as a
    /// market (`is_market`) or limit order at the order's `limit_px`.
    Trigger {
        trigger_px: f64,
        is_market: bool,
        kind: TriggerKind,
    },
}

/// A validated, coin-resolved order ready for the matching engine.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRequest {
    pub coin: String,
    pub is_buy: bool,
    pub sz: f64,
    pub limit_px: f64,
    pub order_type: OrderType,
    pub reduce_only: bool,
}

/// Result of processing one order, mirroring the live `statuses[]` entries.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderOutcome {
    Filled {
        oid: u64,
        total_sz: f64,
        avg_px: f64,
        fee: f64,
    },
    Resting {
        oid: u64,
    },
    Error(String),
}

impl OrderOutcome {
    /// Render as the live-shaped status object.
    pub fn to_status(&self) -> Value {
        match self {
            OrderOutcome::Filled {
                oid,
                total_sz,
                avg_px,
                fee,
            } => serde_json::json!({
                "filled": {
                    "oid": oid,
                    "totalSz": format_qty(*total_sz),
                    "avgPx": format_qty(*avg_px),
                    "fee": format_qty(*fee),
                }
            }),
            OrderOutcome::Resting { oid } => serde_json::json!({"resting": {"oid": oid}}),
            OrderOutcome::Error(msg) => serde_json::json!({"error": msg}),
        }
    }
}

/// Render a number the way Hyperliquid does: a decimal string.
pub fn format_qty(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.0")
    }
}

/// The tradeable perp universe: coin names in asset-index order plus their
/// size-decimal and leverage rules, parsed from a `meta` response.
#[derive(Debug, Clone, Default)]
pub struct Universe {
    coins: Vec<String>,
    sz_decimals: Vec<u32>,
    max_leverage: Vec<f64>,
}

impl Universe {
    /// Parse from a live-shaped `meta` body: `{"universe":[{"name",...}]}`.
    pub fn from_meta(meta: &Value) -> anyhow::Result<Self> {
        let arr = meta
            .get("universe")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("meta body missing `universe` array"))?;
        let mut u = Universe::default();
        for entry in arr {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("universe entry missing `name`"))?;
            u.coins.push(name.to_string());
            u.sz_decimals
                .push(entry.get("szDecimals").and_then(Value::as_u64).unwrap_or(4) as u32);
            u.max_leverage.push(
                entry
                    .get("maxLeverage")
                    .and_then(Value::as_f64)
                    .unwrap_or(50.0),
            );
        }
        Ok(u)
    }

    pub fn coin_by_asset(&self, asset: usize) -> Option<&str> {
        self.coins.get(asset).map(String::as_str)
    }

    pub fn sz_decimals(&self, coin: &str) -> Option<u32> {
        self.index_of(coin).map(|i| self.sz_decimals[i])
    }

    pub fn max_leverage(&self, coin: &str) -> f64 {
        self.index_of(coin).map_or(50.0, |i| self.max_leverage[i])
    }

    fn index_of(&self, coin: &str) -> Option<usize> {
        self.coins.iter().position(|c| c == coin)
    }
}

/// Parse the `orders` array of an `/exchange` order action into requests.
/// Each order resolves independently: a bad entry yields an `Err` message in
/// its slot (so `statuses[]` stays index-aligned), never poisons the batch.
pub fn parse_order_action(
    action: &Value,
    universe: &Universe,
) -> Vec<Result<OrderRequest, String>> {
    let Some(orders) = action.get("orders").and_then(Value::as_array) else {
        return vec![Err("order action missing `orders` array".to_string())];
    };
    orders
        .iter()
        .map(|o| parse_wire_order(o, universe))
        .collect()
}

fn parse_wire_order(o: &Value, universe: &Universe) -> Result<OrderRequest, String> {
    let asset = o
        .get("a")
        .and_then(Value::as_u64)
        .ok_or("order missing asset index `a`")? as usize;
    let coin = universe
        .coin_by_asset(asset)
        .ok_or_else(|| format!("unknown asset index {asset}"))?
        .to_string();
    let is_buy = o
        .get("b")
        .and_then(Value::as_bool)
        .ok_or("order missing `b`")?;
    let limit_px = num_field(o, "p")?;
    let sz = num_field(o, "s")?;
    let reduce_only = o.get("r").and_then(Value::as_bool).unwrap_or(false);

    let t = o.get("t").ok_or("order missing type `t`")?;
    let order_type = if let Some(limit) = t.get("limit") {
        let tif = limit
            .get("tif")
            .and_then(Value::as_str)
            .and_then(Tif::parse)
            .ok_or("limit order missing valid `tif`")?;
        OrderType::Limit { tif }
    } else if let Some(trig) = t.get("trigger") {
        let trigger_px = num_field(trig, "triggerPx")?;
        let is_market = trig
            .get("isMarket")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let kind = match trig.get("tpsl").and_then(Value::as_str) {
            Some("tp") => TriggerKind::TakeProfit,
            _ => TriggerKind::StopLoss,
        };
        OrderType::Trigger {
            trigger_px,
            is_market,
            kind,
        }
    } else {
        return Err("order type `t` must be `limit` or `trigger`".to_string());
    };

    Ok(OrderRequest {
        coin,
        is_buy,
        sz,
        limit_px,
        order_type,
        reduce_only,
    })
}

/// Parse the `cancels` array of a cancel action into `(coin, oid)` pairs.
pub fn parse_cancel_action(
    action: &Value,
    universe: &Universe,
) -> Vec<Result<(String, u64), String>> {
    let Some(cancels) = action.get("cancels").and_then(Value::as_array) else {
        return vec![Err("cancel action missing `cancels` array".to_string())];
    };
    cancels
        .iter()
        .map(|c| {
            let asset = c
                .get("a")
                .and_then(Value::as_u64)
                .ok_or("cancel missing asset index `a`")? as usize;
            let coin = universe
                .coin_by_asset(asset)
                .ok_or_else(|| format!("unknown asset index {asset}"))?
                .to_string();
            let oid = c
                .get("o")
                .and_then(Value::as_u64)
                .ok_or("cancel missing oid `o`")?;
            Ok((coin, oid))
        })
        .collect()
}

/// Numbers arrive as strings on the wire (`"95000.0"`) but accept raw numbers.
fn num_field(v: &Value, key: &str) -> Result<f64, String> {
    let field = v.get(key).ok_or_else(|| format!("missing `{key}`"))?;
    match field {
        Value::String(s) => s
            .parse()
            .map_err(|_| format!("`{key}` is not a number: {s}")),
        Value::Number(n) => n.as_f64().ok_or_else(|| format!("`{key}` is not finite")),
        _ => Err(format!("`{key}` has unexpected type")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn universe() -> Universe {
        Universe::from_meta(&json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ETH", "szDecimals": 4},
        ]}))
        .unwrap()
    }

    #[test]
    fn universe_maps_asset_index_decimals_and_leverage() {
        let u = universe();
        assert_eq!(u.coin_by_asset(0), Some("BTC"));
        assert_eq!(u.coin_by_asset(1), Some("ETH"));
        assert_eq!(u.coin_by_asset(2), None);
        assert_eq!(u.sz_decimals("BTC"), Some(5));
        assert_eq!(u.max_leverage("BTC"), 40.0);
        assert_eq!(u.max_leverage("ETH"), 50.0, "defaults when absent");
    }

    #[test]
    fn parses_an_sdk_ioc_limit_order() {
        let action = json!({"type": "order", "orders": [
            {"a": 0, "b": true, "p": "95000.0", "s": "0.01", "r": false,
             "t": {"limit": {"tif": "Ioc"}}}
        ], "grouping": "na"});
        let parsed = parse_order_action(&action, &universe());
        assert_eq!(parsed.len(), 1);
        let req = parsed[0].as_ref().unwrap();
        assert_eq!(req.coin, "BTC");
        assert!(req.is_buy);
        assert_eq!(req.sz, 0.01);
        assert_eq!(req.limit_px, 95000.0);
        assert_eq!(req.order_type, OrderType::Limit { tif: Tif::Ioc });
        assert!(!req.reduce_only);
    }

    #[test]
    fn parses_a_stop_loss_trigger_order() {
        let action = json!({"type": "order", "orders": [
            {"a": 1, "b": true, "p": "3500", "s": "1.5", "r": true,
             "t": {"trigger": {"isMarket": true, "triggerPx": "3400", "tpsl": "sl"}}}
        ]});
        let req = parse_order_action(&action, &universe())[0].clone().unwrap();
        assert_eq!(req.coin, "ETH");
        assert!(req.reduce_only);
        assert_eq!(
            req.order_type,
            OrderType::Trigger {
                trigger_px: 3400.0,
                is_market: true,
                kind: TriggerKind::StopLoss
            }
        );
    }

    #[test]
    fn bad_entries_fail_in_place_without_poisoning_the_batch() {
        let action = json!({"type": "order", "orders": [
            {"a": 9, "b": true, "p": "1", "s": "1", "t": {"limit": {"tif": "Ioc"}}},
            {"a": 0, "b": false, "p": "94000", "s": "0.5", "t": {"limit": {"tif": "Gtc"}}},
        ]});
        let parsed = parse_order_action(&action, &universe());
        assert!(parsed[0]
            .as_ref()
            .unwrap_err()
            .contains("unknown asset index 9"));
        assert_eq!(parsed[1].as_ref().unwrap().coin, "BTC");
    }

    #[test]
    fn parses_cancel_pairs() {
        let action = json!({"type": "cancel", "cancels": [
            {"a": 0, "o": 1001}, {"a": 1, "o": 1002}
        ]});
        let parsed = parse_cancel_action(&action, &universe());
        assert_eq!(parsed[0].clone().unwrap(), ("BTC".to_string(), 1001));
        assert_eq!(parsed[1].clone().unwrap(), ("ETH".to_string(), 1002));
    }

    #[test]
    fn outcome_renders_live_status_shapes() {
        let filled = OrderOutcome::Filled {
            oid: 7,
            total_sz: 0.01,
            avg_px: 95000.5,
            fee: 0.43,
        };
        let v = filled.to_status();
        assert_eq!(v["filled"]["oid"], 7);
        assert_eq!(v["filled"]["totalSz"], "0.01");
        assert_eq!(v["filled"]["avgPx"], "95000.5");
        assert_eq!(v["filled"]["fee"], "0.43");
        assert_eq!(
            OrderOutcome::Resting { oid: 9 }.to_status()["resting"]["oid"],
            9
        );
        assert_eq!(
            OrderOutcome::Error("nope".into()).to_status()["error"],
            "nope"
        );
    }

    #[test]
    fn numeric_fields_accept_strings_and_numbers() {
        let action = json!({"orders": [
            {"a": 0, "b": true, "p": 95000, "s": 0.25, "t": {"limit": {"tif": "Gtc"}}}
        ]});
        let req = parse_order_action(&action, &universe())[0].clone().unwrap();
        assert_eq!(req.limit_px, 95000.0);
        assert_eq!(req.sz, 0.25);
    }
}
