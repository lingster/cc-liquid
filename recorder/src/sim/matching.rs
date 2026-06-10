//! Matching rules (PRD §7.1): aggressive orders walk the replayed L2 book
//! level-by-level (partial fills, size-weighted average price); resting
//! orders fill when later ticks cross their level, subject to a configurable
//! queue model (§7.1.2).
//!
//! Sessions without L2 data (mids-only recordings) degrade to mid-price
//! matching with unlimited liquidity — documented, deterministic, and exactly
//! what the 5-minute demo needs. The recorded book is exogenous: our fills
//! consume recorded liquidity for price calculation but do not alter the
//! future stream (PRD §7.1 self-impact assumption).

use std::str::FromStr;

use crate::events::Trade;
use crate::replay::state::BookState;
use crate::sim::order::TriggerKind;

/// How a resting order's place in the queue is modelled (PRD §7.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueModel {
    /// Fills only after the recorded size ahead at the level is consumed by
    /// trades, or the price trades strictly through the level.
    #[default]
    Conservative,
    /// Fills as soon as the replayed price touches the level.
    Optimistic,
    /// Resting orders are rejected (aggressive-only twin).
    Disabled,
}

impl FromStr for QueueModel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "conservative" => Ok(QueueModel::Conservative),
            "optimistic" => Ok(QueueModel::Optimistic),
            "disabled" => Ok(QueueModel::Disabled),
            other => Err(format!(
                "unknown queue model `{other}` (use conservative|optimistic|disabled)"
            )),
        }
    }
}

/// Result of matching an aggressive order.
#[derive(Debug, Clone, PartialEq)]
pub struct FillResult {
    /// Size actually filled (may be less than requested — partial fill).
    pub filled_sz: f64,
    /// Size-weighted average price across consumed levels.
    pub avg_px: f64,
    /// Worst (deepest) consumed level — the `worst_case` overlay substrate.
    pub worst_px: f64,
}

/// Match an aggressive (IOC-like) order against the book at the cursor,
/// falling back to the mid when the session has no L2 for the coin.
/// Returns `None` when nothing crosses.
pub fn match_aggressive(
    book: Option<&BookState>,
    mid: Option<f64>,
    is_buy: bool,
    sz: f64,
    limit_px: f64,
) -> Option<FillResult> {
    if let Some(book) = book {
        let opposing = if is_buy { &book.asks } else { &book.bids };
        if !opposing.is_empty() {
            return walk_levels(opposing, is_buy, sz, limit_px);
        }
    }
    // Mids-only session: unlimited synthetic liquidity at the mid.
    let mid = mid?;
    let crosses = if is_buy {
        limit_px >= mid
    } else {
        limit_px <= mid
    };
    crosses.then_some(FillResult {
        filled_sz: sz,
        avg_px: mid,
        worst_px: mid,
    })
}

/// Walk opposing levels best-first, consuming size while the limit allows.
fn walk_levels(
    levels: &[crate::events::Level],
    is_buy: bool,
    sz: f64,
    limit_px: f64,
) -> Option<FillResult> {
    let mut remaining = sz;
    let mut notional = 0.0;
    let mut worst_px = 0.0;
    for level in levels {
        let crosses = if is_buy {
            level.px <= limit_px
        } else {
            level.px >= limit_px
        };
        if !crosses || remaining <= 0.0 {
            break;
        }
        let take = remaining.min(level.sz);
        notional += take * level.px;
        worst_px = level.px;
        remaining -= take;
    }
    let filled = sz - remaining;
    (filled > 0.0).then(|| FillResult {
        filled_sz: filled,
        avg_px: notional / filled,
        worst_px,
    })
}

/// Would an order at `limit_px` cross the current market? (Alo rejection and
/// Gtc immediate-fill checks.)
pub fn would_cross(
    book: Option<&BookState>,
    mid: Option<f64>,
    is_buy: bool,
    limit_px: f64,
) -> bool {
    let best_opposing = book
        .and_then(|b| if is_buy { b.best_ask() } else { b.best_bid() })
        .or(mid);
    match best_opposing {
        Some(px) => {
            if is_buy {
                limit_px >= px
            } else {
                limit_px <= px
            }
        }
        None => false,
    }
}

/// Recorded size resting at the order's own price level when it is placed —
/// the queue ahead of a conservative resting order.
pub fn queue_ahead(book: Option<&BookState>, is_buy: bool, limit_px: f64) -> f64 {
    let Some(book) = book else { return 0.0 };
    let own_side = if is_buy { &book.bids } else { &book.asks };
    own_side
        .iter()
        .find(|l| l.px == limit_px)
        .map_or(0.0, |l| l.sz)
}

