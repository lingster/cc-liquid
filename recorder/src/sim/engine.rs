//! The simulation engine (PRD §7): validates incoming wire orders, matches
//! them against the replayed market, maintains the virtual account, and
//! manages resting/trigger orders across replay ticks.
//!
//! One instance per twin session. The engine never sees transport concerns —
//! the proxy hands it parsed `/exchange` action bodies and the current
//! [`MarketState`], and asks for live-shaped JSON back.

use serde_json::{json, Value};

use crate::events::Trade;
use crate::replay::state::MarketState;
use crate::sim::account::VirtualAccount;
use crate::sim::matching::{
    match_aggressive, queue_ahead, resting_fills, trigger_activated, would_cross, QueueModel,
};
use crate::sim::order::{
    format_qty, parse_cancel_action, parse_order_action, OrderOutcome, OrderRequest, OrderType,
    Tif, TriggerKind, Universe,
};
use crate::sim::overlay::{FillOverlay, Xorshift64};

/// Tunables for a simulated session (PRD §7.1.1–§7.2, §11).
#[derive(Debug, Clone)]
pub struct SimConfig {
    pub start_balance: f64,
    pub taker_rate: f64,
    pub maker_rate: f64,
    pub overlay: FillOverlay,
    pub queue: QueueModel,
    pub min_notional: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            start_balance: 10_000.0,
            // Hyperliquid base tier.
            taker_rate: 0.00045,
            maker_rate: 0.00015,
            overlay: FillOverlay::Book,
            queue: QueueModel::Conservative,
            min_notional: 10.0,
        }
    }
}

/// An order resting in the virtual book (limit) or pending activation (trigger).
#[derive(Debug, Clone)]
struct OpenOrder {
    oid: u64,
    req: OrderRequest,
    /// Conservative-queue state for resting limits (unused for triggers).
    queue_remaining: f64,
    time_ms: i64,
}

/// Matching engine + virtual account for one twin session.
pub struct SimEngine {
    cfg: SimConfig,
    universe: Universe,
    account: VirtualAccount,
    rng: Xorshift64,
    next_oid: u64,
    open: Vec<OpenOrder>,
    /// Set to `false` by end-of-window `stop`: new orders are rejected.
    accepting: bool,
}

impl SimEngine {
    pub fn new(cfg: SimConfig, universe: Universe) -> Self {
        let account = VirtualAccount::new(cfg.start_balance, cfg.taker_rate, cfg.maker_rate);
        let rng = Xorshift64::new(cfg.overlay.seed());
        Self {
            cfg,
            universe,
            account,
            rng,
            next_oid: 1000,
            open: Vec::new(),
            accepting: true,
        }
    }

    /// End-of-window control: when `false`, `/exchange` orders are rejected
    /// (PRD §6 `stop`); cancels and reads continue to work.
    pub fn set_accepting(&mut self, accepting: bool) {
        self.accepting = accepting;
    }

    /// Handle a full `/exchange` action body, returning the live-shaped
    /// response (`{"status":"ok","response":{...}}`).
    pub fn handle_action(&mut self, action: &Value, state: &MarketState, now_ms: i64) -> Value {
        match action.get("type").and_then(Value::as_str) {
            Some("order") => {
                let statuses: Vec<Value> = parse_order_action(action, &self.universe)
                    .into_iter()
                    .map(|parsed| match parsed {
                        Ok(req) => self.place(req, state, now_ms).to_status(),
                        Err(msg) => json!({"error": msg}),
                    })
                    .collect();
                json!({"status": "ok", "response": {"type": "order", "data": {"statuses": statuses}}})
            }
            Some("cancel") => {
                let statuses: Vec<Value> = parse_cancel_action(action, &self.universe)
                    .into_iter()
                    .map(|parsed| match parsed {
                        Ok((coin, oid)) => match self.cancel(&coin, oid) {
                            Ok(()) => json!("success"),
                            Err(msg) => json!({"error": msg}),
                        },
                        Err(msg) => json!({"error": msg}),
                    })
                    .collect();
                json!({"status": "ok", "response": {"type": "cancel", "data": {"statuses": statuses}}})
            }
            other => json!({
                "status": "err",
                "response": format!("hl-proxy sim: unsupported action type {other:?}"),
            }),
        }
    }

