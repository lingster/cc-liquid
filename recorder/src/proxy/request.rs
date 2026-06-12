//! Request classification: map raw `/info` and `/exchange` JSON bodies onto the
//! logical Appendix A methods they correspond to (PRD §B.3).
//!
//! Pure functions — no I/O — so the routing rules (which requests are market
//! reads, which are writes) are unit-testable in isolation. The proxy keys on
//! the body's `type` (info) / `action.type` (exchange) discriminator, which
//! makes the capture log self-classifying.

use serde_json::Value;

/// The two REST endpoints the Hyperliquid SDK posts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Info,
    Exchange,
}

impl Endpoint {
    /// Resolve a request path to an endpoint, if supported.
    pub fn from_path(path: &str) -> Option<Self> {
        match path {
            "/info" => Some(Endpoint::Info),
            "/exchange" => Some(Endpoint::Exchange),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Endpoint::Info => "/info",
            Endpoint::Exchange => "/exchange",
        }
    }
}

/// The logical Appendix A method a wire request corresponds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodTag {
    // --- /info reads ---
    AllMids,
    UserState,
    Meta,
    SpotMeta,
    FrontendOpenOrders,
    UserFills,
    UserFillsByTime,
    UserFees,
    /// Any other `/info` type (forwarded verbatim, tagged for the log).
    InfoOther(String),
    // --- /exchange writes ---
    BulkOrders,
    BulkCancel,
    /// Any other `/exchange` action type.
    ExchangeOther(String),
    /// Body had no recognizable discriminator.
    Unknown,
}

impl MethodTag {
    /// Stable string tag for the capture log (`method_tag` field).
    pub fn as_str(&self) -> &str {
        match self {
            MethodTag::AllMids => "all_mids",
            MethodTag::UserState => "user_state",
            MethodTag::Meta => "meta",
            MethodTag::SpotMeta => "spot_meta",
            MethodTag::FrontendOpenOrders => "frontend_open_orders",
            MethodTag::UserFills => "user_fills",
            MethodTag::UserFillsByTime => "user_fills_by_time",
            MethodTag::UserFees => "user_fees",
            MethodTag::InfoOther(t) => t,
            MethodTag::BulkOrders => "bulk_orders",
            MethodTag::BulkCancel => "bulk_cancel",
            MethodTag::ExchangeOther(t) => t,
            MethodTag::Unknown => "unknown",
        }
    }

    /// Pure market-data reads that the playback source can answer offline
    /// (PRD §B.2: only market reads are toggleable; account reads forward).
    pub fn is_market_read(&self) -> bool {
        matches!(
            self,
            MethodTag::AllMids | MethodTag::Meta | MethodTag::SpotMeta
        )
    }
}

/// Classify a parsed request body for the given endpoint.
pub fn classify(endpoint: Endpoint, body: &Value) -> MethodTag {
    match endpoint {
        Endpoint::Info => classify_info(body),
        Endpoint::Exchange => classify_exchange(body),
    }
}

fn classify_info(body: &Value) -> MethodTag {
    let Some(ty) = body.get("type").and_then(Value::as_str) else {
        return MethodTag::Unknown;
    };
    match ty {
        "allMids" => MethodTag::AllMids,
        "clearinghouseState" => MethodTag::UserState,
        "meta" => MethodTag::Meta,
        "spotMeta" => MethodTag::SpotMeta,
        "frontendOpenOrders" => MethodTag::FrontendOpenOrders,
        "userFills" => MethodTag::UserFills,
        "userFillsByTime" => MethodTag::UserFillsByTime,
        "userFees" => MethodTag::UserFees,
        other => MethodTag::InfoOther(other.to_string()),
    }
}

