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
use arrow::array::{Float64Array, Int64Array, StringArray, UInt32Array, UInt64Array};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

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
    let tables = SessionTables::scan(dir)?;
    let mut by_seq: BTreeMap<u64, RecordedEvent> = BTreeMap::new();

    for path in &tables.all_mids {
        load_all_mids(path, &mut by_seq)?;
    }
    for path in &tables.l2_book {
        load_l2_book(path, &mut by_seq)?;
    }
    for path in &tables.trades {
        load_trades(path, &mut by_seq)?;
    }

    Ok(by_seq.into_values().collect())
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
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch?);
    }
    Ok(batches)
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
