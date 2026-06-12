//! Thin HTTP client for the Hyperliquid `info` endpoint.
//!
//! Deliberately minimal: it performs the one-shot `meta` POST and hands the raw
//! body to the pure [`crate::universe::parse_perp_assets`] parser. Network I/O
//! here, parsing/validation rules over in [`crate::universe`]. Exercised by a
//! network-gated test.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::universe::{parse_perp_assets, AssetMeta};

/// Upper bound on the buffered `meta` response body. The real payload is tens
/// of KiB; 16 MiB is far larger yet still bounds memory against a hostile or
/// malfunctioning endpoint streaming an unbounded body (M3).
const MAX_META_BYTES: usize = 16 * 1024 * 1024;

/// Fetch the raw `meta` response body from `info_endpoint`
/// (e.g. `https://api.hyperliquid.xyz/info`). The verbatim body is also what
/// the recorder persists as the session's `meta.json` snapshot (PRD §5.2), so
/// playback serves the exact universe/szDecimals seen at record time.
pub async fn fetch_meta_body(info_endpoint: &str) -> anyhow::Result<String> {
    // Bounded timeouts so a stalled endpoint can never hang the recorder at
    // startup before any recording deadline exists (M2).
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()?;

    let mut resp = client
        .post(info_endpoint)
        .header("Content-Type", "application/json")
        .body(r#"{"type":"meta"}"#)
        .send()
        .await?
        .error_for_status()?;

    // Stream the body with a hard cap so an unbounded response can't exhaust
    // memory (M3).
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > MAX_META_BYTES {
            anyhow::bail!(
                "info `meta` response exceeded {MAX_META_BYTES} bytes; refusing to buffer further"
            );
        }
        buf.extend_from_slice(&chunk);
    }

    String::from_utf8(buf)
        .map_err(|e| anyhow::anyhow!("info `meta` response was not valid UTF-8: {e}"))
}

/// Fetch the set of tradeable perp coin names from `info_endpoint`.
pub async fn fetch_perp_universe(info_endpoint: &str) -> anyhow::Result<HashSet<String>> {
    Ok(fetch_perp_assets(info_endpoint)
        .await?
        .into_keys()
        .collect())
}

/// Fetch the tradeable perp universe with per-asset metadata (`szDecimals`)
/// from `info_endpoint` (e.g. `https://api.hyperliquid.xyz/info`).
pub async fn fetch_perp_assets(info_endpoint: &str) -> anyhow::Result<HashMap<String, AssetMeta>> {
    parse_perp_assets(&fetch_meta_body(info_endpoint).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Network;

    /// Live check that the mainnet universe lists majors but not NMR.
    /// Network-gated: `cargo test --test ... -- --ignored` / run explicitly.
    #[tokio::test]
    #[ignore = "requires network access to Hyperliquid mainnet"]
    async fn live_fetches_mainnet_perp_universe() {
        let names = fetch_perp_universe(Network::Mainnet.info_endpoint())
            .await
            .expect("fetch universe");
        assert!(names.contains("BTC"));
        assert!(names.contains("ETH"));
        assert!(names.contains("SOL"));
        assert!(!names.contains("NMR"));
    }
}
