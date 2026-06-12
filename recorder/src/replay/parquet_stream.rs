//! Loads a recorded Parquet session back into ordered [`RecordedEvent`]s.
//!
//! This is the inverse of [`crate::storage::ParquetSink`]: it reads the three
//! exploded tables and reassembles the original events, grouping rows by `seq`
//! and merging all streams into one ascending-`seq` sequence — exactly what the
//! replay engine consumes.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use arrow::array::{
    Array, BooleanArray, Float64Array, Int64Array, StringArray, UInt32Array, UInt64Array,
};
use arrow::error::ArrowError;
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::{
    ArrowPredicateFn, ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder, RowFilter,
};
use parquet::arrow::ProjectionMask;
use parquet::file::statistics::Statistics;

use crate::events::{AllMids, L2Book, Level, MarketEvent, RecordedEvent, Side, Trade};
use crate::replay::stream::VecEventStream;
use crate::storage::tables::{ALL_MIDS_FILE, L2_BOOK_DIR, L2_BOOK_FILE, TRADES_FILE};

/// Load a session directory into events ordered by `seq`.
///
/// Supports every sink layout: single-file tables, a partitioned
/// `l2_book/part-*.parquet` directory (sharded sink), and daily-rotated
/// sessions where each table carries a `YYYYMMDD_` prefix (`--daily`). All
/// matching files are merged; `seq` is globally monotonic across days, so the
/// merge reproduces the original event order.
pub fn load_session(dir: impl AsRef<Path>) -> anyhow::Result<Vec<RecordedEvent>> {
    let dir = dir.as_ref();
    let started = std::time::Instant::now();
    let tables = SessionTables::scan(dir)?;
    tracing::info!(
        dir = %dir.display(),
        all_mids = tables.all_mids.len(),
        l2_book = tables.l2_book.len(),
        trades = tables.trades.len(),
        "scanning session tables; decoding into memory"
    );
    // NOTE: this materialises every event into RAM (see by_seq below); there is
    // no streaming. Very large sessions (multi-GB l2_book) are correspondingly
    // memory- and time-heavy — the per-file debug logs show where time goes.
    let mut by_seq: BTreeMap<u64, RecordedEvent> = BTreeMap::new();

    for path in &tables.all_mids {
        load_all_mids(path, &mut by_seq)?;
    }
    for path in &tables.l2_book {
        let t = std::time::Instant::now();
        load_l2_book(path, &mut by_seq)?;
        tracing::debug!(
            file = %path.display(),
            events_so_far = by_seq.len(),
            elapsed_ms = t.elapsed().as_millis() as u64,
            "decoded l2_book file"
        );
    }
    for path in &tables.trades {
        load_trades(path, &mut by_seq)?;
    }

    let events: Vec<RecordedEvent> = by_seq.into_values().collect();
    tracing::info!(
        events = events.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "session decode complete"
    );
    Ok(events)
}

/// List the coins available in a session, cheaply.
///
/// Prefers the `coins` array in `manifest.json` (no file scan); falls back to
/// streaming the `l2_book` table and collecting the distinct `coin` column when
/// the manifest is absent or carries no coins. Result is sorted and de-duped.
/// This lets the viewer populate its coin list without materialising any books
/// (see lazy per-coin loading).
pub fn list_session_coins(dir: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let dir = dir.as_ref();

    // Fast path: manifest.json's coins array.
    #[derive(serde::Deserialize)]
    struct CoinsOnly {
        #[serde(default)]
        coins: Vec<String>,
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("manifest.json")) {
        if let Ok(m) = serde_json::from_str::<CoinsOnly>(&text) {
            if !m.coins.is_empty() {
                let mut coins = m.coins;
                coins.sort();
                coins.dedup();
                tracing::info!(coins = coins.len(), "coin list from manifest");
                return Ok(coins);
            }
        }
    }

    // Fallback: scan the l2_book table for distinct coins (names only, no books).
    let tables = SessionTables::scan(dir)?;
    let mut set = std::collections::BTreeSet::new();
    for path in &tables.l2_book {
        // Stream batch-by-batch so the full table is never resident at once.
        for batch in open_reader(path)? {
            let batch = batch?;
            let coin = col::<StringArray>(&batch, "coin")?;
            for i in 0..batch.num_rows() {
                if !set.contains(coin.value(i)) {
                    set.insert(coin.value(i).to_string());
                }
            }
        }
    }
    tracing::info!(coins = set.len(), "coin list from l2_book scan");
    Ok(set.into_iter().collect())
}

