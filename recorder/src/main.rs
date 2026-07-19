//! `hl-recorder` CLI — records live Hyperliquid market data into a replayable
//! Parquet session for the digital twin.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use hl_recorder::client::{connect_sharded, WsSource};
use hl_recorder::config::RecordConfig;
use hl_recorder::crowdcent;
use hl_recorder::info::fetch_meta_body;
use hl_recorder::manifest::{AssetInfo, Counts, Manifest, SCHEMA_VERSION};
use hl_recorder::notify::DiscordNotifier;
use hl_recorder::reconnect::{ReconnectPolicy, ReconnectSource};
use hl_recorder::recorder::Recorder;
use hl_recorder::settings::{self, PartialConfig, Settings};
use hl_recorder::sink::EventSink;
use hl_recorder::source::EventSource;
use hl_recorder::storage::{self, ParquetSink, RotatingSink, ShardedParquetSink};
use hl_recorder::subscription::{build_subscriptions, StreamSelection};
use hl_recorder::universe::validate_coins;
use hl_recorder::watchdog::IdleTimeoutSource;

/// A boxed source so single-connection and sharded transports share one type.
type BoxedSource = Box<dyn EventSource + Send>;

const MANIFEST_FILE: &str = "manifest.json";

/// Record tick-by-tick Hyperliquid L2 book, mids and trades to Parquet.
///
/// Every flag is one layer over a YAML config file (`--config`, default
/// `./config.yaml` when present) which uses the same names; explicit CLI flags
/// win, then the file, then built-in defaults.
#[derive(Parser, Debug)]
#[command(name = "hl-recorder", version)]
struct Cli {
    /// YAML config file with the same keys as these flags. When omitted,
    /// `./config.yaml` is loaded if it exists.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Comma-separated coins, e.g. `BTC,ETH,SOL`. Optional when `--cc` is given.
    #[arg(long, value_delimiter = ',')]
    assets: Vec<String>,

    /// Source the coin list from the CrowdCent meta model instead of `--assets`.
    /// Downloads the challenge's consolidated meta model and records the coins of
    /// its latest release. Requires `CROWDCENT_API_KEY` (read from the
    /// environment or a `.env` file).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    cc: Option<bool>,

    /// CrowdCent challenge slug to pull the coin universe from (with `--cc`).
    #[arg(long)]
    cc_challenge: Option<String>,

    /// CrowdCent API base URL (with `--cc`).
    #[arg(long)]
    cc_url: Option<String>,

    /// Recording duration in seconds (e.g. 300 for 5 minutes). `0` (the default)
    /// records until stopped by a shutdown signal (Ctrl-C / SIGTERM), finalizing
    /// the Parquet at that point.
    #[arg(long)]
    duration: Option<u64>,

    /// Target network (mainnet|testnet). Default: mainnet.
    #[arg(long)]
    network: Option<String>,

    /// Output session directory. Default: /data/hyperliquid/sessions.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Skip the L2 book stream (mids/trades only).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_l2: Option<bool>,

    /// Skip the trades stream.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_trades: Option<bool>,

    /// Skip the all-mids stream.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_mids: Option<bool>,

    /// Coins per WebSocket connection. `0` = single connection. Use a smaller
    /// value to shard full-universe L2 capture across many connections.
    #[arg(long)]
    shard_size: Option<usize>,

    /// Number of parallel L2 Parquet part-files. `1` = single `l2_book.parquet`;
    /// higher values fan L2 writes across that many worker threads/files.
    #[arg(long)]
    l2_shards: Option<usize>,

    /// Flush buffered rows to disk at least this often (seconds). `0` disables
    /// the time-based flush (rows are still flushed at the row-count threshold
    /// and on shutdown). The Parquet footer is only written on shutdown.
    #[arg(long)]
    flush_interval: Option<u64>,

    /// Rotate output files at midnight UTC: each day's tables get a
    /// `YYYYMMDD_` prefix and are finalized (footer written) in the background
    /// while the new day records. Recommended for multi-day recordings.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    daily: Option<bool>,

    /// Treat a connection with no inbound traffic for this many seconds as
    /// dead and reconnect (a half-dead TCP link never errors on its own).
    /// `0` disables the watchdog. Default: 90.
    #[arg(long)]
    idle_timeout: Option<u64>,
}

