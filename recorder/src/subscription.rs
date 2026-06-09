//! Builds Hyperliquid WebSocket subscription request messages.
//!
//! Pure functions only — no transport. This keeps the wire format independently
//! testable from the networking layer.

use serde_json::{json, Value};

/// Which market data streams to subscribe to for a recording session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSelection {
    pub all_mids: bool,
    pub l2_book: bool,
    pub trades: bool,
}

impl Default for StreamSelection {
    /// By default record the full fidelity set required by the digital twin:
    /// L2 book + mids + trades.
    fn default() -> Self {
        Self {
            all_mids: true,
            l2_book: true,
            trades: true,
        }
    }
}

/// Build the ordered list of subscription messages for the given coins.
///
/// `allMids` is a global (coin-less) subscription; `l2Book` and `trades` are
/// per-coin. The returned values are ready to be serialized and sent over the
/// WebSocket.
pub fn build_subscriptions(coins: &[String], sel: &StreamSelection) -> Vec<Value> {
    let mut msgs = Vec::new();

    if sel.all_mids {
        msgs.push(subscribe_message(json!({ "type": "allMids" })));
    }
    for coin in coins {
        if sel.l2_book {
            msgs.push(subscribe_message(json!({ "type": "l2Book", "coin": coin })));
        }
        if sel.trades {
            msgs.push(subscribe_message(json!({ "type": "trades", "coin": coin })));
        }
    }
    msgs
}

/// Wrap a subscription descriptor in the `subscribe` envelope.
fn subscribe_message(subscription: Value) -> Value {
    json!({ "method": "subscribe", "subscription": subscription })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_mids_is_a_single_global_subscription() {
        let sel = StreamSelection {
            all_mids: true,
            l2_book: false,
            trades: false,
        };
        let msgs = build_subscriptions(&["BTC".into(), "ETH".into()], &sel);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0],
            json!({"method":"subscribe","subscription":{"type":"allMids"}})
        );
    }

    #[test]
    fn l2_and_trades_are_per_coin() {
        let sel = StreamSelection {
            all_mids: false,
            l2_book: true,
            trades: true,
        };
        let msgs = build_subscriptions(&["BTC".into(), "ETH".into()], &sel);
        // 2 coins * (l2Book + trades) = 4
        assert_eq!(msgs.len(), 4);
        assert!(msgs.contains(&json!(
            {"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC"}}
        )));
        assert!(msgs.contains(&json!(
            {"method":"subscribe","subscription":{"type":"trades","coin":"ETH"}}
        )));
    }

    #[test]
    fn default_selection_records_everything() {
        let sel = StreamSelection::default();
        let msgs = build_subscriptions(&["BTC".into()], &sel);
        // allMids + l2Book(BTC) + trades(BTC) = 3
        assert_eq!(msgs.len(), 3);
    }
}