/// The session's wall-clock span as inclusive `(start_ms, end_ms)` unix millis,
/// read from `manifest.json`'s RFC3339 `started_at`/`ended_at`. Returns `None`
/// when the manifest is absent or the timestamps can't be parsed (the caller
/// then falls back to the loaded coin's own time range). Used to scale the UI's
/// window range slider without scanning the data.
pub fn session_time_span(dir: impl AsRef<Path>) -> Option<(i64, i64)> {
    #[derive(serde::Deserialize)]
    struct Span {
        started_at: String,
        ended_at: String,
    }
    let text = std::fs::read_to_string(dir.as_ref().join("manifest.json")).ok()?;
    let span: Span = serde_json::from_str(&text).ok()?;
    let parse = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp_millis())
    };
    match (parse(&span.started_at), parse(&span.ended_at)) {
        (Some(a), Some(b)) if a <= b => Some((a, b)),
        _ => None,
    }
}

/// Load just one coin's L2 book events from a session, ordered by `seq`.
///
/// Streams the `l2_book` table and keeps only rows for `coin`, so peak memory
/// is bounded by that single coin's books rather than the whole session. This
/// is the per-coin half of the viewer's lazy loading: the order book, depth
/// chart and price series for the selected coin are derived from these events.
///
/// `window` (inclusive `[start_ms, end_ms]` on `ts_event_ms`) restricts the
/// load to a time slice — e.g. a single 24h period. Because the table is
/// written in time order, row groups whose `ts_event_ms` range falls outside
/// the window are skipped via parquet statistics, so a window load is both
/// smaller *and* faster than a full coin load. `None` loads the whole coin.
pub fn load_coin_session(
    dir: impl AsRef<Path>,
    coin: &str,
    window: Option<(i64, i64)>,
) -> anyhow::Result<Vec<RecordedEvent>> {
    let dir = dir.as_ref();
    let started = std::time::Instant::now();
    let tables = SessionTables::scan(dir)?;
    // Only this coin's seqs are retained, so the map stays small.
    let mut acc: HashMap<u64, BookAccum> = HashMap::new();
    for path in &tables.l2_book {
        // Predicate pushdown filters to this coin during decode; row-group
        // pruning skips chunks outside the window; batches are streamed and
        // dropped, so peak memory is bounded by one coin's windowed books.
        for batch in open_coin_reader(path, coin, window)? {
            let batch = batch?;
            let seq = col::<UInt64Array>(&batch, "seq")?;
            let te = col::<Int64Array>(&batch, "ts_event_ms")?;
            let tr = col::<Int64Array>(&batch, "ts_recv_ms")?;
            let coin_col = col::<StringArray>(&batch, "coin")?;
            let side = col::<StringArray>(&batch, "side")?;
            let level_idx = col::<UInt32Array>(&batch, "level_idx")?;
            let px = col::<Float64Array>(&batch, "px")?;
            let sz = col::<Float64Array>(&batch, "sz")?;
            let n = col::<UInt32Array>(&batch, "n")?;
            for i in 0..batch.num_rows() {
                if coin_col.value(i) != coin {
                    continue;
                }
                // Row-group pruning is coarse; enforce the exact window here.
                if let Some((lo, hi)) = window {
                    let ts = te.value(i);
                    if ts < lo || ts > hi {
                        continue;
                    }
                }
                let accum = acc.entry(seq.value(i)).or_insert_with(|| BookAccum {
                    common: Common {
                        ts_event_ms: te.value(i),
                        ts_recv_ms: tr.value(i),
                    },
                    coin: coin.to_string(),
                    bids: Vec::new(),
                    asks: Vec::new(),
                });
                let level = Level {
                    px: px.value(i),
                    sz: sz.value(i),
                    n: n.value(i),
                };
                match side.value(i) {
                    "bid" => accum.bids.push((level_idx.value(i), level)),
                    "ask" => accum.asks.push((level_idx.value(i), level)),
                    other => return Err(anyhow!("unknown book side `{other}`")),
                }
            }
        }
    }

    // Order by seq and rebuild best-first books.
    let mut by_seq: BTreeMap<u64, RecordedEvent> = BTreeMap::new();
    for (seq, mut accum) in acc {
        accum.bids.sort_by_key(|(idx, _)| *idx);
        accum.asks.sort_by_key(|(idx, _)| *idx);
        by_seq.insert(
            seq,
            RecordedEvent {
                seq,
                ts_event_ms: accum.common.ts_event_ms,
                ts_recv_ms: accum.common.ts_recv_ms,
                payload: MarketEvent::L2Book(L2Book {
                    coin: accum.coin,
                    time_ms: accum.common.ts_event_ms,
                    bids: accum.bids.into_iter().map(|(_, l)| l).collect(),
                    asks: accum.asks.into_iter().map(|(_, l)| l).collect(),
                }),
            },
        );
    }
    let events: Vec<RecordedEvent> = by_seq.into_values().collect();
    tracing::info!(
        coin,
        events = events.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "loaded single coin session"
    );
    Ok(events)
}