impl Cli {
    /// This invocation's flags as one settings layer (`None` = flag not given).
    fn as_layer(&self) -> PartialConfig {
        PartialConfig {
            assets: (!self.assets.is_empty()).then(|| self.assets.clone()),
            // clap SetTrue yields Some(false) when the flag is absent; only an
            // explicit flag should override the config file, so map to None.
            cc: self.cc.filter(|&v| v),
            cc_challenge: self.cc_challenge.clone(),
            cc_url: self.cc_url.clone(),
            duration: self.duration,
            network: self.network.clone(),
            out: self.out.clone(),
            no_l2: self.no_l2.filter(|&v| v),
            no_trades: self.no_trades.filter(|&v| v),
            no_mids: self.no_mids.filter(|&v| v),
            shard_size: self.shard_size,
            l2_shards: self.l2_shards,
            flush_interval: self.flush_interval,
            daily: self.daily.filter(|&v| v),
            idle_timeout: self.idle_timeout,
        }
    }
}

fn record_config(s: &Settings) -> RecordConfig {
    RecordConfig {
        network: s.network,
        coins: s.assets.clone(),
        streams: StreamSelection {
            all_mids: !s.no_mids,
            l2_book: !s.no_l2,
            trades: !s.no_trades,
        },
        duration_secs: s.duration,
        out_dir: s.out.clone(),
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

    let cli = Cli::parse();

    // Layer resolution: CLI flags > config file > defaults. An explicit
    // `--config` must load; the default `./config.yaml` is optional.
    let file_layer = match &cli.config {
        Some(path) => settings::load_file(path)?,
        None => settings::load_file_if_exists(std::path::Path::new(
            settings::DEFAULT_CONFIG_FILE,
        ))?,
    };
    let mut s = cli.as_layer().or(file_layer).finalize()?;

    if s.cc {
        s.assets = resolve_crowdcent_coins(&s.cc_url, &s.cc_challenge).await?;
    }
    if s.assets.is_empty() {
        anyhow::bail!(
            "no coins to record: pass --assets BTC,ETH,... or --cc (flags or config.yaml)"
        );
    }

    let (shard_size, l2_shards, daily) = (s.shard_size, s.l2_shards, s.daily);
    let flush_interval = (s.flush_interval > 0).then(|| Duration::from_secs(s.flush_interval));
    let idle_timeout = (s.idle_timeout > 0).then(|| Duration::from_secs(s.idle_timeout));

    // Operator alerting for unrecoverable failures (opt-in via env/.env).
    let notifier = DiscordNotifier::from_env();
    if notifier.is_some() {
        info!("discord notifications enabled for unrecoverable errors");
    }

    let result = run(
        record_config(&s),
        shard_size,
        l2_shards,
        flush_interval,
        daily,
        idle_timeout,
    )
    .await;

    if let Err(e) = &result {
        if let Some(n) = &notifier {
            n.send(&format!(
                "🔴 **hl-recorder** exited with an unrecoverable error on `{}`:\n```\n{e:#}\n```",
                hostname()
            ))
            .await;
        }
    }
    result
}

/// Best-effort hostname for notification context.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown-host".to_string())
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
    idle_timeout: Option<Duration>,
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
            // The watchdog turns a half-dead connection (silent, never errors)
            // into a stream error so the reconnect layer can replace it.
            let src: BoxedSource = match idle_timeout {
                Some(t) => Box::new(IdleTimeoutSource::new(src, t)),
                None => src,
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
    // `run_until` finalizes the sink so the Parquet footer is written. Track
    // whether the stop future actually fired: if the run ends any other way,
    // the source was exhausted (reconnection gave up), which is unrecoverable
    // and must surface as an error, not a clean exit.
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = {
        let stopped = stopped.clone();
        let duration_secs = cfg.duration_secs;
        async move {
            wait_for_stop(duration_secs).await;
            stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    };
    let stats = Recorder::new()
        .with_flush_interval(flush_interval)
        .run_until(&mut source, &mut sink, now_ms, stop)
        .await?;
    let source_exhausted = !stopped.load(std::sync::atomic::Ordering::SeqCst);
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
    if source_exhausted {
        // The session on disk is finalized and valid, but the run did not end
        // by operator intent — exit nonzero so a supervisor restarts us.
        anyhow::bail!(
            "event source exhausted (reconnection gave up after repeated failures); \
             session finalized at {}",
            cfg.out_dir.display()
        );
    }
    Ok(())
}