    /// Place one validated order against the market at the cursor.
    pub fn place(
        &mut self,
        mut req: OrderRequest,
        state: &MarketState,
        now_ms: i64,
    ) -> OrderOutcome {
        if !self.accepting {
            return OrderOutcome::Error(
                "session window ended (end_of_window=stop): order rejected".to_string(),
            );
        }
        if req.sz <= 0.0 {
            return OrderOutcome::Error("Order has zero size".to_string());
        }
        let ref_px = match req.order_type {
            OrderType::Trigger { trigger_px, .. } => trigger_px,
            OrderType::Limit { .. } => req.limit_px,
        };
        if req.sz * ref_px < self.cfg.min_notional {
            return OrderOutcome::Error(format!(
                "Order must have minimum value of ${}",
                self.cfg.min_notional
            ));
        }
        if req.reduce_only {
            let pos = self.account.position_szi(&req.coin);
            let delta = if req.is_buy { req.sz } else { -req.sz };
            if pos == 0.0 || pos.signum() == delta.signum() {
                return OrderOutcome::Error(
                    "Reduce only order would increase position".to_string(),
                );
            }
            // Live auto-resizes reduce-only orders down to the position.
            req.sz = req.sz.min(pos.abs());
        }

        match req.order_type {
            OrderType::Trigger { .. } => {
                Self::rest(&mut self.open, &mut self.next_oid, req, 0.0, now_ms)
            }
            OrderType::Limit { tif } => self.place_limit(req, tif, state, now_ms),
        }
    }

    fn place_limit(
        &mut self,
        req: OrderRequest,
        tif: Tif,
        state: &MarketState,
        now_ms: i64,
    ) -> OrderOutcome {
        let book = state.book(&req.coin);
        let mid = state.price(&req.coin);
        let crossing = would_cross(book, mid, req.is_buy, req.limit_px);

        if tif == Tif::Alo {
            if crossing {
                return OrderOutcome::Error(
                    "Post only order would have immediately matched, bbo was crossed".to_string(),
                );
            }
            return self.rest_limit(req, state, now_ms);
        }

        let matched = match_aggressive(book, mid, req.is_buy, req.sz, req.limit_px);
        match (matched, tif) {
            (Some(fill), _) => {
                let oid = self.alloc_oid();
                let px =
                    self.cfg
                        .overlay
                        .adjust(fill.avg_px, fill.worst_px, req.is_buy, &mut self.rng);
                let fee = px * fill.filled_sz * self.cfg.taker_rate;
                self.account.apply_fill(
                    &req.coin,
                    req.is_buy,
                    fill.filled_sz,
                    px,
                    fee,
                    now_ms,
                    oid,
                );
                // Gtc: any unfilled remainder rests under the same oid.
                let remainder = req.sz - fill.filled_sz;
                if tif == Tif::Gtc && remainder > 0.0 {
                    let mut rest_req = req.clone();
                    rest_req.sz = remainder;
                    let queue = queue_ahead(book, rest_req.is_buy, rest_req.limit_px);
                    self.open.push(OpenOrder {
                        oid,
                        req: rest_req,
                        queue_remaining: queue,
                        time_ms: now_ms,
                    });
                }
                OrderOutcome::Filled {
                    oid,
                    total_sz: fill.filled_sz,
                    avg_px: px,
                    fee,
                }
            }
            (None, Tif::Ioc) => OrderOutcome::Error(
                "Order could not immediately match against any resting orders".to_string(),
            ),
            (None, _) => self.rest_limit(req, state, now_ms),
        }
    }

    fn rest_limit(&mut self, req: OrderRequest, state: &MarketState, now_ms: i64) -> OrderOutcome {
        if self.cfg.queue == QueueModel::Disabled {
            return OrderOutcome::Error(
                "resting orders disabled (queue_model=disabled)".to_string(),
            );
        }
        let queue = queue_ahead(state.book(&req.coin), req.is_buy, req.limit_px);
        Self::rest(&mut self.open, &mut self.next_oid, req, queue, now_ms)
    }

