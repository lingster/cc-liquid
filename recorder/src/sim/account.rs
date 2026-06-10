//! Virtual account (PRD §7.2): balance, positions, fills and fees, plus the
//! live-shaped `clearinghouseState` / `userFills` / `userFees` projections
//! that cc-liquid's `get_portfolio_info()` and PnL aggregation consume.
//!
//! Position accounting is standard average-entry: adding to a position
//! re-averages the entry price; reducing realizes PnL on the closed portion;
//! crossing through zero realizes the whole old side and opens the remainder
//! at the fill price. Balance moves only on realized PnL and fees.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::sim::order::{format_qty, Universe};

/// One open position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// Signed size (positive = long, negative = short).
    pub szi: f64,
    pub entry_px: f64,
}

/// One executed fill, in the vocabulary of `userFills`.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub coin: String,
    pub is_buy: bool,
    pub px: f64,
    pub sz: f64,
    pub fee: f64,
    pub closed_pnl: f64,
    pub time_ms: i64,
    pub oid: u64,
    pub dir: String,
    /// Signed position size *before* this fill.
    pub start_position: f64,
}

/// Virtual balance, positions and fill history.
#[derive(Debug, Clone)]
pub struct VirtualAccount {
    balance: f64,
    taker_rate: f64,
    maker_rate: f64,
    positions: BTreeMap<String, Position>,
    fills: Vec<Fill>,
}

impl VirtualAccount {
    pub fn new(start_balance: f64, taker_rate: f64, maker_rate: f64) -> Self {
        Self {
            balance: start_balance,
            taker_rate,
            maker_rate,
            positions: BTreeMap::new(),
            fills: Vec::new(),
        }
    }

    pub fn taker_rate(&self) -> f64 {
        self.taker_rate
    }

    pub fn maker_rate(&self) -> f64 {
        self.maker_rate
    }

    /// Signed position size for a coin (0 when flat).
    pub fn position_szi(&self, coin: &str) -> f64 {
        self.positions.get(coin).map_or(0.0, |p| p.szi)
    }