/// The resolved table files of a session, across all supported layouts.
struct SessionTables {
    all_mids: Vec<PathBuf>,
    l2_book: Vec<PathBuf>,
    trades: Vec<PathBuf>,
}

impl SessionTables {
    fn scan(dir: &Path) -> anyhow::Result<Self> {
        let mut tables = Self {
            all_mids: Vec::new(),
            l2_book: Vec::new(),
            trades: Vec::new(),
        };
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("reading session dir {}", dir.display()))?
        {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if path.is_dir() && name.ends_with(L2_BOOK_DIR) {
                // `l2_book/` or `YYYYMMDD_l2_book/`: collect its part-files.
                let mut parts: Vec<PathBuf> = std::fs::read_dir(&path)?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "parquet"))
                    .collect();
                parts.sort();
                tables.l2_book.extend(parts);
            } else if name.ends_with(ALL_MIDS_FILE) {
                tables.all_mids.push(path);
            } else if name.ends_with(L2_BOOK_FILE) {
                tables.l2_book.push(path);
            } else if name.ends_with(TRADES_FILE) {
                tables.trades.push(path);
            }
        }
        if tables.all_mids.is_empty() && tables.l2_book.is_empty() && tables.trades.is_empty() {
            anyhow::bail!("no session tables found in {}", dir.display());
        }
        tables.all_mids.sort();
        tables.l2_book.sort();
        tables.trades.sort();
        Ok(tables)
    }
}

/// Convenience: load a session straight into an [`EventStream`].
pub fn load_session_stream(dir: impl AsRef<Path>) -> anyhow::Result<VecEventStream> {
    Ok(VecEventStream::new(load_session(dir)?))
}

fn read_batches(path: &Path) -> anyhow::Result<Vec<RecordBatch>> {
    let mut batches = Vec::new();
    for batch in open_reader(path)? {
        batches.push(batch?);
    }
    Ok(batches)
}

/// Open a streaming batch reader over a parquet file. Iterating decodes one
/// batch (row group chunk) at a time, so callers that process-and-drop each
/// batch never hold the whole file in memory — essential for the multi-GB
/// `l2_book` table (see lazy per-coin loading).
fn open_reader(path: &Path) -> anyhow::Result<ParquetRecordBatchReader> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(ParquetRecordBatchReaderBuilder::try_new(file)?.build()?)
}