    fn rest(
        open: &mut Vec<OpenOrder>,
        next_oid: &mut u64,
        req: OrderRequest,
        queue_remaining: f64,
        time_ms: i64,
    ) -> OrderOutcome {
        let oid = *next_oid;
        *next_oid += 1;
        open.push(OpenOrder {
            oid,
            req,
            queue_remaining,
            time_ms,
        });
        OrderOutcome::Resting { oid }
    }

    fn alloc_oid(&mut self) -> u64 {
        let oid = self.next_oid;
        self.next_oid += 1;
        oid
    }

    /// Cancel an open order by `(coin, oid)`.
    pub fn cancel(&mut self, coin: &str, oid: u64) -> Result<(), String> {
        let before = self.open.len();
        self.open.retain(|o| !(o.oid == oid && o.req.coin == coin));
        if self.open.len() < before {
            Ok(())
        } else {
            Err("Order was never placed, already canceled, or filled.".to_string())
        }
    }

    /// Advance the simulation by one replay tick: activate crossed triggers
    /// and fill resting orders whose level the market reached. `trades` are
    /// the public trades folded during this tick.
    pub fn on_tick(&mut self, state: &MarketState, trades: &[Trade], now_ms: i64) {
        // Take the list so the closure can borrow `self` mutably for fills.
        let mut pending = std::mem::take(&mut self.open);
        pending.retain_mut(|order| {
            let coin_trades: Vec<&Trade> =
                trades.iter().filter(|t| t.coin == order.req.coin).collect();
            let done = match order.req.order_type {
                OrderType::Trigger {
                    trigger_px,
                    is_market,
                    kind,
                } => self.try_fire_trigger(order, trigger_px, is_market, kind, state, now_ms),
                OrderType::Limit { .. } => {
                    self.try_fill_resting(order, state, &coin_trades, now_ms)
                }
            };
            !done
        });
        self.open = pending;
    }

    fn try_fire_trigger(
        &mut self,
        order: &OpenOrder,
        trigger_px: f64,
        _is_market: bool,
        kind: TriggerKind,
        state: &MarketState,
        now_ms: i64,
    ) -> bool {
        let Some(mark) = state.price(&order.req.coin) else {
            return false;
        };
        if !trigger_activated(kind, order.req.is_buy, trigger_px, mark) {
            return false;
        }
        // Reduce-only triggers clamp to the live position at activation time.
        let mut sz = order.req.sz;
        if order.req.reduce_only {
            let pos = self.account.position_szi(&order.req.coin);
            let delta_sign = if order.req.is_buy { 1.0 } else { -1.0 };
            if pos == 0.0 || pos.signum() == delta_sign {
                return true; // nothing left to reduce: drop the trigger
            }
            sz = sz.min(pos.abs());
        }
        // Both market and limit triggers execute capped at the order's limit
        // price (cc-liquid pre-computes the slippage-adjusted limit).
        let fill = match_aggressive(
            state.book(&order.req.coin),
            Some(mark),
            order.req.is_buy,
            sz,
            order.req.limit_px,
        );
        match fill {
            Some(fill) => {
                let px = self.cfg.overlay.adjust(
                    fill.avg_px,
                    fill.worst_px,
                    order.req.is_buy,
                    &mut self.rng,
                );
                let fee = px * fill.filled_sz * self.cfg.taker_rate;
                self.account.apply_fill(
                    &order.req.coin,
                    order.req.is_buy,
                    fill.filled_sz,
                    px,
                    fee,
                    now_ms,
                    order.oid,
                );
                true
            }
            // Activated but the limit doesn't cross yet: keep waiting.
            None => false,
        }
    }

