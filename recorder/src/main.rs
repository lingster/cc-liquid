//! `hl-recorder` CLI — records live Hyperliquid market data into a replayable
//! Parquet session for the digital twin.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use hl_recorder::client::{connect_sharded, WsSource};
use hl_recorder::config::{Network, RecordConfig};
use hl_recorder::crowdcent;
use hl_recorder::info::fetch_meta_body;
use hl_recorder::manifest::{AssetInfo, Counts, Manifest, SCHEMA_VERSION};
use hl_recorder::reconnect::{ReconnectPolicy, ReconnectSource};
use hl_recorder::recorder::Recorder;
use hl_recorder::sink::EventSink;
use hl_recorder::source::EventSource;
use hl_recorder::storage::{self, ParquetSink, RotatingSink, ShardedParquetSink};
use hl_recorder::subscription::{build_subscriptions, StreamSelection};
use hl_recorder::universe::validate_coins;

/// A boxed source so single-connection and sharded transports share one type.
type BoxedSource = Box<dyn EventSource + Send>;

const MANIFEST_FILE: &str = "manifest.json";

/// Record tick-by-tick Hyperliquid L2 book, mids and trades to Parquet.
#[derive(Parser, Debug)]
#[command(name = "hl-recorder", version)]
struct Cli {
    /// Comma-separated coins, e.g. `BTC,ETH,SOL`. Optional when `--cc` is given.
    #[arg(long, value_delimiter = ',')]
    assets: Vec<String>,

    /// Source the coin list from the CrowdCent meta model instead of `--assets`.
    /// Downloads the challenge's consolidated meta model and records the coins of
    /// its latest release. Requires `CROWDCENT_API_KEY` (read from the
    /// environment or a `.env` file).
    #[arg(long, default_value_t = false)]
    cc: bool,

    /// CrowdCent challenge slug to pull the coin universe from (with `--cc`).
    #[arg(long, default_value = crowdcent::DEFAULT_CHALLENGE_SLUG)]
    cc_challenge: String,

    /// CrowdCent API base URL (with `--cc`).
    #[arg(long, default_value = crowdcent::DEFAULT_BASE_URL)]
    cc_url: String,

    /// Recording duration in seconds (e.g. 300 for 5 minutes). `0` (the default)
    /// records until stopped by a shutdown signal (Ctrl-C / SIGTERM), finalizing
    /// the Parquet at that point.
    #[arg(long, default_value_t = 0)]
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

    /// Flush buffered rows to disk at least this often (seconds). `0` disables
    /// the time-based flush (rows are still flushed at the row-count threshold
    /// and on shutdown). The Parquet footer is only written on shutdown.
    #[arg(long, default_value_t = 300)]
    flush_interval: u64,

    /// Rotate output files at midnight UTC: each day's tables get a
    /// `YYYYMMDD_` prefix and are finalized (footer written) in the background
    /// while the new day records. Recommended for multi-day recordings.
    #[arg(long, default_value_t = false)]
    daily: bool,
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
    // Make a nearby `.env` a fallback for every env var (e.g. CROWDCENT_API_KEY,
    // RUST_LOG) before anything reads the environment. Real env vars still win.
    crowdcent::apply_dotenv_fallback();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut cli = Cli::parse();

    if cli.cc {
        cli.assets = resolve_crowdcent_coins(&cli.cc_url, &cli.cc_challenge).await?;
    }
    if cli.assets.is_empty() {
        anyhow::bail!("no coins to record: pass --assets BTC,ETH,... or --cc");
    }

    let (shard_size, l2_shards, daily) = (cli.shard_size, cli.l2_shards, cli.daily);
    let flush_interval = (cli.flush_interval > 0).then(|| Duration::from_secs(cli.flush_interval));
    run(
        cli.into_config(),
        shard_size,
        l2_shards,
        flush_interval,
        daily,
    )
    .await
}

/// Resolve when a recording session should stop: the duration deadline (when
/// set), or a graceful shutdown signal. `duration_secs == 0` means run until a
/// signal arrives. SIGKILL cannot be caught, but SIGTERM/SIGINT/SIGHUP/SIGQUIT
/// let us finalize the Parquet footer instead of leaving a truncated file.
async fn wait_for_stop(duration_secs: u64) {
    if duration_secs == 0 {
        // Unlimited: only a shutdown signal stops the session.
        let sig = wait_for_shutdown_signal().await;
        warn!("received {sig}; finalizing session (writing Parquet footer)");
        return;
    }
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(duration_secs)) =>
            info!("duration of {duration_secs}s reached; finalizing session"),
        sig = wait_for_shutdown_signal() =>
            warn!("received {sig}; finalizing session early (writing Parquet footer)"),
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    // If a handler cannot be installed we fall back to a never-resolving future
    // for that signal rather than aborting the whole recording.
    let mut term = signal(SignalKind::terminate()).ok();
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut hup = signal(SignalKind::hangup()).ok();
    let mut quit = signal(SignalKind::quit()).ok();

    async fn recv(s: &mut Option<tokio::signal::unix::Signal>) {
        match s {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending().await,
        }
    }

    tokio::select! {
        _ = recv(&mut term) => "SIGTERM",
        _ = recv(&mut int) => "SIGINT",
        _ = recv(&mut hup) => "SIGHUP",
        _ = recv(&mut quit) => "SIGQUIT",
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "Ctrl-C"
}

