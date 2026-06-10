//! Recording session configuration and endpoint resolution.

use std::path::PathBuf;
use std::str::FromStr;

use crate::subscription::StreamSelection;

/// Target Hyperliquid network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    /// WebSocket endpoint for this network.
    pub fn ws_endpoint(&self) -> &'static str {
        match self {
            Network::Mainnet => "wss://api.hyperliquid.xyz/ws",
            Network::Testnet => "wss://api.hyperliquid-testnet.xyz/ws",
        }
    }

    /// HTTP API base URL for this network (REST `/info` + `/exchange`).
    pub fn api_endpoint(&self) -> &'static str {
        match self {
            Network::Mainnet => "https://api.hyperliquid.xyz",
            Network::Testnet => "https://api.hyperliquid-testnet.xyz",
        }
    }

    /// HTTP `info` endpoint for this network (used to fetch the coin universe).
    pub fn info_endpoint(&self) -> &'static str {
        match self {
            Network::Mainnet => "https://api.hyperliquid.xyz/info",
            Network::Testnet => "https://api.hyperliquid-testnet.xyz/info",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        }
    }
}

impl FromStr for Network {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "mainnet" | "main" => Ok(Network::Mainnet),
            "testnet" | "test" => Ok(Network::Testnet),
            other => Err(format!("unknown network `{other}` (use mainnet|testnet)")),
        }
    }
}

/// A fully-resolved recording session configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordConfig {
    pub network: Network,
    pub coins: Vec<String>,
    pub streams: StreamSelection,
    pub duration_secs: u64,
    pub out_dir: PathBuf,
}

impl RecordConfig {
    /// Human-readable list of subscribed stream names (for the manifest).
    pub fn stream_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.streams.all_mids {
            names.push("allMids".to_string());
        }
        if self.streams.l2_book {
            names.push("l2Book".to_string());
        }
        if self.streams.trades {
            names.push("trades".to_string());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_parses_case_insensitively() {
        assert_eq!(Network::from_str("Mainnet").unwrap(), Network::Mainnet);
        assert_eq!(Network::from_str("TEST").unwrap(), Network::Testnet);
        assert!(Network::from_str("foo").is_err());
    }

    #[test]
    fn endpoints_differ_by_network() {
        assert!(Network::Mainnet
            .ws_endpoint()
            .contains("api.hyperliquid.xyz"));
        assert!(Network::Testnet
            .ws_endpoint()
            .contains("hyperliquid-testnet"));
    }

    #[test]
    fn stream_names_reflect_selection() {
        let cfg = RecordConfig {
            network: Network::Mainnet,
            coins: vec!["BTC".into()],
            streams: StreamSelection {
                all_mids: true,
                l2_book: true,
                trades: false,
            },
            duration_secs: 300,
            out_dir: PathBuf::from("/tmp/x"),
        };
        assert_eq!(cfg.stream_names(), vec!["allMids", "l2Book"]);
    }
}
