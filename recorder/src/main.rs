//! `hl-recorder` CLI — records live Hyperliquid market data into a replayable
//! Parquet session for the digital twin.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::info;

use hl_recorder::client::{connect_sharded, WsSource};
use hl_recorder::config::{Network, RecordConfig};
use hl_recorder::manifest::{Counts, Manifest, SCHEMA_VERSION};
use hl_recorder::recorder::Recorder;
use hl_recorder::sink::EventSink;
use hl_recorder::source::EventSource;
use hl_recorder::storage::{ParquetSink, ShardedParquetSink};
use hl_recorder::subscription::{build_subscriptions, StreamSelection};

const MANIFEST_FILE: &str = "manifest.json";

/// Record tick-by-tick Hyperliquid L2 book, mids and trades to Parquet.
#[derive(Parser, Debug)]
#[command(name = "hl-recorder", version)]
struct Cli {
    /// Comma-separated coins, e.g. `BTC,ETH,SOL`.
    #[arg(long, value_delimiter = ',', required = true)]
    assets: Vec<String>,

    /// Recording duration in seconds (e.g. 300 for 5 minutes).
    #[arg(long, default_value_t = 300)]
    duration: u64,

    /// Target network.
    #[arg(long, default_value = "mainnet")]
    network: Network,

    /// Output session directory.
    #[arg(long)]
    out: PathBuf,

    /// Skip the L2 book stream (mids/trades only).
    #[arg(long, default_value_t = false)]
    no_l2: bool,

    /// Skip the trades stream.
    #[arg(long, default_value_t = false)]
    no_trades: bool,

    /// Skip the all-mids stream.
    #[arg(long, default_value_t = false)]
    no_mids: bool,

    /// Coins per WebSocket connection. `0` = single connection. Use a smaller
    /// value to shard full-universe L2 capture across many connections.
    #[arg(long, default_value_t = 0)]
    shard_size: usize,

    /// Number of parallel L2 Parquet part-files. `1` = single `l2_book.parquet`;
    /// higher values fan L2 writes across that many worker threads/files.
    #[arg(long, default_value_t = 1)]
    l2_shards: usize,
}

impl Cli {
    fn into_config(self) -> RecordConfig {
        RecordConfig {
            network: self.network,
            coins: self.assets,
            streams: StreamSelection {
                all_mids: !self.no_mids,
                l2_book: !self.no_l2,
                trades: !self.no_trades,
            },
            duration_secs: self.duration,
            out_dir: self.out,
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let (shard_size, l2_shards) = (cli.shard_size, cli.l2_shards);
    run(cli.into_config(), shard_size, l2_shards).await
}

async fn run(cfg: RecordConfig, shard_size: usize, l2_shards: usize) -> anyhow::Result<()> {
    let endpoint = cfg.network.ws_endpoint();

    info!(
        "recording {} coin(s) on {} for {}s (shard_size={}, l2_shards={}) -> {}",
        cfg.coins.len(),
        cfg.network.as_str(),
        cfg.duration_secs,
        shard_size,
        l2_shards,
        cfg.out_dir.display()
    );

    let started_at = chrono::Utc::now();

    // Choose transport: single connection, or sharded across many.
    let mut source: Box<dyn EventSource + Send> = if shard_size > 0 {
        Box::new(
            connect_sharded(endpoint, &cfg.coins, &cfg.streams, shard_size)
                .await
                .with_context(|| format!("connecting (sharded) to {endpoint}"))?,
        )
    } else {
        let subs = build_subscriptions(&cfg.coins, &cfg.streams);
        Box::new(
            WsSource::connect(endpoint, &subs)
                .await
                .with_context(|| format!("connecting to {endpoint}"))?,
        )
    };

    // Choose storage: single-file, or partitioned/parallel L2.
    let mut sink: Box<dyn EventSink> = if l2_shards > 1 {
        Box::new(ShardedParquetSink::create(&cfg.out_dir, l2_shards)?)
    } else {
        Box::new(ParquetSink::create(&cfg.out_dir)?)
    };

    let deadline = tokio::time::sleep(Duration::from_secs(cfg.duration_secs));
    let stats = Recorder::new()
        .run_until(&mut source, &mut sink, now_ms, deadline)
        .await?;
    let ended_at = chrono::Utc::now();

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        network: cfg.network.as_str().to_string(),
        endpoint: endpoint.to_string(),
        coins: cfg.coins.clone(),
        streams: cfg.stream_names(),
        started_at: started_at.to_rfc3339(),
        ended_at: ended_at.to_rfc3339(),
        duration_secs: cfg.duration_secs,
        counts: Counts::from(&stats),
        recorder_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    std::fs::write(cfg.out_dir.join(MANIFEST_FILE), manifest.to_json()?)?;

    info!(
        "done: recorded={} (mids={}, l2={}, trades={}), ignored={}, errors={}",
        stats.recorded,
        stats.all_mids,
        stats.l2_book,
        stats.trades,
        stats.ignored,
        stats.parse_errors
    );
    Ok(())
}
