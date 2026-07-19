//! Layered recorder settings: CLI flags over a YAML config file over defaults.
//!
//! Both the CLI and the config file produce a [`PartialConfig`] (every field
//! optional); [`PartialConfig::or`] layers one over another and
//! [`PartialConfig::finalize`] fills the remaining holes with built-in
//! defaults. Precedence is therefore explicit and testable without clap:
//!
//! ```text
//! cli.or(file).finalize()  // CLI wins, then config.yaml, then defaults
//! ```
//!
//! The config file uses the same names as the CLI flags (`out`, `cc`,
//! `l2_shards`, ...) so nothing needs a mental translation table. Unknown keys
//! are rejected so a typo cannot silently fall back to a default.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::config::Network;
use crate::crowdcent;

/// Default config file, looked up in the working directory when `--config` is
/// not given. Missing is fine (defaults apply); unreadable/invalid is an error.
pub const DEFAULT_CONFIG_FILE: &str = "config.yaml";

/// Default session output root when neither CLI nor config file set `out`.
pub const DEFAULT_OUT_DIR: &str = "/data/hyperliquid/sessions";

/// One layer of settings; `None` means "not specified at this layer".
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PartialConfig {
    pub assets: Option<Vec<String>>,
    pub cc: Option<bool>,
    pub cc_challenge: Option<String>,
    pub cc_url: Option<String>,
    pub duration: Option<u64>,
    pub network: Option<String>,
    pub out: Option<PathBuf>,
    pub no_l2: Option<bool>,
    pub no_trades: Option<bool>,
    pub no_mids: Option<bool>,
    pub shard_size: Option<usize>,
    pub l2_shards: Option<usize>,
    pub flush_interval: Option<u64>,
    pub daily: Option<bool>,
    pub idle_timeout: Option<u64>,
}

/// Fully-resolved settings with every default applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub assets: Vec<String>,
    pub cc: bool,
    pub cc_challenge: String,
    pub cc_url: String,
    pub duration: u64,
    pub network: Network,
    pub out: PathBuf,
    pub no_l2: bool,
    pub no_trades: bool,
    pub no_mids: bool,
    pub shard_size: usize,
    pub l2_shards: usize,
    pub flush_interval: u64,
    pub daily: bool,
    pub idle_timeout: u64,
}

impl PartialConfig {
    /// Layer `self` over `fallback`: any field set here wins.
    pub fn or(self, fallback: PartialConfig) -> PartialConfig {
        PartialConfig {
            assets: self.assets.or(fallback.assets),
            cc: self.cc.or(fallback.cc),
            cc_challenge: self.cc_challenge.or(fallback.cc_challenge),
            cc_url: self.cc_url.or(fallback.cc_url),
            duration: self.duration.or(fallback.duration),
            network: self.network.or(fallback.network),
            out: self.out.or(fallback.out),
            no_l2: self.no_l2.or(fallback.no_l2),
            no_trades: self.no_trades.or(fallback.no_trades),
            no_mids: self.no_mids.or(fallback.no_mids),
            shard_size: self.shard_size.or(fallback.shard_size),
            l2_shards: self.l2_shards.or(fallback.l2_shards),
            flush_interval: self.flush_interval.or(fallback.flush_interval),
            daily: self.daily.or(fallback.daily),
            idle_timeout: self.idle_timeout.or(fallback.idle_timeout),
        }
    }

    /// Fill unset fields with built-in defaults and validate parsed values.
    pub fn finalize(self) -> anyhow::Result<Settings> {
        let network = match self.network {
            Some(s) => s.parse::<Network>().map_err(anyhow::Error::msg)?,
            None => Network::Mainnet,
        };
        Ok(Settings {
            assets: self.assets.unwrap_or_default(),
            cc: self.cc.unwrap_or(false),
            cc_challenge: self
                .cc_challenge
                .unwrap_or_else(|| crowdcent::DEFAULT_CHALLENGE_SLUG.to_string()),
            cc_url: self
                .cc_url
                .unwrap_or_else(|| crowdcent::DEFAULT_BASE_URL.to_string()),
            duration: self.duration.unwrap_or(0),
            network,
            out: self.out.unwrap_or_else(|| PathBuf::from(DEFAULT_OUT_DIR)),
            no_l2: self.no_l2.unwrap_or(false),
            no_trades: self.no_trades.unwrap_or(false),
            no_mids: self.no_mids.unwrap_or(false),
            shard_size: self.shard_size.unwrap_or(0),
            l2_shards: self.l2_shards.unwrap_or(1),
            flush_interval: self.flush_interval.unwrap_or(300),
            daily: self.daily.unwrap_or(false),
            idle_timeout: self.idle_timeout.unwrap_or(90),
        })
    }
}

/// Parse a YAML config file into a [`PartialConfig`].
pub fn load_file(path: &Path) -> anyhow::Result<PartialConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Load `path` if it exists; a missing optional config file is just defaults.
pub fn load_file_if_exists(path: &Path) -> anyhow::Result<PartialConfig> {
    if path.exists() {
        load_file(path)
    } else {
        Ok(PartialConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let s = PartialConfig::default().finalize().unwrap();
        assert_eq!(s.out, PathBuf::from("/data/hyperliquid/sessions"));
        assert_eq!(s.network, Network::Mainnet);
        assert_eq!(s.l2_shards, 1);
        assert_eq!(s.flush_interval, 300);
        assert_eq!(s.idle_timeout, 90);
        assert!(!s.cc);
        assert!(!s.daily);
        assert!(s.assets.is_empty());
    }

    #[test]
    fn yaml_fields_parse_and_finalize() {
        let file: PartialConfig = serde_yaml::from_str(
            r#"
            cc: true
            daily: true
            l2_shards: 4
            out: /data/hyperliquid/sessions/long-run
            network: testnet
            assets: [BTC, ETH]
            idle_timeout: 120
            "#,
        )
        .unwrap();
        let s = file.finalize().unwrap();
        assert!(s.cc && s.daily);
        assert_eq!(s.l2_shards, 4);
        assert_eq!(s.out, PathBuf::from("/data/hyperliquid/sessions/long-run"));
        assert_eq!(s.network, Network::Testnet);
        assert_eq!(s.assets, vec!["BTC", "ETH"]);
        assert_eq!(s.idle_timeout, 120);
    }

    #[test]
    fn cli_layer_wins_over_file_layer() {
        let file: PartialConfig = serde_yaml::from_str(
            "out: /from/file\nl2_shards: 4\ndaily: true\n",
        )
        .unwrap();
        let cli = PartialConfig {
            out: Some(PathBuf::from("/from/cli")),
            l2_shards: Some(8),
            ..Default::default()
        };
        let s = cli.or(file).finalize().unwrap();
        assert_eq!(s.out, PathBuf::from("/from/cli")); // CLI wins
        assert_eq!(s.l2_shards, 8); // CLI wins
        assert!(s.daily); // file fills the CLI hole
    }

    #[test]
    fn unknown_yaml_keys_are_rejected() {
        let err = serde_yaml::from_str::<PartialConfig>("l2shards: 4\n").unwrap_err();
        assert!(err.to_string().contains("l2shards"), "{err}");
    }

    #[test]
    fn bad_network_string_is_an_error() {
        let file: PartialConfig = serde_yaml::from_str("network: mixednet\n").unwrap();
        assert!(file.finalize().is_err());
    }

    #[test]
    fn missing_optional_file_yields_empty_layer() {
        let layer = load_file_if_exists(Path::new("/nonexistent/config.yaml")).unwrap();
        assert_eq!(layer, PartialConfig::default());
    }
}