    fn try_fill_resting(
        &mut self,
        order: &mut OpenOrder,
        state: &MarketState,
        coin_trades: &[&Trade],
        now_ms: i64,
    ) -> bool {
        let filled = resting_fills(
            self.cfg.queue,
            order.req.is_buy,
            order.req.limit_px,
            &mut order.queue_remaining,
            state.book(&order.req.coin),
            state.price(&order.req.coin),
            coin_trades,
        );
        if !filled {
            return false;
        }
        let mut sz = order.req.sz;
        if order.req.reduce_only {
            let pos = self.account.position_szi(&order.req.coin);
            let delta_sign = if order.req.is_buy { 1.0 } else { -1.0 };
            if pos == 0.0 || pos.signum() == delta_sign {
                return true; // would increase: drop instead of filling
            }
            sz = sz.min(pos.abs());
        }
        let fee = order.req.limit_px * sz * self.cfg.maker_rate;
        self.account.apply_fill(
            &order.req.coin,
            order.req.is_buy,
            sz,
            order.req.limit_px,
            fee,
            now_ms,
            order.oid,
        );
        true
    }

    /// Live-shaped `clearinghouseState` at the current cursor.
    pub fn user_state_json(&self, state: &MarketState, now_ms: i64) -> Value {
        self.account
            .user_state_json(|coin| state.price(coin), &self.universe, now_ms)
    }

    /// Live-shaped `frontendOpenOrders`.
    pub fn open_orders_json(&self) -> Value {
        let entries: Vec<Value> = self
            .open
            .iter()
            .map(|o| {
                let (is_trigger, trigger_px, order_type, tpsl) = match o.req.order_type {
                    OrderType::Limit { .. } => (false, 0.0, "Limit", Value::Null),
                    OrderType::Trigger {
                        trigger_px,
                        is_market,
                        kind,
                    } => {
                        let label = match (kind, is_market) {
                            (TriggerKind::StopLoss, true) => "Stop Market",
                            (TriggerKind::StopLoss, false) => "Stop Limit",
                            (TriggerKind::TakeProfit, true) => "Take Profit Market",
                            (TriggerKind::TakeProfit, false) => "Take Profit Limit",
                        };
                        let tpsl = match kind {
                            TriggerKind::StopLoss => json!("sl"),
                            TriggerKind::TakeProfit => json!("tp"),
                        };
                        (true, trigger_px, label, tpsl)
                    }
                };
                json!({
                    "coin": o.req.coin,
                    "oid": o.oid,
                    "side": if o.req.is_buy { "B" } else { "A" },
                    "limitPx": format_qty(o.req.limit_px),
                    "sz": format_qty(o.req.sz),
                    "origSz": format_qty(o.req.sz),
                    "timestamp": o.time_ms,
                    "isTrigger": is_trigger,
                    "triggerPx": format_qty(trigger_px),
                    "triggerCondition": "N/A",
                    "orderType": order_type,
                    "reduceOnly": o.req.reduce_only,
                    "isPositionTpsl": false,
                    "tpsl": tpsl,
                    "children": [],
                })
            })
            .collect();
        Value::Array(entries)
    }

    pub fn user_fills_json(&self) -> Value {
        self.account.user_fills_json()
    }

    pub fn user_fees_json(&self) -> Value {
        self.account.user_fees_json()
    }

    pub fn account(&self) -> &VirtualAccount {
        &self.account
    }

    pub fn open_order_count(&self) -> usize {
        self.open.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Level, MarketEvent, RecordedEvent, Side};

