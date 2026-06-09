//! Parquet implementation of [`EventSink`].
//!
//! Events are written to three flat ("exploded") tables in a session directory,
//! as specified in `PRD_HYPERLIQUID_DIGITAL_TWIN.md` §5.4:
//!
//! - `all_mids.parquet` — one row per (event, coin) mid price.
//! - `l2_book.parquet`   — one row per book level (`side`, `level_idx`, px/sz/n).
//! - `trades.parquet`    — one row per trade print.
//!
//! Common columns (`seq`, `ts_event_ms`, `ts_recv_ms`, `coin`) are shared across
//! tables so a replay engine can fold events back into market state by `seq`.
//! Row groups are flushed periodically to bound memory and limit data loss on
//! crash.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::events::{MarketEvent, RecordedEvent};
use crate::sink::EventSink;

const DEFAULT_FLUSH_THRESHOLD: usize = 10_000;

pub const ALL_MIDS_FILE: &str = "all_mids.parquet";
pub const L2_BOOK_FILE: &str = "l2_book.parquet";
pub const TRADES_FILE: &str = "trades.parquet";

fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build()
}

/// Manages a single Parquet file: lazily opens on first write, guarantees the
/// file exists (with schema) even when no rows were produced.
struct TableFile {
    path: PathBuf,
    schema: Arc<Schema>,
    writer: Option<ArrowWriter<File>>,
}

impl TableFile {
    fn new(path: PathBuf, schema: Arc<Schema>) -> Self {
        Self {
            path,
            schema,
            writer: None,
        }
    }

    fn write_batch(&mut self, batch: &RecordBatch) -> anyhow::Result<()> {
        if self.writer.is_none() {
            let file = File::create(&self.path)?;
            self.writer = Some(ArrowWriter::try_new(
                file,
                self.schema.clone(),
                Some(writer_props()),
            )?);
        }
        self.writer.as_mut().unwrap().write(batch)?;
        Ok(())
    }

    /// Close the file, ensuring it exists with at least an (empty) schema.
    fn close(&mut self) -> anyhow::Result<()> {
        if self.writer.is_none() {
            let empty = RecordBatch::new_empty(self.schema.clone());
            self.write_batch(&empty)?;
        }
        if let Some(w) = self.writer.take() {
            w.close()?;
        }
        Ok(())
    }
}

/// Column buffers for the `all_mids` table.
#[derive(Default)]
struct MidsBuffer {
    seq: Vec<u64>,
    ts_event_ms: Vec<i64>,
    ts_recv_ms: Vec<i64>,
    coin: Vec<String>,
    mid: Vec<f64>,
}

impl MidsBuffer {
    fn len(&self) -> usize {
        self.seq.len()
    }
    fn schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("seq", DataType::UInt64, false),
            Field::new("ts_event_ms", DataType::Int64, false),
            Field::new("ts_recv_ms", DataType::Int64, false),
            Field::new("coin", DataType::Utf8, false),
            Field::new("mid", DataType::Float64, false),
        ]))
    }
    fn drain_to_batch(&mut self) -> anyhow::Result<RecordBatch> {
        let cols: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from(std::mem::take(&mut self.seq))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_event_ms))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_recv_ms))),
            Arc::new(StringArray::from(std::mem::take(&mut self.coin))),
            Arc::new(Float64Array::from(std::mem::take(&mut self.mid))),
        ];
        Ok(RecordBatch::try_new(Self::schema(), cols)?)
    }
}

/// Column buffers for the `l2_book` table.
#[derive(Default)]
struct BookBuffer {
    seq: Vec<u64>,
    ts_event_ms: Vec<i64>,
    ts_recv_ms: Vec<i64>,
    coin: Vec<String>,
    side: Vec<String>,
    level_idx: Vec<u32>,
    px: Vec<f64>,
    sz: Vec<f64>,
    n: Vec<u32>,
}

impl BookBuffer {
    fn len(&self) -> usize {
        self.seq.len()
    }
    fn schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("seq", DataType::UInt64, false),
            Field::new("ts_event_ms", DataType::Int64, false),
            Field::new("ts_recv_ms", DataType::Int64, false),
            Field::new("coin", DataType::Utf8, false),
            Field::new("side", DataType::Utf8, false),
            Field::new("level_idx", DataType::UInt32, false),
            Field::new("px", DataType::Float64, false),
            Field::new("sz", DataType::Float64, false),
            Field::new("n", DataType::UInt32, false),
        ]))
    }
    fn drain_to_batch(&mut self) -> anyhow::Result<RecordBatch> {
        let cols: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from(std::mem::take(&mut self.seq))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_event_ms))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_recv_ms))),
            Arc::new(StringArray::from(std::mem::take(&mut self.coin))),
            Arc::new(StringArray::from(std::mem::take(&mut self.side))),
            Arc::new(UInt32Array::from(std::mem::take(&mut self.level_idx))),
            Arc::new(Float64Array::from(std::mem::take(&mut self.px))),
            Arc::new(Float64Array::from(std::mem::take(&mut self.sz))),
            Arc::new(UInt32Array::from(std::mem::take(&mut self.n))),
        ];
        Ok(RecordBatch::try_new(Self::schema(), cols)?)
    }
}