fn classify_exchange(body: &Value) -> MethodTag {
    let Some(ty) = body
        .get("action")
        .and_then(|a| a.get("type"))
        .and_then(Value::as_str)
    else {
        return MethodTag::Unknown;
    };
    match ty {
        "order" => MethodTag::BulkOrders,
        "cancel" => MethodTag::BulkCancel,
        other => MethodTag::ExchangeOther(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_supported_paths() {
        assert_eq!(Endpoint::from_path("/info"), Some(Endpoint::Info));
        assert_eq!(Endpoint::from_path("/exchange"), Some(Endpoint::Exchange));
        assert_eq!(Endpoint::from_path("/ws"), None);
        assert_eq!(Endpoint::from_path("/"), None);
    }

    #[test]
    fn classifies_every_appendix_a_info_method() {
        let cases = [
            (json!({"type":"allMids","dex":""}), MethodTag::AllMids),
            (
                json!({"type":"clearinghouseState","user":"0xabc","dex":""}),
                MethodTag::UserState,
            ),
            (json!({"type":"meta","dex":""}), MethodTag::Meta),
            (json!({"type":"spotMeta"}), MethodTag::SpotMeta),
            (
                json!({"type":"frontendOpenOrders","user":"0xabc"}),
                MethodTag::FrontendOpenOrders,
            ),
            (
                json!({"type":"userFills","user":"0xabc"}),
                MethodTag::UserFills,
            ),
            (
                json!({"type":"userFillsByTime","user":"0xabc","startTime":1,"endTime":2}),
                MethodTag::UserFillsByTime,
            ),
            (
                json!({"type":"userFees","user":"0xabc"}),
                MethodTag::UserFees,
            ),
        ];
        for (body, want) in cases {
            assert_eq!(classify(Endpoint::Info, &body), want, "body: {body}");
        }
    }

    #[test]
    fn unknown_info_types_are_tagged_but_not_market_reads() {
        let tag = classify(Endpoint::Info, &json!({"type":"candleSnapshot"}));
        assert_eq!(tag, MethodTag::InfoOther("candleSnapshot".into()));
        assert_eq!(tag.as_str(), "candleSnapshot");
        assert!(!tag.is_market_read());
    }

    #[test]
    fn classifies_exchange_actions() {
        let order = json!({
            "action": {"type":"order","orders":[{"a":0,"b":true}]},
            "nonce": 1, "signature": {"r":"0x1","s":"0x2","v":27}
        });
        assert_eq!(classify(Endpoint::Exchange, &order), MethodTag::BulkOrders);

        let cancel = json!({"action":{"type":"cancel","cancels":[{"a":0,"o":123}]}});
        assert_eq!(classify(Endpoint::Exchange, &cancel), MethodTag::BulkCancel);

        let other = classify(Endpoint::Exchange, &json!({"action":{"type":"usdSend"}}));
        assert_eq!(other, MethodTag::ExchangeOther("usdSend".into()));
    }

    #[test]
    fn missing_discriminator_is_unknown() {
        assert_eq!(classify(Endpoint::Info, &json!({})), MethodTag::Unknown);
        assert_eq!(
            classify(Endpoint::Info, &json!({"type": 7})),
            MethodTag::Unknown
        );
        assert_eq!(
            classify(Endpoint::Exchange, &json!({"nonce":1})),
            MethodTag::Unknown
        );
    }

    #[test]
    fn only_mids_meta_and_spot_meta_are_playback_servable() {
        assert!(MethodTag::AllMids.is_market_read());
        assert!(MethodTag::Meta.is_market_read());
        assert!(MethodTag::SpotMeta.is_market_read());
        for tag in [
            MethodTag::UserState,
            MethodTag::FrontendOpenOrders,
            MethodTag::UserFills,
            MethodTag::UserFillsByTime,
            MethodTag::UserFees,
            MethodTag::BulkOrders,
            MethodTag::BulkCancel,
        ] {
            assert!(!tag.is_market_read(), "{tag:?} must always forward");
        }
    }

    #[test]
    fn method_tags_have_stable_log_strings() {
        assert_eq!(MethodTag::AllMids.as_str(), "all_mids");
        assert_eq!(MethodTag::UserState.as_str(), "user_state");
        assert_eq!(MethodTag::BulkOrders.as_str(), "bulk_orders");
        assert_eq!(MethodTag::BulkCancel.as_str(), "bulk_cancel");
        assert_eq!(MethodTag::Unknown.as_str(), "unknown");
    }
}
