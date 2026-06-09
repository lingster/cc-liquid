//! Reusable Parquet table building blocks shared by the single-file and
//! sharded sinks: column buffers, schemas, and a lazy file writer.
//!
//! Keeping these in one place means the exploded schemas (and the row-append
//! logic) live in exactly one location, so both sinks stay byte-for-byte
//! compatible (DRY).

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::events::{L2Book, Trade};

pub const ALL_MIDS_FILE: &str = "all_mids.parquet";
pub const L2_BOOK_FILE: &str = "l2_book.parquet";
pub const TRADES_FILE: &str = "trades.parquet";

/// Directory holding the partitioned L2 book part-files.
pub const L2_BOOK_DIR: &str = "l2_book";

pub fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build()
}

/// Manages a single Parquet file: lazily opens on first write, and guarantees
/// the file exists (with schema) even when no rows were produced.
pub struct TableFile {
    path: PathBuf,
    schema: Arc<Schema>,
    writer: Option<ArrowWriter<File>>,
}

impl TableFile {
    pub fn new(path: PathBuf, schema: Arc<Schema>) -> Self {
        Self {
            path,
            schema,
            writer: None,
        }
    }

    pub fn write_batch(&mut self, batch: &RecordBatch) -> anyhow::Result<()> {
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
    pub fn close(&mut self) -> anyhow::Result<()> {
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
pub struct MidsBuffer {
    seq: Vec<u64>,
    ts_event_ms: Vec<i64>,
    ts_recv_ms: Vec<i64>,
    coin: Vec<String>,
    mid: Vec<f64>,
}

impl MidsBuffer {
    pub fn len(&self) -> usize {
        self.seq.len()
    }
    pub fn is_empty(&self) -> bool {
        self.seq.is_empty()
    }
    pub fn push(&mut self, seq: u64, te: i64, tr: i64, coin: &str, mid: f64) {
        self.seq.push(seq);
        self.ts_event_ms.push(te);
        self.ts_recv_ms.push(tr);
        self.coin.push(coin.to_string());
        self.mid.push(mid);
    }
    pub fn schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("seq", DataType::UInt64, false),
            Field::new("ts_event_ms", DataType::Int64, false),
            Field::new("ts_recv_ms", DataType::Int64, false),
            Field::new("coin", DataType::Utf8, false),
            Field::new("mid", DataType::Float64, false),
        ]))
    }
    pub fn drain_to_batch(&mut self) -> anyhow::Result<RecordBatch> {
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
pub struct BookBuffer {
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
    pub fn len(&self) -> usize {
        self.seq.len()
    }
    pub fn is_empty(&self) -> bool {
        self.seq.is_empty()
    }
    /// Append all levels of one book snapshot (both sides, best-first).
    pub fn push_book(&mut self, seq: u64, te: i64, tr: i64, book: &L2Book) {
        for (side, levels) in [("bid", &book.bids), ("ask", &book.asks)] {
            for (idx, lvl) in levels.iter().enumerate() {
                self.seq.push(seq);
                self.ts_event_ms.push(te);
                self.ts_recv_ms.push(tr);
                self.coin.push(book.coin.clone());
                self.side.push(side.to_string());
                self.level_idx.push(idx as u32);
                self.px.push(lvl.px);
                self.sz.push(lvl.sz);
                self.n.push(lvl.n);
            }
        }
    }
    pub fn schema() -> Arc<Schema> {
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
    pub fn drain_to_batch(&mut self) -> anyhow::Result<RecordBatch> {
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
pub struct TradesBuffer {
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
    pub fn len(&self) -> usize {
        self.seq.len()
    }
    pub fn is_empty(&self) -> bool {
        self.seq.is_empty()
    }
    pub fn push_trades(&mut self, seq: u64, te: i64, tr: i64, trades: &[Trade]) {
        for t in trades {
            self.seq.push(seq);
            self.ts_event_ms.push(te);
            self.ts_recv_ms.push(tr);
            self.coin.push(t.coin.clone());
            self.side.push(t.side.as_str().to_string());
            self.px.push(t.px);
            self.sz.push(t.sz);
            self.trade_time_ms.push(t.time_ms);
        }
    }
    pub fn schema() -> Arc<Schema> {
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
    pub fn drain_to_batch(&mut self) -> anyhow::Result<RecordBatch> {
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
