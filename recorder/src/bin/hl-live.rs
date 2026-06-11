//! hl-live — run an exported `orderbooker` ONNX model against live (or
//! replayed) Hyperliquid L2 data and score its predictions.
//!
//! ```bash
//! # Live: 5 minutes on mainnet, scoring at the model's own trained horizon
//! hl-live model.onnx --duration 300
//!
//! # Live at explicit horizons
//! hl-live model.onnx --horizons 50,100,200 --duration 300 --out results.parquet
//!
//! # Deterministic replay of a recorded session (parity testing / CI)
//! hl-live model.onnx --replay sessions/demo5min
//! ```

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tracing::info;

use hl_recorder::client::WsSource;
use hl_recorder::config::Network;
use hl_recorder::live::harness::{run_live, run_replay, Harness};
use hl_recorder::live::ledger::summarize;
use hl_recorder::live::model::{OnnxModel, Sidecar};
use hl_recorder::live::results::write_results;
use hl_recorder::replay::load_session;
use hl_recorder::subscription::{build_subscriptions, StreamSelection};

#[derive(Parser, Debug)]
#[command(
    name = "hl-live",
    about = "Score an exported orderbooker model on live or replayed L2 data"
)]
struct Args {
    /// Exported ONNX model (expects `<model>.onnx.json` sidecar next to it)
    model: PathBuf,
    /// Comma-separated horizons in snapshots; defaults to the model's trained horizon
    #[arg(long)]
    horizons: Option<String>,
    /// Wall-clock seconds to run (live mode)
    #[arg(long, default_value_t = 300)]
    duration: u64,
    /// mainnet or testnet
    #[arg(long, default_value = "mainnet")]
    network: String,
    /// Replay a recorded session directory instead of connecting live
    #[arg(long)]
    replay: Option<PathBuf>,
    /// Output parquet of resolved predictions
    #[arg(long, default_value = "live-results.parquet")]
    out: PathBuf,
}

fn parse_horizons(spec: Option<&str>, fallback: u32) -> anyhow::Result<Vec<u32>> {
    match spec {
        None => Ok(vec![fallback]),
        Some(s) => s
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .map(|p| p.trim().parse::<u32>().map_err(Into::into))
            .collect(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let meta = Sidecar::load_for_model(&args.model)?;
    let horizons = parse_horizons(args.horizons.as_deref(), meta.grid.horizon)?;
    info!(
        "model {} | coin {} tick {} window {} depth {} | horizons {:?}",
        args.model.display(),
        meta.coin,
        meta.tick,
        meta.grid.window,
        meta.grid.depth,
        horizons
    );

    let mut model = OnnxModel::load(&args.model, &meta)?;
    let mut harness = Harness::new(&meta, horizons, &mut model)?;

    if let Some(session) = &args.replay {
        info!("replaying session {}", session.display());
        run_replay(&mut harness, &load_session(session)?)?;
    } else {
        let network = match args.network.as_str() {
            "mainnet" => Network::Mainnet,
            "testnet" => Network::Testnet,
            other => anyhow::bail!("unknown network {other}"),
        };
        let subs = build_subscriptions(
            std::slice::from_ref(&meta.coin),
            &StreamSelection {
                all_mids: false,
                l2_book: true,
                trades: false,
            },
        );
        let mut source = WsSource::connect(network.ws_endpoint(), &subs).await?;
        info!(
            "subscribed to l2Book:{} on {}",
            meta.coin,
            network.ws_endpoint()
        );
        run_live(
            &mut harness,
            &mut source,
            Duration::from_secs(args.duration),
        )
        .await?;
    }

    let stats = harness.stats();
    if harness.ledger.records.is_empty() {
        anyhow::bail!(
            "no predictions resolved ({} snapshots seen) — feed too short for the horizons?",
            stats.snapshots_seen
        );
    }
    write_results(&harness.ledger.records, &args.out)?;

    println!(
        "snapshots={} issued={} resolved={} unresolved={}",
        stats.snapshots_seen, stats.predictions_issued, stats.resolved, stats.unresolved
    );
    println!(
        "{:>7}  {:>5}  {:>8}  {:>8}  {:>12}  {:>13}",
        "horizon", "n", "accuracy", "baseline", "up prec/rec", "down prec/rec"
    );
    for s in summarize(&harness.ledger.records) {
        println!(
            "{:>7}  {:>5}  {:>8.3}  {:>8.3}  {:>5.2}/{:<5.2}  {:>6.2}/{:<5.2}",
            s.horizon,
            s.n,
            s.accuracy,
            s.baseline,
            s.up_precision,
            s.up_recall,
            s.down_precision,
            s.down_recall
        );
    }
    println!("resolved predictions written to {}", args.out.display());
    Ok(())
}