/// Open a streaming reader that pushes a `coin == target` predicate down into
/// parquet, and (when `window` is set) skips row groups whose `ts_event_ms`
/// range lies entirely outside the window. The `coin` column is decoded first
/// to build a row mask, and the remaining (wide) columns are only materialised
/// for matching rows — so extracting one coin/window from a table with all
/// coins interleaved skips the bulk of the decode work, not just allocation.
fn open_coin_reader(
    path: &Path,
    coin: &str,
    window: Option<(i64, i64)>,
) -> anyhow::Result<ParquetRecordBatchReader> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

    // Leaf indices (flat schema ⇒ field index == leaf index).
    let leaf_idx = |name: &str| {
        builder
            .schema()
            .fields()
            .iter()
            .position(|f| f.name() == name)
    };
    let coin_idx =
        leaf_idx("coin").ok_or_else(|| anyhow!("l2_book table is missing a `coin` column"))?;

    // Row-group pruning by ts_event_ms statistics (table is time-ordered).
    if let (Some((lo, hi)), Some(ts_idx)) = (window, leaf_idx("ts_event_ms")) {
        let meta = builder.metadata();
        let mut keep = Vec::new();
        for rg in 0..meta.num_row_groups() {
            let stats = meta.row_group(rg).column(ts_idx).statistics().cloned();
            let overlaps = match stats {
                Some(Statistics::Int64(s)) => match (s.min_opt(), s.max_opt()) {
                    (Some(&min), Some(&max)) => max >= lo && min <= hi,
                    _ => true, // missing min/max: keep to be safe
                },
                _ => true, // no/other stats: keep
            };
            if overlaps {
                keep.push(rg);
            }
        }
        builder = builder.with_row_groups(keep);
    }

    let mask = ProjectionMask::leaves(builder.parquet_schema(), [coin_idx]);
    let target = coin.to_string();
    let predicate = ArrowPredicateFn::new(mask, move |batch: RecordBatch| {
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ArrowError::CastError("coin column is not Utf8".into()))?;
        Ok(BooleanArray::from_iter(
            (0..col.len()).map(|i| Some(col.value(i) == target)),
        ))
    });
    let reader = builder
        .with_row_filter(RowFilter::new(vec![Box::new(predicate)]))
        .build()?;
    Ok(reader)
}

fn col<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a T> {
    batch
        .column_by_name(name)
        .ok_or_else(|| anyhow!("missing column `{name}`"))?
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| anyhow!("column `{name}` has unexpected type"))
}

/// Accumulator preserving the common per-event fields keyed by `seq`.
struct Common {
    ts_event_ms: i64,
    ts_recv_ms: i64,
}

fn load_all_mids(path: &Path, out: &mut BTreeMap<u64, RecordedEvent>) -> anyhow::Result<()> {
    let mut acc: HashMap<u64, (Common, Vec<(String, f64)>)> = HashMap::new();
    for batch in read_batches(path)? {
        let seq = col::<UInt64Array>(&batch, "seq")?;
        let te = col::<Int64Array>(&batch, "ts_event_ms")?;
        let tr = col::<Int64Array>(&batch, "ts_recv_ms")?;
        let coin = col::<StringArray>(&batch, "coin")?;
        let mid = col::<Float64Array>(&batch, "mid")?;
        for i in 0..batch.num_rows() {
            let entry = acc.entry(seq.value(i)).or_insert_with(|| {
                (
                    Common {
                        ts_event_ms: te.value(i),
                        ts_recv_ms: tr.value(i),
                    },
                    Vec::new(),
                )
            });
            entry.1.push((coin.value(i).to_string(), mid.value(i)));
        }
    }
    for (seq, (common, mut mids)) in acc {
        mids.sort_by(|a, b| a.0.cmp(&b.0));
        out.insert(
            seq,
            RecordedEvent {
                seq,
                ts_event_ms: common.ts_event_ms,
                ts_recv_ms: common.ts_recv_ms,
                payload: MarketEvent::AllMids(AllMids { mids }),
            },
        );
    }
    Ok(())
}

/// Per-`seq` book accumulator: levels tagged with their original index so the
/// best-first ordering survives the round-trip.
struct BookAccum {
    common: Common,
    coin: String,
    bids: Vec<(u32, Level)>,
    asks: Vec<(u32, Level)>,
}