    /// Apply an executed fill: update the position, realize PnL on any closed
    /// portion, charge the fee, and record the fill. Returns the realized PnL.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        px: f64,
        fee: f64,
        time_ms: i64,
        oid: u64,
    ) -> f64 {
        let old = self.positions.get(coin).copied().unwrap_or(Position {
            szi: 0.0,
            entry_px: 0.0,
        });
        let delta = if is_buy { sz } else { -sz };
        let new_szi = old.szi + delta;

        let mut closed_pnl = 0.0;
        let new_pos = if old.szi == 0.0 || old.szi.signum() == delta.signum() {
            // Opening or adding: size-weighted entry.
            let entry = (old.szi.abs() * old.entry_px + delta.abs() * px) / new_szi.abs();
            Position {
                szi: new_szi,
                entry_px: entry,
            }
        } else {
            // Reducing/flipping: realize PnL on the closed portion.
            let closed = delta.abs().min(old.szi.abs());
            closed_pnl = (px - old.entry_px) * closed * old.szi.signum();
            if new_szi == 0.0 {
                Position {
                    szi: 0.0,
                    entry_px: 0.0,
                }
            } else if new_szi.signum() == old.szi.signum() {
                Position {
                    szi: new_szi,
                    entry_px: old.entry_px,
                }
            } else {
                // Flipped through zero: remainder opens at the fill price.
                Position {
                    szi: new_szi,
                    entry_px: px,
                }
            }
        };
        if new_pos.szi == 0.0 {
            self.positions.remove(coin);
        } else {
            self.positions.insert(coin.to_string(), new_pos);
        }

        self.balance += closed_pnl - fee;
        self.fills.push(Fill {
            coin: coin.to_string(),
            is_buy,
            px,
            sz,
            fee,
            closed_pnl,
            time_ms,
            oid,
            dir: direction_label(old.szi, delta),
            start_position: old.szi,
        });
        closed_pnl
    }

    /// Live-shaped `clearinghouseState`. `mark` resolves the current mark
    /// price per coin (positions with no mark fall back to entry).
    pub fn user_state_json(
        &self,
        mark: impl Fn(&str) -> Option<f64>,
        universe: &Universe,
        time_ms: i64,
    ) -> Value {
        let mut unrealized_total = 0.0;
        let mut ntl_total = 0.0;
        let mut margin_total = 0.0;
        let mut asset_positions = Vec::new();
        for (coin, pos) in &self.positions {
            let mark_px = mark(coin).unwrap_or(pos.entry_px);
            let ntl = pos.szi.abs() * mark_px;
            let unrealized = (mark_px - pos.entry_px) * pos.szi;
            let lev = universe.max_leverage(coin);
            let margin = ntl / lev;
            unrealized_total += unrealized;
            ntl_total += ntl;
            margin_total += margin;
            let roe = if margin > 0.0 {
                unrealized / margin
            } else {
                0.0
            };
            asset_positions.push(json!({
                "type": "oneWay",
                "position": {
                    "coin": coin,
                    "szi": format_qty(pos.szi),
                    "entryPx": format_qty(pos.entry_px),
                    "positionValue": format_qty(ntl),
                    "unrealizedPnl": format_qty(unrealized),
                    "returnOnEquity": format_qty(roe),
                    "liquidationPx": Value::Null,
                    "marginUsed": format_qty(margin),
                    "maxLeverage": lev,
                    "leverage": {"type": "cross", "value": lev},
                }
            }));
        }
        let account_value = self.balance + unrealized_total;
        let withdrawable = (account_value - margin_total).max(0.0);
        let summary = json!({
            "accountValue": format_qty(account_value),
            "totalNtlPos": format_qty(ntl_total),
            "totalMarginUsed": format_qty(margin_total),
            "totalRawUsd": format_qty(self.balance),
        });
        json!({
            "marginSummary": summary,
            "crossMarginSummary": summary,
            "crossMaintenanceMarginUsed": "0.0",
            "assetPositions": asset_positions,
            "withdrawable": format_qty(withdrawable),
            "time": time_ms,
        })
    }

    /// Live-shaped `userFills` (most recent first, like the real API).
    pub fn user_fills_json(&self) -> Value {
        let entries: Vec<Value> = self
            .fills
            .iter()
            .rev()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "coin": f.coin,
                    "px": format_qty(f.px),
                    "sz": format_qty(f.sz),
                    "side": if f.is_buy { "B" } else { "A" },
                    "time": f.time_ms,
                    "startPosition": format_qty(f.start_position),
                    "dir": f.dir,
                    "closedPnl": format_qty(f.closed_pnl),
                    "hash": "0x0",
                    "oid": f.oid,
                    "crossed": true,
                    "fee": format_qty(f.fee),
                    "feeToken": "USDC",
                    "tid": i as u64,
                })
            })
            .collect();
        Value::Array(entries)
    }

    /// Live-shaped `userFees`.
    pub fn user_fees_json(&self) -> Value {
        json!({
            "userCrossRate": format_qty(self.taker_rate),
            "userAddRate": format_qty(self.maker_rate),
            "feeSchedule": {},
        })
    }

    pub fn fills(&self) -> &[Fill] {
        &self.fills
    }

    pub fn balance(&self) -> f64 {
        self.balance
    }
}