/// Resolve the recording coin list from the CrowdCent meta model.
async fn resolve_crowdcent_coins(cc_url: &str, cc_challenge: &str) -> anyhow::Result<Vec<String>> {
    let api_key = std::env::var(crowdcent::API_KEY_ENV_VAR).map_err(|_| {
        anyhow::anyhow!(
            "--cc requires {} (set it in the environment or a .env file)",
            crowdcent::API_KEY_ENV_VAR
        )
    })?;

    info!("fetching coin universe from CrowdCent challenge `{cc_challenge}`");
    let coins = crowdcent::fetch_crowdcent_coins(
        cc_url,
        cc_challenge,
        &api_key,
        crowdcent::DEFAULT_ID_COLUMN,
        crowdcent::DEFAULT_DATE_COLUMN,
    )
    .await
    .context("fetching CrowdCent coin universe")?;

    info!("CrowdCent meta model yielded {} coin(s)", coins.len());
    Ok(coins)
}

async fn run(
    cfg: RecordConfig,
    shard_size: usize,
    l2_shards: usize,
    flush_interval: Option<Duration>,
    daily: bool,
) -> anyhow::Result<()> {
    let endpoint = cfg.network.ws_endpoint();

    // Validate requested coins against the live universe so one bad coin (e.g. a
    // symbol not listed on Hyperliquid) can never poison the whole connection.
    // The same fetch carries per-asset `szDecimals`, recorded in the manifest so
    // consumers can derive exact tick sizes instead of inferring them.
    let meta_body = fetch_meta_body(cfg.network.info_endpoint())
        .await
        .with_context(|| {
            format!(
                "fetching perp universe from {}",
                cfg.network.info_endpoint()
            )
        })?;
    let asset_metas = hl_recorder::universe::parse_perp_assets(&meta_body)?;
    // Persist the verbatim `meta` snapshot (PRD §5.2 meta_snapshot) so twin
    // playback serves the exact universe/szDecimals seen at record time.
    std::fs::create_dir_all(&cfg.out_dir)?;
    std::fs::write(cfg.out_dir.join("meta.json"), &meta_body)?;
    let universe: std::collections::HashSet<String> = asset_metas.keys().cloned().collect();
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

    let flush_secs = flush_interval.map_or(0, |d| d.as_secs());
    let duration_label = if cfg.duration_secs == 0 {
        "until stopped".to_string()
    } else {
        format!("for {}s", cfg.duration_secs)
    };
    info!(
        "recording {} coin(s) on {} {} (shard_size={}, l2_shards={}, flush_interval={}s) -> {}",
        coins.len(),
        cfg.network.as_str(),
        duration_label,
        shard_size,
        l2_shards,
        flush_secs,
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

    // Choose storage: single-file or partitioned/parallel L2, optionally
    // wrapped in midnight-UTC daily rotation (`YYYYMMDD_` prefixed files,
    // previous day finalized on a background thread).
    let mut sink: Box<dyn EventSink> = if daily {
        let out_dir = cfg.out_dir.clone();
        Box::new(RotatingSink::new(
            move |prefix: &str| -> anyhow::Result<storage::rotating::BoxedSendSink> {
                Ok(if l2_shards > 1 {
                    Box::new(ShardedParquetSink::create_prefixed(
                        &out_dir, prefix, l2_shards,
                    )?)
                } else {
                    Box::new(ParquetSink::create_prefixed(&out_dir, prefix)?)
                })
            },
        ))
    } else if l2_shards > 1 {
        Box::new(ShardedParquetSink::create(&cfg.out_dir, l2_shards)?)
    } else {
        Box::new(ParquetSink::create(&cfg.out_dir)?)
    };

    // Stop on the duration deadline *or* a graceful shutdown signal; either way
    // `run_until` finalizes the sink so the Parquet footer is written.
    let stats = Recorder::new()
        .with_flush_interval(flush_interval)
        .run_until(
            &mut source,
            &mut sink,
            now_ms,
            wait_for_stop(cfg.duration_secs),
        )
        .await?;
    let ended_at = chrono::Utc::now();
    // Record the actual elapsed wall-clock, which is the only meaningful value
    // when running unlimited (`--duration 0`) and matches the target otherwise.
    let actual_duration_secs = (ended_at - started_at).num_seconds().max(0) as u64;

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        network: cfg.network.as_str().to_string(),
        endpoint: endpoint.to_string(),
        coins: coins.clone(),
        streams: cfg.stream_names(),
        started_at: started_at.to_rfc3339(),
        ended_at: ended_at.to_rfc3339(),
        duration_secs: actual_duration_secs,
        counts: Counts::from(&stats),
        recorder_version: env!("CARGO_PKG_VERSION").to_string(),
        daily,
        assets: coins
            .iter()
            .filter_map(|c| asset_metas.get(c).map(|m| (c.clone(), AssetInfo::from(*m))))
            .collect(),
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