    fn universe() -> Universe {
        Universe::from_meta(&json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 50},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 50},
        ]}))
        .unwrap()
    }

    fn engine() -> SimEngine {
        SimEngine::new(SimConfig::default(), universe())
    }

    fn state_with_mid(coin: &str, px: f64) -> MarketState {
        let mut s = MarketState::new();
        s.apply(&RecordedEvent {
            seq: 0,
            ts_event_ms: 1,
            ts_recv_ms: 1,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![(coin.into(), px)],
            }),
        });
        s
    }

    fn state_with_book(coin: &str, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> MarketState {
        let level = |&(px, sz): &(f64, f64)| Level { px, sz, n: 1 };
        let mut s = MarketState::new();
        s.apply(&RecordedEvent {
            seq: 0,
            ts_event_ms: 1,
            ts_recv_ms: 1,
            payload: MarketEvent::L2Book(L2Book {
                coin: coin.into(),
                time_ms: 1,
                bids: bids.iter().map(level).collect(),
                asks: asks.iter().map(level).collect(),
            }),
        });
        s
    }

    fn ioc(coin: &str, is_buy: bool, sz: f64, limit_px: f64) -> OrderRequest {
        OrderRequest {
            coin: coin.into(),
            is_buy,
            sz,
            limit_px,
            order_type: OrderType::Limit { tif: Tif::Ioc },
            reduce_only: false,
        }
    }

    #[test]
    fn ioc_buy_fills_at_mid_and_updates_account() {
        let mut e = engine();
        let state = state_with_mid("BTC", 95_000.0);
        let out = e.place(ioc("BTC", true, 0.01, 95_500.0), &state, 10);
        match out {
            OrderOutcome::Filled {
                total_sz,
                avg_px,
                fee,
                ..
            } => {
                assert_eq!(total_sz, 0.01);
                assert_eq!(avg_px, 95_000.0);
                assert!((fee - 95_000.0 * 0.01 * 0.00045).abs() < 1e-9);
            }
            other => panic!("expected fill, got {other:?}"),
        }
        assert_eq!(e.account().position_szi("BTC"), 0.01);
    }

    #[test]
    fn ioc_walks_l2_book_with_partial_fill() {
        let mut e = engine();
        let state = state_with_book("BTC", &[], &[(100.0, 1.0), (101.0, 0.5)]);
        let out = e.place(ioc("BTC", true, 3.0, 101.0), &state, 10);
        match out {
            OrderOutcome::Filled {
                total_sz, avg_px, ..
            } => {
                assert_eq!(total_sz, 1.5, "book only had 1.5 within limit");
                let want = (100.0 * 1.0 + 101.0 * 0.5) / 1.5;
                assert!((avg_px - want).abs() < 1e-9);
            }
            other => panic!("expected partial fill, got {other:?}"),
        }
    }

    #[test]
    fn non_crossing_ioc_errors_like_live() {
        let mut e = engine();
        let state = state_with_mid("BTC", 95_000.0);
        let out = e.place(ioc("BTC", true, 0.01, 90_000.0), &state, 10);
        assert!(matches!(out, OrderOutcome::Error(ref m) if m.contains("immediately match")));
    }

    #[test]
    fn min_notional_and_zero_size_are_rejected() {
        let mut e = engine();
        let state = state_with_mid("BTC", 95_000.0);
        let out = e.place(ioc("BTC", true, 0.00001, 95_000.0), &state, 10);
        assert!(matches!(out, OrderOutcome::Error(ref m) if m.contains("minimum value")));
        let out = e.place(ioc("BTC", true, 0.0, 95_000.0), &state, 10);
        assert!(matches!(out, OrderOutcome::Error(ref m) if m.contains("zero size")));
    }

    #[test]
    fn reduce_only_rejects_increases_and_clamps_to_position() {
        let mut e = engine();
        let state = state_with_mid("BTC", 100.0);
        e.place(ioc("BTC", true, 1.0, 100.0), &state, 1);
        // Same-direction reduce-only is rejected.
        let mut inc = ioc("BTC", true, 1.0, 100.0);
        inc.reduce_only = true;
        assert!(matches!(e.place(inc, &state, 2), OrderOutcome::Error(_)));
        // Oversized close clamps to the position.
        let mut close = ioc("BTC", false, 5.0, 100.0);
        close.reduce_only = true;
        match e.place(close, &state, 3) {
            OrderOutcome::Filled { total_sz, .. } => assert_eq!(total_sz, 1.0),
            other => panic!("expected clamped fill, got {other:?}"),
        }
        assert_eq!(e.account().position_szi("BTC"), 0.0);
    }

    #[test]
    fn gtc_rests_when_not_crossing_and_fills_on_later_tick() {
        let mut e = engine();
        let state = state_with_mid("BTC", 100.0);
        let mut req = ioc("BTC", true, 1.0, 95.0);
        req.order_type = OrderType::Limit { tif: Tif::Gtc };
        let out = e.place(req, &state, 1);
        let oid = match out {
            OrderOutcome::Resting { oid } => oid,
            other => panic!("expected resting, got {other:?}"),
        };
        assert_eq!(e.open_order_count(), 1);
        // Mid drops through the level -> conservative fill at the limit.
        let state2 = state_with_mid("BTC", 94.0);
        e.on_tick(&state2, &[], 2);
        assert_eq!(e.open_order_count(), 0);
        assert_eq!(e.account().position_szi("BTC"), 1.0);
        let fill = &e.account().fills()[0];
        assert_eq!(fill.px, 95.0, "maker fill at the limit price");
        assert_eq!(fill.oid, oid);
        assert!((fill.fee - 95.0 * 1.0 * 0.00015).abs() < 1e-12, "maker fee");
    }

    #[test]
    fn alo_rejects_when_crossing() {
        let mut e = engine();
        let state = state_with_mid("BTC", 100.0);
        let mut req = ioc("BTC", true, 1.0, 100.0);
        req.order_type = OrderType::Limit { tif: Tif::Alo };
        assert!(
            matches!(e.place(req, &state, 1), OrderOutcome::Error(ref m) if m.contains("Post only"))
        );
    }

    #[test]
    fn stop_loss_trigger_rests_then_fires_when_mark_crosses() {
        let mut e = engine();
        let state = state_with_mid("BTC", 100.0);
        e.place(ioc("BTC", true, 1.0, 100.0), &state, 1);
        // SL sell at 90 with slippage-limit 85.
        let sl = OrderRequest {
            coin: "BTC".into(),
            is_buy: false,
            sz: 1.0,
            limit_px: 85.0,
            order_type: OrderType::Trigger {
                trigger_px: 90.0,
                is_market: true,
                kind: TriggerKind::StopLoss,
            },
            reduce_only: true,
        };
        assert!(matches!(
            e.place(sl, &state, 2),
            OrderOutcome::Resting { .. }
        ));
        // Mark above the stop: nothing happens.
        e.on_tick(&state_with_mid("BTC", 95.0), &[], 3);
        assert_eq!(e.open_order_count(), 1);
        // Mark crashes through the stop: position is closed at the mark.
        e.on_tick(&state_with_mid("BTC", 88.0), &[], 4);
        assert_eq!(e.open_order_count(), 0);
        assert_eq!(e.account().position_szi("BTC"), 0.0);
        let fill = e.account().fills().last().unwrap();
        assert_eq!(fill.px, 88.0);
        assert!((fill.closed_pnl - (88.0 - 100.0)).abs() < 1e-9);
    }

    #[test]
    fn cancel_removes_resting_order() {
        let mut e = engine();
        let state = state_with_mid("BTC", 100.0);
        let mut req = ioc("BTC", true, 1.0, 95.0);
        req.order_type = OrderType::Limit { tif: Tif::Gtc };
        let OrderOutcome::Resting { oid } = e.place(req, &state, 1) else {
            panic!()
        };
        assert!(e.cancel("BTC", oid).is_ok());
        assert_eq!(e.open_order_count(), 0);
        assert!(e.cancel("BTC", oid).is_err(), "double cancel fails");
    }

    #[test]
    fn handle_action_speaks_the_wire_format_end_to_end() {
        let mut e = engine();
        let state = state_with_mid("ETH", 3_200.0);
        let action = json!({"type": "order", "orders": [
            {"a": 1, "b": true, "p": "3216.0", "s": "1.5", "r": false,
             "t": {"limit": {"tif": "Ioc"}}}
        ], "grouping": "na"});
        let resp = e.handle_action(&action, &state, 10);
        assert_eq!(resp["status"], "ok");
        let status = &resp["response"]["data"]["statuses"][0];
        assert_eq!(status["filled"]["totalSz"], "1.5");
        assert_eq!(status["filled"]["avgPx"], "3200.0");

        let cancel = json!({"type": "cancel", "cancels": [{"a": 1, "o": 424242}]});
        let resp = e.handle_action(&cancel, &state, 11);
        assert_eq!(resp["status"], "ok");
        assert!(resp["response"]["data"]["statuses"][0]["error"].is_string());

        let unknown = json!({"type": "usdSend"});
        assert_eq!(e.handle_action(&unknown, &state, 12)["status"], "err");
    }

    #[test]
    fn open_orders_render_frontend_shapes() {
        let mut e = engine();
        let state = state_with_mid("BTC", 100.0);
        let mut limit = ioc("BTC", true, 1.0, 95.0);
        limit.order_type = OrderType::Limit { tif: Tif::Gtc };
        e.place(limit, &state, 5);
        e.place(ioc("BTC", true, 1.0, 100.0), &state, 5); // fill to allow SL
        let sl = OrderRequest {
            coin: "BTC".into(),
            is_buy: false,
            sz: 1.0,
            limit_px: 85.0,
            order_type: OrderType::Trigger {
                trigger_px: 90.0,
                is_market: true,
                kind: TriggerKind::StopLoss,
            },
            reduce_only: true,
        };
        e.place(sl, &state, 6);

        let orders = e.open_orders_json();
        let arr = orders.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let limit_o = &arr[0];
        assert_eq!(limit_o["coin"], "BTC");
        assert_eq!(limit_o["side"], "B");
        assert_eq!(limit_o["isTrigger"], false);
        assert_eq!(limit_o["orderType"], "Limit");
        let sl_o = &arr[1];
        assert_eq!(sl_o["isTrigger"], true);
        assert_eq!(sl_o["triggerPx"], "90.0");
        assert_eq!(sl_o["orderType"], "Stop Market");
        assert_eq!(sl_o["tpsl"], "sl");
        assert_eq!(sl_o["reduceOnly"], true);
    }

    #[test]
    fn user_state_reflects_marks_at_cursor() {
        let mut e = engine();
        e.place(
            ioc("BTC", true, 1.0, 100.0),
            &state_with_mid("BTC", 100.0),
            1,
        );
        let later = state_with_mid("BTC", 120.0);
        let us = e.user_state_json(&later, 99);
        assert_eq!(us["assetPositions"][0]["position"]["unrealizedPnl"], "20.0");
    }

    #[test]
    fn end_of_window_stop_rejects_new_orders() {
        let mut e = engine();
        e.set_accepting(false);
        let out = e.place(
            ioc("BTC", true, 1.0, 100.0),
            &state_with_mid("BTC", 100.0),
            1,
        );
        assert!(matches!(out, OrderOutcome::Error(ref m) if m.contains("window ended")));
    }

    #[test]
    fn identical_runs_produce_identical_fills_with_random_overlay() {
        let run = || {
            let cfg = SimConfig {
                overlay: FillOverlay::RandomSpread {
                    max_frac: 0.01,
                    seed: 99,
                },
                ..SimConfig::default()
            };
            let mut e = SimEngine::new(cfg, universe());
            let mut fills = Vec::new();
            for i in 0..5 {
                let state = state_with_mid("BTC", 100.0 + i as f64);
                if let OrderOutcome::Filled { avg_px, .. } =
                    e.place(ioc("BTC", true, 1.0, 200.0), &state, i)
                {
                    fills.push(avg_px);
                }
            }
            fills
        };
        let a = run();
        let b = run();
        assert_eq!(a.len(), 5);
        assert_eq!(a, b, "same session + same orders + same seed = same fills");
        assert!(
            a.windows(2).any(|w| w[0] - 100.0 != w[1] - 101.0),
            "spread actually varies"
        );
    }

    #[test]
    fn conservative_queue_via_trades_stream() {
        let cfg = SimConfig {
            queue: QueueModel::Conservative,
            ..SimConfig::default()
        };
        let mut e = SimEngine::new(cfg, universe());
        // Book with 2.0 ahead of us at 99.
        let state = state_with_book("BTC", &[(99.0, 2.0)], &[(101.0, 1.0)]);
        let mut req = ioc("BTC", true, 1.0, 99.0);
        req.order_type = OrderType::Limit { tif: Tif::Gtc };
        e.place(req, &state, 1);
        let trade = |sz: f64| Trade {
            coin: "BTC".into(),
            side: Side::Sell,
            px: 99.0,
            sz,
            time_ms: 0,
        };
        // 1.5 trades at our level: queue not yet consumed.
        e.on_tick(&state, &[trade(1.5)], 2);
        assert_eq!(e.open_order_count(), 1);
        // Another 0.6 exhausts the 2.0 queue: we fill.
        e.on_tick(&state, &[trade(0.6)], 3);
        assert_eq!(e.open_order_count(), 0);
        assert_eq!(e.account().position_szi("BTC"), 1.0);
    }
}