/// Evaluate one replay tick for a resting order. `queue_remaining` is the
/// order's mutable queue-ahead state (conservative model only). Returns
/// `true` when the order fills (at its limit price, as maker).
pub fn resting_fills(
    model: QueueModel,
    is_buy: bool,
    limit_px: f64,
    queue_remaining: &mut f64,
    book: Option<&BookState>,
    mid: Option<f64>,
    trades: &[&Trade],
) -> bool {
    // "Strictly through": the market traded/quoted beyond our level, so any
    // queue at the level was fully consumed.
    let through = price_reached(book, mid, trades, is_buy, limit_px, true);
    match model {
        QueueModel::Disabled => false,
        QueueModel::Optimistic => {
            through || price_reached(book, mid, trades, is_buy, limit_px, false)
        }
        QueueModel::Conservative => {
            if through {
                return true;
            }
            // Trades printing exactly at our level consume the queue ahead.
            let consumed: f64 = trades
                .iter()
                .filter(|t| t.px == limit_px)
                .map(|t| t.sz)
                .sum();
            if consumed > 0.0 {
                *queue_remaining -= consumed;
                return *queue_remaining <= 0.0;
            }
            false
        }
    }
}

/// Did the replayed market reach our level? `strict` requires trading/quoting
/// beyond it; otherwise touching is enough.
fn price_reached(
    book: Option<&BookState>,
    mid: Option<f64>,
    trades: &[&Trade],
    is_buy: bool,
    limit_px: f64,
    strict: bool,
) -> bool {
    let beats = |px: f64| {
        if is_buy {
            if strict {
                px < limit_px
            } else {
                px <= limit_px
            }
        } else if strict {
            px > limit_px
        } else {
            px >= limit_px
        }
    };
    // Opposing best quote crossing our level means we'd be matched.
    let quote = book
        .and_then(|b| if is_buy { b.best_ask() } else { b.best_bid() })
        .or(mid);
    quote.is_some_and(beats) || trades.iter().any(|t| beats(t.px))
}