fn load_l2_book(path: &Path, out: &mut BTreeMap<u64, RecordedEvent>) -> anyhow::Result<()> {
    let mut acc: HashMap<u64, BookAccum> = HashMap::new();
    for batch in read_batches(path)? {
        let seq = col::<UInt64Array>(&batch, "seq")?;
        let te = col::<Int64Array>(&batch, "ts_event_ms")?;
        let tr = col::<Int64Array>(&batch, "ts_recv_ms")?;
        let coin = col::<StringArray>(&batch, "coin")?;
        let side = col::<StringArray>(&batch, "side")?;
        let level_idx = col::<UInt32Array>(&batch, "level_idx")?;
        let px = col::<Float64Array>(&batch, "px")?;
        let sz = col::<Float64Array>(&batch, "sz")?;
        let n = col::<UInt32Array>(&batch, "n")?;
        for i in 0..batch.num_rows() {
            let accum = acc.entry(seq.value(i)).or_insert_with(|| BookAccum {
                common: Common {
                    ts_event_ms: te.value(i),
                    ts_recv_ms: tr.value(i),
                },
                coin: coin.value(i).to_string(),
                bids: Vec::new(),
                asks: Vec::new(),
            });
            let level = Level {
                px: px.value(i),
                sz: sz.value(i),
                n: n.value(i),
            };
            match side.value(i) {
                "bid" => accum.bids.push((level_idx.value(i), level)),
                "ask" => accum.asks.push((level_idx.value(i), level)),
                other => return Err(anyhow!("unknown book side `{other}`")),
            }
        }
    }
    for (seq, mut accum) in acc {
        accum.bids.sort_by_key(|(idx, _)| *idx);
        accum.asks.sort_by_key(|(idx, _)| *idx);
        out.insert(
            seq,
            RecordedEvent {
                seq,
                ts_event_ms: accum.common.ts_event_ms,
                ts_recv_ms: accum.common.ts_recv_ms,
                payload: MarketEvent::L2Book(L2Book {
                    coin: accum.coin,
                    time_ms: accum.common.ts_event_ms,
                    bids: accum.bids.into_iter().map(|(_, l)| l).collect(),
                    asks: accum.asks.into_iter().map(|(_, l)| l).collect(),
                }),
            },
        );
    }
    Ok(())
}

