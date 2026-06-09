//! Session manifest: self-describing metadata written alongside the Parquet
//! tables so a recording can be validated and replayed without guesswork.

use serde::{Deserialize, Serialize};

use crate::recorder::RecordingStats;

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