/// Hyperliquid-style direction label for a fill.
fn direction_label(old_szi: f64, delta: f64) -> String {
    let buying = delta > 0.0;
    match (old_szi == 0.0, old_szi > 0.0, buying) {
        (true, _, true) => "Open Long".to_string(),
        (true, _, false) => "Open Short".to_string(),
        (false, true, true) => "Open Long".to_string(),
        (false, true, false) => "Close Long".to_string(),
        (false, false, false) => "Open Short".to_string(),
        (false, false, true) => "Close Short".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn universe() -> Universe {
        Universe::from_meta(&json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 50},
        ]}))
        .unwrap()
    }

    fn account() -> VirtualAccount {
        VirtualAccount::new(10_000.0, 0.00045, 0.00015)
    }

    #[test]
    fn opening_and_adding_averages_the_entry() {
        let mut a = account();
        a.apply_fill("BTC", true, 1.0, 100.0, 0.0, 1, 1);
        a.apply_fill("BTC", true, 1.0, 110.0, 0.0, 2, 2);
        assert_eq!(a.position_szi("BTC"), 2.0);
        let pos = a.positions.get("BTC").unwrap();
        assert!((pos.entry_px - 105.0).abs() < 1e-9);
    }

    #[test]
    fn reducing_realizes_pnl_into_the_balance() {
        let mut a = account();
        a.apply_fill("BTC", true, 2.0, 100.0, 0.0, 1, 1);
        let pnl = a.apply_fill("BTC", false, 1.0, 110.0, 0.0, 2, 2);
        assert!((pnl - 10.0).abs() < 1e-9, "closed 1 @ +10");
        assert!((a.balance() - 10_010.0).abs() < 1e-9);
        assert_eq!(a.position_szi("BTC"), 1.0);
        // Entry of the remainder is unchanged.
        assert_eq!(a.positions.get("BTC").unwrap().entry_px, 100.0);
    }

    #[test]
    fn flipping_through_zero_realizes_and_reopens_at_fill_price() {
        let mut a = account();
        a.apply_fill("BTC", false, 1.0, 100.0, 0.0, 1, 1); // short 1
        let pnl = a.apply_fill("BTC", true, 3.0, 90.0, 0.0, 2, 2); // buy 3 → long 2
        assert!((pnl - 10.0).abs() < 1e-9, "short closed 10 below entry");
        let pos = a.positions.get("BTC").unwrap();
        assert_eq!(pos.szi, 2.0);
        assert_eq!(pos.entry_px, 90.0);
    }

    #[test]
    fn fees_reduce_balance_and_short_pnl_is_signed_correctly() {
        let mut a = account();
        a.apply_fill("BTC", false, 1.0, 100.0, 0.5, 1, 1);
        let pnl = a.apply_fill("BTC", true, 1.0, 105.0, 0.5, 2, 2);
        assert!((pnl + 5.0).abs() < 1e-9, "short lost 5");
        assert!((a.balance() - (10_000.0 - 5.0 - 1.0)).abs() < 1e-9);
        assert_eq!(a.position_szi("BTC"), 0.0);
    }

    #[test]
    fn user_state_recomputes_margin_summary_from_marks() {
        let mut a = account();
        a.apply_fill("BTC", true, 2.0, 100.0, 0.0, 1, 1);
        let state = a.user_state_json(|_| Some(110.0), &universe(), 999);
        // accountValue = 10000 + 2*(110-100) = 10020
        assert_eq!(state["marginSummary"]["accountValue"], "10020.0");
        assert_eq!(state["marginSummary"]["totalNtlPos"], "220.0");
        // margin = 220 / 50x
        assert_eq!(state["marginSummary"]["totalMarginUsed"], "4.4");
        assert_eq!(state["withdrawable"], "10015.6");
        let pos = &state["assetPositions"][0]["position"];
        assert_eq!(pos["coin"], "BTC");
        assert_eq!(pos["szi"], "2.0");
        assert_eq!(pos["entryPx"], "100.0");
        assert_eq!(pos["unrealizedPnl"], "20.0");
        assert!(pos["liquidationPx"].is_null());
        assert_eq!(state["time"], 999);
    }

    #[test]
    fn flat_account_has_empty_positions_and_full_withdrawable() {
        let a = account();
        let state = a.user_state_json(|_| None, &universe(), 0);
        assert_eq!(state["assetPositions"], json!([]));
        assert_eq!(state["marginSummary"]["accountValue"], "10000.0");
        assert_eq!(state["withdrawable"], "10000.0");
    }

    #[test]
    fn fills_render_most_recent_first_with_live_fields() {
        let mut a = account();
        a.apply_fill("BTC", true, 1.0, 100.0, 0.1, 10, 1);
        a.apply_fill("BTC", false, 1.0, 105.0, 0.1, 20, 2);
        let fills = a.user_fills_json();
        assert_eq!(fills[0]["time"], 20);
        assert_eq!(fills[0]["dir"], "Close Long");
        assert_eq!(fills[0]["closedPnl"], "5.0");
        assert_eq!(fills[0]["side"], "A");
        assert_eq!(fills[1]["dir"], "Open Long");
        assert_eq!(fills[1]["startPosition"], "0.0");
    }

    #[test]
    fn user_fees_exposes_configured_rates() {
        let fees = account().user_fees_json();
        assert_eq!(fees["userCrossRate"], "0.00045");
        assert_eq!(fees["userAddRate"], "0.00015");
    }
}
