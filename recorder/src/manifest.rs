//! Session manifest: self-describing metadata written alongside the Parquet
//! tables so a recording can be validated and replayed without guesswork.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::recorder::RecordingStats;
use crate::universe::AssetMeta;

/// Schema version for forward/backward compatibility checks on replay.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    /// `mainnet` or `testnet`.
    pub network: String,
    /// WebSocket endpoint used.
    pub endpoint: String,
    pub coins: Vec<String>,
    /// Streams subscribed to (e.g. `["allMids","l2Book","trades"]`).
    pub streams: Vec<String>,
    /// Session wall-clock start, RFC3339.
    pub started_at: String,
    /// Session wall-clock end, RFC3339.
    pub ended_at: String,
    /// Requested recording duration in seconds.
    pub duration_secs: u64,
    pub counts: Counts,
    /// SDK/recorder version that produced the session.
    pub recorder_version: String,
    /// True when the session was recorded with `--daily` rotation: tables are
    /// split per UTC day with `YYYYMMDD_` file prefixes.
    #[serde(default)]
    pub daily: bool,
    /// Per-coin price/size grid metadata from the exchange `meta` endpoint,
    /// so consumers can derive exact tick sizes instead of inferring them
    /// from observed prices. Empty for sessions recorded before this field
    /// existed (`serde(default)` keeps old manifests readable).
    #[serde(default)]
    pub assets: BTreeMap<String, AssetInfo>,
}

/// Price/size grid metadata for one coin.
///
/// The exact tick at price `p` is `max(10^-px_decimals, 10^(floor(log10 p) - 4))`
/// (Hyperliquid allows at most `px_decimals` decimals and 5 significant figures).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInfo {
    /// `szDecimals` from the exchange universe entry.
    pub sz_decimals: u32,
    /// Maximum decimal places a price may carry (`6 - sz_decimals` for perps).
    pub px_decimals: u32,
}

impl From<AssetMeta> for AssetInfo {
    fn from(meta: AssetMeta) -> Self {
        Self {
            sz_decimals: meta.sz_decimals,
            px_decimals: meta.px_decimals(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub recorded: u64,
    pub ignored: u64,
    pub parse_errors: u64,
    pub all_mids: u64,
    pub l2_book: u64,
    pub trades: u64,
}

impl From<&RecordingStats> for Counts {
    fn from(s: &RecordingStats) -> Self {
        Self {
            recorded: s.recorded,
            ignored: s.ignored,
            parse_errors: s.parse_errors,
            all_mids: s.all_mids,
            l2_book: s.l2_book,
            trades: s.trades,
        }
    }
}

impl Manifest {
    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Parse from JSON.
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            network: "mainnet".into(),
            endpoint: "wss://api.hyperliquid.xyz/ws".into(),
            coins: vec!["BTC".into(), "ETH".into()],
            streams: vec!["allMids".into(), "l2Book".into(), "trades".into()],
            started_at: "2026-06-09T00:00:00Z".into(),
            ended_at: "2026-06-09T00:05:00Z".into(),
            duration_secs: 300,
            counts: Counts {
                recorded: 10,
                ignored: 2,
                parse_errors: 0,
                all_mids: 4,
                l2_book: 4,
                trades: 2,
            },
            recorder_version: "0.1.0".into(),
            daily: false,
            assets: BTreeMap::from([
                (
                    "BTC".to_string(),
                    AssetInfo {
                        sz_decimals: 5,
                        px_decimals: 1,
                    },
                ),
                (
                    "ETH".to_string(),
                    AssetInfo {
                        sz_decimals: 4,
                        px_decimals: 2,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let m = sample();
        let json = m.to_json().unwrap();
        let back = Manifest::from_json(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn old_manifests_without_assets_still_parse() {
        let mut json = serde_json::to_value(sample()).unwrap();
        json.as_object_mut().unwrap().remove("assets");
        let back = Manifest::from_json(&json.to_string()).unwrap();
        assert!(back.assets.is_empty());
    }

    #[test]
    fn asset_info_derives_px_decimals_from_meta() {
        let info = AssetInfo::from(AssetMeta { sz_decimals: 5 });
        assert_eq!(info.sz_decimals, 5);
        assert_eq!(info.px_decimals, 1);
    }

    #[test]
    fn counts_are_derived_from_stats() {
        let stats = RecordingStats {
            recorded: 5,
            ignored: 1,
            parse_errors: 0,
            all_mids: 2,
            l2_book: 2,
            trades: 1,
        };
        let counts = Counts::from(&stats);
        assert_eq!(counts.recorded, 5);
        assert_eq!(counts.l2_book, 2);
    }
}
