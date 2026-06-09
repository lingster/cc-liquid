//! `hl-recorder` CLI — records live Hyperliquid market data into a replayable
//! Parquet session for the digital twin.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use hl_recorder::client::{connect_sharded, WsSource};
use hl_recorder::config::{Network, RecordConfig};
use hl_recorder::info::fetch_perp_universe;
use hl_recorder::manifest::{Counts, Manifest, SCHEMA_VERSION};
use hl_recorder::reconnect::{ReconnectPolicy, ReconnectSource};
use hl_recorder::recorder::Recorder;
use hl_recorder::sink::EventSink;
use hl_recorder::source::EventSource;
use hl_recorder::storage::{ParquetSink, ShardedParquetSink};
use hl_recorder::subscription::{build_subscriptions, StreamSelection};
use hl_recorder::universe::validate_coins;

/// A boxed source so single-connection and sharded transports share one type.
type BoxedSource = Box<dyn EventSource + Send>;

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

    // Validate requested coins against the live universe so one bad coin (e.g. a
    // symbol not listed on Hyperliquid) can never poison the whole connection.
    let universe = fetch_perp_universe(cfg.network.info_endpoint())
        .await
        .with_context(|| {
            format!(
                "fetching perp universe from {}",
                cfg.network.info_endpoint()
            )
        })?;
    let validation = validate_coins(&cfg.coins, &universe);
    if !validation.dropped.is_empty() {
        warn!(
            "ignoring coin(s) not tradeable on {}: {:?}",
            cfg.network.as_str(),
            validation.dropped
        );
    }
    if validation.kept.is_empty() {
        anyhow::bail!(
            "none of the requested coins are on Hyperliquid {}: {:?}",
            cfg.network.as_str(),
            cfg.coins
        );
    }
    let coins = validation.kept;

    info!(
        "recording {} coin(s) on {} for {}s (shard_size={}, l2_shards={}) -> {}",
        coins.len(),
        cfg.network.as_str(),
        cfg.duration_secs,
        shard_size,
        l2_shards,
        cfg.out_dir.display()
    );

    let started_at = chrono::Utc::now();

    // A `connect` factory used for the initial connect *and* every reconnect:
    // it re-builds subscriptions each time so a fresh socket is fully resubscribed.
    let streams = cfg.streams.clone();
    let connect_coins = coins.clone();
    let connect = move || {
        let endpoint = endpoint.to_string();
        let coins = connect_coins.clone();
        let streams = streams.clone();
        async move {
            let src: BoxedSource = if shard_size > 0 {
                Box::new(connect_sharded(&endpoint, &coins, &streams, shard_size).await?)
            } else {
                let subs = build_subscriptions(&coins, &streams);
                Box::new(WsSource::connect(&endpoint, &subs).await?)
            };
            Ok::<BoxedSource, anyhow::Error>(src)
        }
    };

    // Fail fast if we cannot connect even once; otherwise wrap in a self-healing
    // source that survives transient disconnects for the full duration.
    let initial = connect()
        .await
        .with_context(|| format!("connecting to {endpoint}"))?;
    let mut source = ReconnectSource::new(initial, connect, ReconnectPolicy::default());

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
        coins: coins.clone(),
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