fn load_trades(path: &Path, out: &mut BTreeMap<u64, RecordedEvent>) -> anyhow::Result<()> {
    let mut acc: HashMap<u64, (Common, Vec<Trade>)> = HashMap::new();
    for batch in read_batches(path)? {
        let seq = col::<UInt64Array>(&batch, "seq")?;
        let te = col::<Int64Array>(&batch, "ts_event_ms")?;
        let tr = col::<Int64Array>(&batch, "ts_recv_ms")?;
        let coin = col::<StringArray>(&batch, "coin")?;
        let side = col::<StringArray>(&batch, "side")?;
        let px = col::<Float64Array>(&batch, "px")?;
        let sz = col::<Float64Array>(&batch, "sz")?;
        let trade_time = col::<Int64Array>(&batch, "trade_time_ms")?;
        for i in 0..batch.num_rows() {
            let entry = acc.entry(seq.value(i)).or_insert_with(|| {
                (
                    Common {
                        ts_event_ms: te.value(i),
                        ts_recv_ms: tr.value(i),
                    },
                    Vec::new(),
                )
            });
            let side = match side.value(i) {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                other => return Err(anyhow!("unknown trade side `{other}`")),
            };
            entry.1.push(Trade {
                coin: coin.value(i).to_string(),
                side,
                px: px.value(i),
                sz: sz.value(i),
                time_ms: trade_time.value(i),
            });
        }
    }
    for (seq, (common, trades)) in acc {
        out.insert(
            seq,
            RecordedEvent {
                seq,
                ts_event_ms: common.ts_event_ms,
                ts_recv_ms: common.ts_recv_ms,
                payload: MarketEvent::Trades(trades),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Level, MarketEvent, Side, Trade};
    use crate::sink::EventSink;
    use crate::storage::ParquetSink;

    fn rec(seq: u64, payload: MarketEvent) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 2000 + seq as i64,
            payload,
        }
    }

    fn write_session(dir: &Path, events: &[RecordedEvent]) {
        let mut sink = ParquetSink::create(dir).unwrap();
        for e in events {
            sink.write(e).unwrap();
        }
        sink.finalize().unwrap();
    }

    #[test]
    fn round_trips_a_mixed_session_in_seq_order() {
        let dir = tempfile::tempdir().unwrap();
        let events = vec![
            rec(
                0,
                MarketEvent::AllMids(AllMids {
                    mids: vec![("BTC".into(), 95000.0), ("ETH".into(), 3200.0)],
                }),
            ),
            rec(
                1,
                MarketEvent::L2Book(L2Book {
                    coin: "BTC".into(),
                    time_ms: 1001,
                    bids: vec![
                        Level {
                            px: 100.0,
                            sz: 1.0,
                            n: 2,
                        },
                        Level {
                            px: 99.0,
                            sz: 3.0,
                            n: 1,
                        },
                    ],
                    asks: vec![Level {
                        px: 101.0,
                        sz: 2.0,
                        n: 1,
                    }],
                }),
            ),
            rec(
                2,
                MarketEvent::Trades(vec![Trade {
                    coin: "BTC".into(),
                    side: Side::Buy,
                    px: 100.5,
                    sz: 0.4,
                    time_ms: 1002,
                }]),
            ),
        ];
        write_session(dir.path(), &events);

        let loaded = load_session(dir.path()).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].seq, 0);
        assert_eq!(loaded[1].seq, 1);
        assert_eq!(loaded[2].seq, 2);

        // L2 book preserves best-first level ordering.
        match &loaded[1].payload {
            MarketEvent::L2Book(b) => {
                assert_eq!(b.bids[0].px, 100.0);
                assert_eq!(b.bids[1].px, 99.0);
                assert_eq!(b.asks[0].px, 101.0);
            }
            _ => panic!("expected L2Book at seq 1"),
        }
        // Trades round-trip with side.
        match &loaded[2].payload {
            MarketEvent::Trades(ts) => {
                assert_eq!(ts.len(), 1);
                assert_eq!(ts[0].side, Side::Buy);
                assert_eq!(ts[0].px, 100.5);
            }
            _ => panic!("expected Trades at seq 2"),
        }
    }

    #[test]
    fn empty_session_loads_to_no_events() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path(), &[]);
        let loaded = load_session(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn missing_session_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_session(dir.path().join("nope")).is_err());
        // An existing-but-empty dir has no tables -> error, not silence.
        assert!(load_session(dir.path()).is_err());
    }

    #[test]
    fn daily_rotated_session_loads_across_day_files() {
        use crate::storage::rotating::{BoxedSendSink, RotatingSink};
        use crate::storage::ShardedParquetSink;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().to_path_buf();
        let mut sink = RotatingSink::new(move |prefix: &str| -> anyhow::Result<BoxedSendSink> {
            Ok(Box::new(ShardedParquetSink::create_prefixed(
                &out, prefix, 2,
            )?))
        });

        // Two events on 2026-06-10, one after midnight UTC on 2026-06-11.
        const DAY1: i64 = 1781135999000;
        const DAY2: i64 = 1781136000500;
        let mk = |seq: u64, ts: i64, coin: &str| RecordedEvent {
            seq,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::L2Book(L2Book {
                coin: coin.into(),
                time_ms: ts,
                bids: vec![Level {
                    px: 99.0,
                    sz: 1.0,
                    n: 1,
                }],
                asks: vec![Level {
                    px: 101.0,
                    sz: 1.0,
                    n: 1,
                }],
            }),
        };
        sink.write(&mk(0, DAY1, "BTC")).unwrap();
        sink.write(&mk(1, DAY1 + 1, "ETH")).unwrap();
        sink.write(&mk(2, DAY2, "BTC")).unwrap();
        sink.finalize().unwrap();

        // Each day produced its own prefixed tables...
        assert!(dir.path().join("20260610_l2_book").is_dir());
        assert!(dir.path().join("20260611_l2_book").is_dir());
        assert!(dir.path().join("20260610_all_mids.parquet").is_file());
        assert!(dir.path().join("20260611_trades.parquet").is_file());

        // ...and the loader merges them back into one ordered event stream.
        let loaded = load_session(dir.path()).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(
            loaded.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(loaded[2].ts_recv_ms, DAY2);
    }
}