/// Column buffers for the `trades` table.
#[derive(Default)]
struct TradesBuffer {
    seq: Vec<u64>,
    ts_event_ms: Vec<i64>,
    ts_recv_ms: Vec<i64>,
    coin: Vec<String>,
    side: Vec<String>,
    px: Vec<f64>,
    sz: Vec<f64>,
    trade_time_ms: Vec<i64>,
}

impl TradesBuffer {
    fn len(&self) -> usize {
        self.seq.len()
    }
    fn schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("seq", DataType::UInt64, false),
            Field::new("ts_event_ms", DataType::Int64, false),
            Field::new("ts_recv_ms", DataType::Int64, false),
            Field::new("coin", DataType::Utf8, false),
            Field::new("side", DataType::Utf8, false),
            Field::new("px", DataType::Float64, false),
            Field::new("sz", DataType::Float64, false),
            Field::new("trade_time_ms", DataType::Int64, false),
        ]))
    }
    fn drain_to_batch(&mut self) -> anyhow::Result<RecordBatch> {
        let cols: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from(std::mem::take(&mut self.seq))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_event_ms))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.ts_recv_ms))),
            Arc::new(StringArray::from(std::mem::take(&mut self.coin))),
            Arc::new(StringArray::from(std::mem::take(&mut self.side))),
            Arc::new(Float64Array::from(std::mem::take(&mut self.px))),
            Arc::new(Float64Array::from(std::mem::take(&mut self.sz))),
            Arc::new(Int64Array::from(std::mem::take(&mut self.trade_time_ms))),
        ];
        Ok(RecordBatch::try_new(Self::schema(), cols)?)
    }
}

/// Writes recorded events to a session directory as three Parquet tables.
pub struct ParquetSink {
    flush_threshold: usize,
    mids_buf: MidsBuffer,
    book_buf: BookBuffer,
    trades_buf: TradesBuffer,
    mids_file: TableFile,
    book_file: TableFile,
    trades_file: TableFile,
}

impl ParquetSink {
    /// Create a sink writing into `dir` (created if missing).
    pub fn create(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::with_flush_threshold(dir, DEFAULT_FLUSH_THRESHOLD)
    }

    pub fn with_flush_threshold(
        dir: impl AsRef<Path>,
        flush_threshold: usize,
    ) -> anyhow::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            flush_threshold: flush_threshold.max(1),
            mids_buf: MidsBuffer::default(),
            book_buf: BookBuffer::default(),
            trades_buf: TradesBuffer::default(),
            mids_file: TableFile::new(dir.join(ALL_MIDS_FILE), MidsBuffer::schema()),
            book_file: TableFile::new(dir.join(L2_BOOK_FILE), BookBuffer::schema()),
            trades_file: TableFile::new(dir.join(TRADES_FILE), TradesBuffer::schema()),
        })
    }

    fn append(&mut self, event: &RecordedEvent) {
        let (seq, te, tr) = (event.seq, event.ts_event_ms, event.ts_recv_ms);
        match &event.payload {
            MarketEvent::AllMids(m) => {
                for (coin, mid) in &m.mids {
                    self.mids_buf.seq.push(seq);
                    self.mids_buf.ts_event_ms.push(te);
                    self.mids_buf.ts_recv_ms.push(tr);
                    self.mids_buf.coin.push(coin.clone());
                    self.mids_buf.mid.push(*mid);
                }
            }
            MarketEvent::L2Book(b) => {
                for (side, levels) in [("bid", &b.bids), ("ask", &b.asks)] {
                    for (idx, lvl) in levels.iter().enumerate() {
                        self.book_buf.seq.push(seq);
                        self.book_buf.ts_event_ms.push(te);
                        self.book_buf.ts_recv_ms.push(tr);
                        self.book_buf.coin.push(b.coin.clone());
                        self.book_buf.side.push(side.to_string());
                        self.book_buf.level_idx.push(idx as u32);
                        self.book_buf.px.push(lvl.px);
                        self.book_buf.sz.push(lvl.sz);
                        self.book_buf.n.push(lvl.n);
                    }
                }
            }
            MarketEvent::Trades(ts) => {
                for t in ts {
                    self.trades_buf.seq.push(seq);
                    self.trades_buf.ts_event_ms.push(te);
                    self.trades_buf.ts_recv_ms.push(tr);
                    self.trades_buf.coin.push(t.coin.clone());
                    self.trades_buf.side.push(t.side.as_str().to_string());
                    self.trades_buf.px.push(t.px);
                    self.trades_buf.sz.push(t.sz);
                    self.trades_buf.trade_time_ms.push(t.time_ms);
                }
            }
        }
    }

    fn flush_full_buffers(&mut self) -> anyhow::Result<()> {
        if self.mids_buf.len() >= self.flush_threshold {
            let batch = self.mids_buf.drain_to_batch()?;
            self.mids_file.write_batch(&batch)?;
        }
        if self.book_buf.len() >= self.flush_threshold {
            let batch = self.book_buf.drain_to_batch()?;
            self.book_file.write_batch(&batch)?;
        }
        if self.trades_buf.len() >= self.flush_threshold {
            let batch = self.trades_buf.drain_to_batch()?;
            self.trades_file.write_batch(&batch)?;
        }
        Ok(())
    }
}