/// Has a trigger order activated at the given mark price? (PRD §7.1: stop
/// orders fire when the mark crosses `trigger_px` toward the loss side;
/// take-profits the other way.)
pub fn trigger_activated(kind: TriggerKind, is_buy: bool, trigger_px: f64, mark: f64) -> bool {
    match (kind, is_buy) {
        // Stop-loss sell protects a long: fire when the mark falls to the stop.
        (TriggerKind::StopLoss, false) => mark <= trigger_px,
        // Stop-loss buy protects a short: fire when the mark rises to the stop.
        (TriggerKind::StopLoss, true) => mark >= trigger_px,
        (TriggerKind::TakeProfit, false) => mark >= trigger_px,
        (TriggerKind::TakeProfit, true) => mark <= trigger_px,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Level, Side};

    fn level(px: f64, sz: f64) -> Level {
        Level { px, sz, n: 1 }
    }

    fn book(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> BookState {
        BookState {
            bids: bids.iter().map(|&(px, sz)| level(px, sz)).collect(),
            asks: asks.iter().map(|&(px, sz)| level(px, sz)).collect(),
            time_ms: 0,
        }
    }

    fn trade(px: f64, sz: f64) -> Trade {
        Trade {
            coin: "BTC".into(),
            side: Side::Sell,
            px,
            sz,
            time_ms: 0,
        }
    }

    #[test]
    fn buy_walks_asks_with_size_weighted_average() {
        let b = book(&[(99.0, 5.0)], &[(100.0, 1.0), (101.0, 2.0), (102.0, 10.0)]);
        let fill = match_aggressive(Some(&b), None, true, 2.0, 101.5).unwrap();
        assert_eq!(fill.filled_sz, 2.0);
        // 1 @ 100 + 1 @ 101 = avg 100.5
        assert!((fill.avg_px - 100.5).abs() < 1e-9);
        assert_eq!(fill.worst_px, 101.0);
    }

    #[test]
    fn limit_price_caps_the_walk_yielding_partial_fill() {
        let b = book(&[], &[(100.0, 1.0), (105.0, 5.0)]);
        let fill = match_aggressive(Some(&b), None, true, 3.0, 100.0).unwrap();
        assert_eq!(fill.filled_sz, 1.0, "second level is above the limit");
        assert_eq!(fill.avg_px, 100.0);
    }

    #[test]
    fn liquidity_exhaustion_yields_partial_fill() {
        let b = book(&[(100.0, 0.5), (99.0, 0.25)], &[]);
        let fill = match_aggressive(Some(&b), None, false, 2.0, 98.0).unwrap();
        assert_eq!(fill.filled_sz, 0.75);
        assert_eq!(fill.worst_px, 99.0);
    }

    #[test]
    fn non_crossing_order_does_not_fill() {
        let b = book(&[(99.0, 1.0)], &[(101.0, 1.0)]);
        assert!(match_aggressive(Some(&b), None, true, 1.0, 100.0).is_none());
        assert!(match_aggressive(Some(&b), None, false, 1.0, 102.0).is_none());
    }

    #[test]
    fn mids_only_session_fills_fully_at_mid_when_crossing() {
        let fill = match_aggressive(None, Some(95000.0), true, 0.5, 95500.0).unwrap();
        assert_eq!(fill.filled_sz, 0.5);
        assert_eq!(fill.avg_px, 95000.0);
        assert!(match_aggressive(None, Some(95000.0), true, 0.5, 94000.0).is_none());
        assert!(match_aggressive(None, None, true, 0.5, 94000.0).is_none());
    }

    #[test]
    fn would_cross_checks_best_opposing_or_mid() {
        let b = book(&[(99.0, 1.0)], &[(101.0, 1.0)]);
        assert!(would_cross(Some(&b), None, true, 101.0));
        assert!(!would_cross(Some(&b), None, true, 100.0));
        assert!(would_cross(None, Some(100.0), false, 100.0));
    }

    #[test]
    fn queue_ahead_reads_recorded_size_at_own_level() {
        let b = book(&[(99.0, 3.5)], &[]);
        assert_eq!(queue_ahead(Some(&b), true, 99.0), 3.5);
        assert_eq!(queue_ahead(Some(&b), true, 98.0), 0.0);
        assert_eq!(queue_ahead(None, true, 99.0), 0.0);
    }

    #[test]
    fn optimistic_resting_fills_on_touch() {
        let mut q = 5.0;
        // Resting buy at 99; a trade prints at 99 → touch.
        let t = trade(99.0, 0.1);
        assert!(resting_fills(
            QueueModel::Optimistic,
            true,
            99.0,
            &mut q,
            None,
            None,
            &[&t]
        ));
        // Mid touching also fills.
        assert!(resting_fills(
            QueueModel::Optimistic,
            true,
            99.0,
            &mut q,
            None,
            Some(99.0),
            &[]
        ));
        assert!(!resting_fills(
            QueueModel::Optimistic,
            true,
            99.0,
            &mut q,
            None,
            Some(99.5),
            &[]
        ));
    }

    #[test]
    fn conservative_resting_needs_queue_consumed_or_price_through() {
        let mut q = 1.0;
        let at_level = trade(99.0, 0.6);
        // First trade at the level only eats part of the queue.
        assert!(!resting_fills(
            QueueModel::Conservative,
            true,
            99.0,
            &mut q,
            None,
            None,
            &[&at_level]
        ));
        assert!((q - 0.4).abs() < 1e-9);
        // Second trade exhausts it → fill.
        assert!(resting_fills(
            QueueModel::Conservative,
            true,
            99.0,
            &mut q,
            None,
            None,
            &[&at_level]
        ));
        // Price trading strictly through fills regardless of queue.
        let mut q2 = 100.0;
        let through = trade(98.5, 0.01);
        assert!(resting_fills(
            QueueModel::Conservative,
            true,
            99.0,
            &mut q2,
            None,
            None,
            &[&through]
        ));
        // Mid crossing strictly through also fills (mids-only sessions).
        let mut q3 = 100.0;
        assert!(resting_fills(
            QueueModel::Conservative,
            true,
            99.0,
            &mut q3,
            None,
            Some(98.9),
            &[]
        ));
        assert!(!resting_fills(
            QueueModel::Conservative,
            true,
            99.0,
            &mut q3,
            None,
            Some(99.0),
            &[]
        ));
    }

    #[test]
    fn disabled_never_fills() {
        let mut q = 0.0;
        assert!(!resting_fills(
            QueueModel::Disabled,
            true,
            99.0,
            &mut q,
            None,
            Some(90.0),
            &[]
        ));
    }

    #[test]
    fn trigger_activation_directions() {
        use TriggerKind::*;
        // SL sell (protecting a long) fires when mark drops to the stop.
        assert!(trigger_activated(StopLoss, false, 90.0, 89.0));
        assert!(!trigger_activated(StopLoss, false, 90.0, 91.0));
        // SL buy (protecting a short) fires when mark rises to the stop.
        assert!(trigger_activated(StopLoss, true, 110.0, 111.0));
        assert!(!trigger_activated(StopLoss, true, 110.0, 109.0));
        // TP mirror image.
        assert!(trigger_activated(TakeProfit, false, 110.0, 111.0));
        assert!(trigger_activated(TakeProfit, true, 90.0, 89.0));
    }

    #[test]
    fn queue_model_parses() {
        assert_eq!(
            "conservative".parse::<QueueModel>().unwrap(),
            QueueModel::Conservative
        );
        assert_eq!(
            "OPTIMISTIC".parse::<QueueModel>().unwrap(),
            QueueModel::Optimistic
        );
        assert!("fifo".parse::<QueueModel>().is_err());
    }
}
