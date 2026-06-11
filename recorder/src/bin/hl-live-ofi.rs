//! hl-live-ofi — run an exported `deepofi` ONNX model (multi-horizon return
//! regressor or deepLOB-style 3-class classifier) against live or replayed
//! Hyperliquid L2 data and score its forecasts.
//!
//! ```bash
//! # Live: 5 minutes on mainnet
//! hl-live-ofi lstm.onnx --duration 300 --out lstm-live.parquet
//!
//! # Deterministic replay of a recorded session (parity testing / CI)
//! hl-live-ofi lstm.onnx --replay sessions/demo5min
//! ```

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tracing::info;

use hl_recorder::client::WsSource;
use hl_recorder::config::Network;
use hl_recorder::events::RecordedEvent;
use hl_recorder::live_ofi::harness::{run_live, run_replay, OfiClsHarness, OfiHarness, Steps};
use hl_recorder::live_ofi::model::{OfiSidecar, OnnxRegressor};
use hl_recorder::replay::load_session;
use hl_recorder::source::EventSource;
use hl_recorder::subscription::{build_subscriptions, StreamSelection};

#[derive(Parser, Debug)]
#[command(
    name = "hl-live-ofi",
    about = "Score an exported deepofi model on live or replayed L2 data"
)]
struct Args {
    /// Exported ONNX model (expects `<model>.onnx.json` sidecar next to it)
    model: PathBuf,
    /// Wall-clock seconds to run (live mode)
    #[arg(long, default_value_t = 300)]
    duration: u64,
    /// mainnet or testnet
    #[arg(long, default_value = "mainnet")]
    network: String,
    /// Replay a recorded session directory instead of connecting live
    #[arg(long)]
    replay: Option<PathBuf>,
    /// Output parquet of resolved forecasts
    #[arg(long, default_value = "live-ofi-results.parquet")]
    out: PathBuf,
}

enum Feed {
    Replay(Vec<RecordedEvent>),
    Live(Box<dyn EventSource>, Duration),
}

async fn drive<H: Steps>(harness: &mut H, feed: Feed) -> anyhow::Result<()> {
    match feed {
        Feed::Replay(events) => run_replay(harness, &events),
        Feed::Live(mut source, duration) => run_live(harness, source.as_mut(), duration).await,
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

    let meta = OfiSidecar::load_for_model(&args.model)?;
    info!(
        "model {} ({}, {}) | coin {} | levels {} window {} horizons {:?}",
        args.model.display(),
        meta.model_name,
        meta.objective.kind,
        meta.coin,
        meta.features.levels,
        meta.features.window,
        meta.features.horizons
    );
    let mut model = OnnxRegressor::load(&args.model, &meta)?;

    let feed = if let Some(session) = &args.replay {
        info!("replaying session {}", session.display());
        Feed::Replay(load_session(session)?)
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
        let source = WsSource::connect(network.ws_endpoint(), &subs).await?;
        info!(
            "subscribed to l2Book:{} on {}",
            meta.coin,
            network.ws_endpoint()
        );
        Feed::Live(Box::new(source), Duration::from_secs(args.duration))
    };

    if meta.objective.kind == "classification" {
        let mut harness = OfiClsHarness::new(&meta, &mut model)?;
        drive(&mut harness, feed).await?;
        let stats = harness.stats();
        anyhow::ensure!(
            !harness.ledger.records.is_empty(),
            "no forecasts resolved ({} snapshots seen)",
            stats.snapshots_seen
        );
        hl_recorder::live::results::write_results(&harness.ledger.records, &args.out)?;
        println!(
            "model={} snapshots={} rows={} issued={} resolved={} unresolved={}",
            meta.model_name,
            stats.snapshots_seen,
            stats.feature_rows,
            stats.predictions_issued,
            stats.resolved,
            stats.unresolved
        );
        println!(
            "{:>7}  {:>5}  {:>8}  {:>8}  {:>12}  {:>13}",
            "horizon", "n", "accuracy", "baseline", "up prec/rec", "down prec/rec"
        );
        for s in hl_recorder::live::ledger::summarize(&harness.ledger.records) {
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
    } else {
        let mut harness = OfiHarness::new(&meta, &mut model)?;
        drive(&mut harness, feed).await?;
        let stats = harness.stats();
        anyhow::ensure!(
            !harness.ledger.records.is_empty(),
            "no forecasts resolved ({} snapshots seen)",
            stats.snapshots_seen
        );
        hl_recorder::live_ofi::results::write_results(&harness.ledger.records, &args.out)?;
        println!(
            "model={} snapshots={} rows={} issued={} resolved={} unresolved={}",
            meta.model_name,
            stats.snapshots_seen,
            stats.feature_rows,
            stats.predictions_issued,
            stats.resolved,
            stats.unresolved
        );
        println!(
            "{:>7}  {:>5}  {:>10}  {:>8}  {:>9}  {:>9}",
            "horizon", "n", "mse(bps2)", "r2_os", "sign_acc", "base_rate"
        );
        for s in hl_recorder::live_ofi::ledger::summarize(&harness.ledger.records) {
            println!(
                "{:>7}  {:>5}  {:>10.4}  {:>8.4}  {:>9.4}  {:>9.4}",
                s.horizon, s.n, s.mse, s.r2_os, s.sign_accuracy, s.sign_base_rate
            );
        }
    }
    println!("resolved forecasts written to {}", args.out.display());
    Ok(())
}