impl EventSink for ParquetSink {
    fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
        self.append(event);
        self.flush_full_buffers()
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        if self.mids_buf.len() > 0 {
            let batch = self.mids_buf.drain_to_batch()?;
            self.mids_file.write_batch(&batch)?;
        }
        if self.book_buf.len() > 0 {
            let batch = self.book_buf.drain_to_batch()?;
            self.book_file.write_batch(&batch)?;
        }
        if self.trades_buf.len() > 0 {
            let batch = self.trades_buf.drain_to_batch()?;
            self.trades_file.write_batch(&batch)?;
        }
        self.mids_file.close()?;
        self.book_file.close()?;
        self.trades_file.close()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Level, MarketEvent, Side, Trade};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn read_row_count(path: &Path) -> usize {
        let file = File::open(path).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        reader.map(|b| b.unwrap().num_rows()).sum()
    }

    fn rec(seq: u64, payload: MarketEvent) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 2000 + seq as i64,
            payload,
        }
    }

    #[test]
    fn explodes_events_into_three_tables() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = ParquetSink::create(dir.path()).unwrap();

        sink.write(&rec(
            0,
            MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), 95000.0), ("ETH".into(), 3200.0)],
            }),
        ))
        .unwrap();
        sink.write(&rec(
            1,
            MarketEvent::L2Book(L2Book {
                coin: "BTC".into(),
                time_ms: 1,
                bids: vec![Level {
                    px: 1.0,
                    sz: 2.0,
                    n: 3,
                }],
                asks: vec![Level {
                    px: 4.0,
                    sz: 5.0,
                    n: 6,
                }],
            }),
        ))
        .unwrap();
        sink.write(&rec(
            2,
            MarketEvent::Trades(vec![Trade {
                coin: "BTC".into(),
                side: Side::Buy,
                px: 95000.0,
                sz: 0.1,
                time_ms: 1,
            }]),
        ))
        .unwrap();
        sink.finalize().unwrap();

        // 2 mids rows, 2 book levels (1 bid + 1 ask), 1 trade row.
        assert_eq!(read_row_count(&dir.path().join(ALL_MIDS_FILE)), 2);
        assert_eq!(read_row_count(&dir.path().join(L2_BOOK_FILE)), 2);
        assert_eq!(read_row_count(&dir.path().join(TRADES_FILE)), 1);
    }

    #[test]
    fn empty_session_still_writes_schema_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = ParquetSink::create(dir.path()).unwrap();
        sink.finalize().unwrap();

        // Files exist with zero rows.
        assert_eq!(read_row_count(&dir.path().join(ALL_MIDS_FILE)), 0);
        assert_eq!(read_row_count(&dir.path().join(L2_BOOK_FILE)), 0);
        assert_eq!(read_row_count(&dir.path().join(TRADES_FILE)), 0);
    }

    #[test]
    fn periodic_flush_preserves_all_rows() {
        let dir = tempfile::tempdir().unwrap();
        // Flush every 2 buffered rows to exercise multi-row-group writes.
        let mut sink = ParquetSink::with_flush_threshold(dir.path(), 2).unwrap();
        for seq in 0..5 {
            sink.write(&rec(
                seq,
                MarketEvent::Trades(vec![Trade {
                    coin: "BTC".into(),
                    side: Side::Sell,
                    px: 100.0,
                    sz: 1.0,
                    time_ms: seq as i64,
                }]),
            ))
            .unwrap();
        }
        sink.finalize().unwrap();
        assert_eq!(read_row_count(&dir.path().join(TRADES_FILE)), 5);
    }
}
