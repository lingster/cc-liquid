//! The Hyperliquid coin universe: parsing the `meta` response and validating a
//! requested coin list against it.
//!
//! Both functions are pure (no network) so the rules — "which coins exist" and
//! "which requested coins to keep/drop" — are independently testable. The actual
//! HTTP fetch lives in [`crate::info`].

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail};
use serde_json::Value;

/// Maximum decimal places allowed in a Hyperliquid perp price. Together with
/// an asset's `szDecimals` this fixes the price grid: prices may carry at most
/// `MAX_PERP_PX_DECIMALS - szDecimals` decimals (and at most 5 significant
/// figures, which is price-level dependent and left to consumers).
pub const MAX_PERP_PX_DECIMALS: u32 = 6;

/// Per-asset metadata from the `meta` universe entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetMeta {
    /// `szDecimals` as reported by the exchange.
    pub sz_decimals: u32,
}

impl AssetMeta {
    /// Maximum decimal places a price for this asset may carry.
    pub fn px_decimals(&self) -> u32 {
        MAX_PERP_PX_DECIMALS.saturating_sub(self.sz_decimals)
    }
}

/// Outcome of validating requested coins against the live universe.
///
/// `kept` preserves the caller's original order; `dropped` lists the coins that
/// are not tradeable on the target network (so one bad coin never silently
/// poisons a recording).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinValidation {
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
}

/// Split `requested` into coins present in `available` (`kept`) and coins that
/// are not (`dropped`), preserving the requested order.
pub fn validate_coins(requested: &[String], available: &HashSet<String>) -> CoinValidation {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for coin in requested {
        if available.contains(coin) {
            kept.push(coin.clone());
        } else {
            dropped.push(coin.clone());
        }
    }
    CoinValidation { kept, dropped }
}

/// Parse the perp `universe` entries (name + `szDecimals`) out of a
/// Hyperliquid `info`/`meta` response body
/// (`{"universe":[{"name":"BTC","szDecimals":5,..},..], ..}`).
///
/// A missing `szDecimals` defaults to 0 rather than dropping the asset, so a
/// schema relaxation upstream can never silently shrink the universe.
pub fn parse_perp_assets(body: &str) -> anyhow::Result<HashMap<String, AssetMeta>> {
    let value: Value = serde_json::from_str(body)?;
    let arr = value
        .get("universe")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("meta response missing `universe` array"))?;

    let mut assets = HashMap::with_capacity(arr.len());
    for entry in arr {
        if let Some(name) = entry.get("name").and_then(Value::as_str) {
            let sz_decimals = entry.get("szDecimals").and_then(Value::as_u64).unwrap_or(0) as u32;
            assets.insert(name.to_string(), AssetMeta { sz_decimals });
        }
    }
    if assets.is_empty() {
        bail!("meta response contained an empty universe");
    }
    Ok(assets)
}

/// Parse just the perp coin names out of a `meta` response body.
pub fn parse_perp_universe(body: &str) -> anyhow::Result<HashSet<String>> {
    Ok(parse_perp_assets(body)?.into_keys().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn universe(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keeps_known_coins_and_drops_unknown_preserving_order() {
        let available = universe(&["BTC", "ETH", "SOL"]);
        let requested = vec![
            "BTC".to_string(),
            "ETH".to_string(),
            "SOL".to_string(),
            "NMR".to_string(),
        ];
        let result = validate_coins(&requested, &available);
        assert_eq!(result.kept, vec!["BTC", "ETH", "SOL"]);
        assert_eq!(result.dropped, vec!["NMR"]);
    }

    #[test]
    fn all_unknown_yields_empty_kept() {
        let available = universe(&["BTC"]);
        let result = validate_coins(&["FOO".to_string(), "BAR".to_string()], &available);
        assert!(result.kept.is_empty());
        assert_eq!(result.dropped, vec!["FOO", "BAR"]);
    }

    #[test]
    fn parses_universe_names_from_meta_body() {
        let body = r#"{"universe":[
            {"name":"BTC","szDecimals":5},
            {"name":"ETH","szDecimals":4},
            {"name":"SOL","szDecimals":2}]}"#;
        let names = parse_perp_universe(body).unwrap();
        assert!(names.contains("BTC"));
        assert!(names.contains("ETH"));
        assert!(names.contains("SOL"));
        assert!(!names.contains("NMR"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn parses_asset_metadata_with_sz_decimals() {
        let body = r#"{"universe":[
            {"name":"BTC","szDecimals":5},
            {"name":"ETH","szDecimals":4},
            {"name":"LEGACY"}]}"#;
        let assets = parse_perp_assets(body).unwrap();
        assert_eq!(assets["BTC"], AssetMeta { sz_decimals: 5 });
        assert_eq!(assets["ETH"], AssetMeta { sz_decimals: 4 });
        // Missing szDecimals keeps the asset with a safe default.
        assert_eq!(assets["LEGACY"], AssetMeta { sz_decimals: 0 });
    }

    #[test]
    fn px_decimals_is_complement_of_sz_decimals() {
        assert_eq!(AssetMeta { sz_decimals: 5 }.px_decimals(), 1); // BTC -> 0.1 grid floor
        assert_eq!(AssetMeta { sz_decimals: 4 }.px_decimals(), 2); // ETH -> 0.01
        assert_eq!(AssetMeta { sz_decimals: 0 }.px_decimals(), 6);
        // Defensive clamp: never underflows even on nonsense input.
        assert_eq!(AssetMeta { sz_decimals: 9 }.px_decimals(), 0);
    }

    #[test]
    fn errors_when_universe_field_is_missing() {
        assert!(parse_perp_universe(r#"{"notUniverse":[]}"#).is_err());
    }

    #[test]
    fn errors_on_empty_universe() {
        assert!(parse_perp_universe(r#"{"universe":[]}"#).is_err());
    }

    #[test]
    fn errors_on_invalid_json() {
        assert!(parse_perp_universe("not json").is_err());
    }
}
