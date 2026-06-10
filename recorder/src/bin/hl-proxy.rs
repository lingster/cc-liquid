//! `hl-proxy` — the Digital Twin Proxy (PRD Appendix B).
//!
//! A standalone loopback HTTP process speaking the Hyperliquid wire protocol.
//! cc-liquid points at it with a one-line override
//! (`--set base_url=http://127.0.0.1:8088`); no other application change.
//!
//! Examples:
//!
//! ```bash
//! # Capture real testnet traffic (incl. orders) while forwarding live:
//! hl-proxy --listen 127.0.0.1:8088 --network testnet --allow-trading \
//!     --out sessions/proxy-demo
//!
//! # Serve a fixed recorded session as the market feed, log everything:
//! hl-proxy --listen 127.0.0.1:8088 --market-source playback \
//!     --session sessions/demo --out sessions/proxy-replay
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use hl_recorder::config::Network;
use hl_recorder::proxy::handler::{MarketSource, ProxyConfig, ProxyHandler};
use hl_recorder::proxy::log::JsonlSink;
use hl_recorder::proxy::market::{MarketDataProvider, PlaybackMarket};
use hl_recorder::proxy::server::bind_and_serve;
use hl_recorder::proxy::upstream::HttpUpstream;

/// Hyperliquid wire-protocol proxy: capture, forward and playback.
#[derive(Parser, Debug)]
#[command(name = "hl-proxy", version)]
struct Cli {
    /// Address to listen on (loopback HTTP, no TLS — see PRD §B.4).
    #[arg(long, default_value = "127.0.0.1:8088")]
    listen: String,

    /// Target network. Determines the default upstream and gates writes:
    /// `/exchange` is only ever forwarded on testnet.
    #[arg(long, default_value = "mainnet")]
    network: Network,

    /// Where `/info` market reads (allMids/meta/spotMeta) are answered from.
    #[arg(long, default_value = "forward")]
    market_source: MarketSource,

    /// Recorded session directory to play back (required with
    /// `--market-source playback`).
    #[arg(long)]
    session: Option<PathBuf>,

    /// Output directory for the capture log (`rpc_log.jsonl`).
    #[arg(long)]
    out: PathBuf,

    /// Forward `POST /exchange` to the upstream. Requires `--network testnet`;
    /// mainnet writes are rejected regardless (PRD §B.2).
    #[arg(long, default_value_t = false)]
    allow_trading: bool,

    /// Store a hash placeholder instead of raw signatures in the capture log.
    #[arg(long, default_value_t = false)]
    redact_signatures: bool,

    /// Upstream base URL override (defaults to the network's API endpoint).
    #[arg(long)]
    upstream: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    if cli.allow_trading && cli.network != Network::Testnet {
        warn!(
            "--allow-trading has no effect on {}: /exchange writes are testnet-only in v1",
            cli.network.as_str()
        );
    }

    let market: Option<Box<dyn MarketDataProvider>> = match cli.market_source {
        MarketSource::Playback => {
            let session = cli
                .session
                .as_ref()
                .context("--market-source playback requires --session <dir>")?;
            info!("loading playback session from {}", session.display());
            Some(Box::new(PlaybackMarket::load(session)?))
        }
        MarketSource::Forward => None,
    };

    let upstream_url = cli
        .upstream
        .unwrap_or_else(|| cli.network.api_endpoint().to_string());
    let cfg = ProxyConfig {
        network: cli.network,
        market_source: cli.market_source,
        allow_trading: cli.allow_trading,
        redact_signatures: cli.redact_signatures,
    };
    let handler = ProxyHandler::new(
        cfg,
        Box::new(HttpUpstream::new(upstream_url.clone())),
        market,
        Box::new(JsonlSink::create(&cli.out)?),
    )?;

    let (addr, task) = bind_and_serve(&cli.listen, Arc::new(handler)).await?;
    info!(
        "hl-proxy listening on http://{addr} (network={}, market_source={:?}, allow_trading={}, upstream={upstream_url})",
        cli.network.as_str(),
        cli.market_source,
        cli.allow_trading,
    );
    info!("point cc-liquid at it: uv run cc-liquid <cmd> --set base_url=http://{addr}");
    info!("capture log: {}", cli.out.join("rpc_log.jsonl").display());

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
        _ = task => warn!("server task exited"),
    }
    Ok(())
}
