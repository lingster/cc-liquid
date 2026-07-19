//! Reusable Parquet table building blocks shared by the single-file and
//! sharded sinks: column buffers, schemas, and a lazy file writer.
//!
//! Keeping these in one place means the exploded schemas (and the row-append
//! logic) live in exactly one location, so both sinks stay byte-for-byte
//! compatible (DRY).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use tracing::{info, warn};

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
///
/// **Same-day restart append:** Parquet cannot be appended in place (one
/// footer, at the end), so opening a path that already holds data would
/// otherwise truncate it — which is how a restart used to clobber the current
/// day's earlier capture. Instead, the existing file is renamed aside and its
/// rows are copied into the fresh writer before new rows flow, so a restart
/// *appends*. An existing file that cannot be read back (e.g. footerless after
/// a hard crash) is preserved next to the new file as `*.unrecovered-*` rather
/// than deleted.
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
            let carried_over = existing_nonempty(&self.path)
                .map(|prev| set_aside(&self.path, &prev))
                .transpose()?;
            let file = File::create(&self.path)?;
            let mut writer =
                ArrowWriter::try_new(file, self.schema.clone(), Some(writer_props()))?;
            if let Some(prev) = carried_over {
                copy_rows_forward(&prev, &mut writer, &self.path)?;
            }
            self.writer = Some(writer);
        }
        self.writer.as_mut().unwrap().write(batch)?;
        Ok(())
    }

    /// Finish the current row group and push it to the OS, without writing the
    /// footer. No-op if nothing has been written yet (so we never create an
    /// empty file mid-session).
    pub fn flush(&mut self) -> anyhow::Result<()> {
        if let Some(w) = self.writer.as_mut() {
            w.flush()?;
        }
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

/// `Some(path)` when a previous file with actual content sits at `path`.
fn existing_nonempty(path: &Path) -> Option<PathBuf> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > 0 => Some(path.to_path_buf()),
        _ => None,
    }
}

/// Rename `prev` aside under a unique name so a crash between the rename and
/// the copy can never destroy already-captured data (a later restart must not
/// re-use the same aside name and clobber it).
fn set_aside(path: &Path, prev: &Path) -> anyhow::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("table.parquet");
    let aside = path.with_file_name(format!("{name}.prev-{nanos}"));
    std::fs::rename(prev, &aside)?;
    Ok(aside)
}

/// Stream every row of `prev` into `writer` (append-on-restart), then delete
/// it. An unreadable `prev` (footerless crash leftover, foreign schema) is
/// preserved as `*.unrecovered-*` and recording continues with new rows only —
/// losing the new capture over an old file's corruption would be worse.
fn copy_rows_forward(
    prev: &Path,
    writer: &mut ArrowWriter<File>,
    path: &Path,
) -> anyhow::Result<()> {
    let mut copy = || -> anyhow::Result<usize> {
        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(prev)?)?.build()?;
        let mut rows = 0usize;
        for batch in reader {
            let batch = batch?;
            rows += batch.num_rows();
            writer.write(&batch)?;
        }
        Ok(rows)
    };
    match copy() {
        Ok(rows) => {
            std::fs::remove_file(prev)?;
            info!(
                "appended {rows} existing row(s) from a previous run into {}",
                path.display()
            );
        }
        Err(e) => {
            let name = prev.to_string_lossy().replace(".prev-", ".unrecovered-");
            let kept = PathBuf::from(name);
            let _ = std::fs::rename(prev, &kept);
            warn!(
                "existing {} is not readable ({e:#}); preserved as {} and recording fresh",
                path.display(),
                kept.display()
            );
        }
    }
    Ok(())
}

/// Highest `seq` present across a session's existing (readable) table files
/// with the given day `prefix` — `None` when no rows exist yet. Used on
/// restart to seed the sequencer past what is already on disk, so an appended
/// day keeps one monotonic `seq` sequence. Unreadable files are skipped: their
/// rows are not carried forward either, so they cannot collide.
pub fn max_existing_seq(dir: &Path, prefix: &str) -> Option<u64> {
    let mut paths = vec![
        dir.join(format!("{prefix}{ALL_MIDS_FILE}")),
        dir.join(format!("{prefix}{TRADES_FILE}")),
        dir.join(format!("{prefix}{L2_BOOK_FILE}")),
    ];
    if let Ok(parts) = std::fs::read_dir(dir.join(format!("{prefix}{L2_BOOK_DIR}"))) {
        paths.extend(parts.flatten().map(|e| e.path()).filter(|p| {
            p.extension().is_some_and(|x| x == "parquet")
        }));
    }
    paths.iter().filter_map(|p| max_seq_in_file(p)).max()
}

/// Max of the `seq` column in one parquet file, or `None` if unreadable/empty.
fn max_seq_in_file(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).ok()?;
    let seq_idx = builder.schema().index_of("seq").ok()?;
    let mask = parquet::arrow::ProjectionMask::roots(builder.parquet_schema(), [seq_idx]);
    let reader = builder.with_projection(mask).build().ok()?;
    reader
        .flatten()
        .filter_map(|batch| {
            batch
                .column(0)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .and_then(arrow::compute::max)
        })
        .max()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mids_batch(seqs: &[u64]) -> RecordBatch {
        let mut buf = MidsBuffer::default();
        for &s in seqs {
            buf.push(s, s as i64, s as i64, "BTC", 1.0);
        }
        buf.drain_to_batch().unwrap()
    }

    fn read_seqs(path: &Path) -> Vec<u64> {
        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        reader
            .flat_map(|b| {
                let b = b.unwrap();
                let idx = b.schema().index_of("seq").unwrap();
                b.column(idx)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .unwrap()
                    .values()
                    .to_vec()
            })
            .collect()
    }

    fn siblings_matching(dir: &Path, pattern: &str) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(pattern))
            .collect()
    }

    #[test]
    fn reopening_a_table_file_appends_instead_of_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mids.parquet");

        let mut first = TableFile::new(path.clone(), MidsBuffer::schema());
        first.write_batch(&mids_batch(&[0, 1, 2])).unwrap();
        first.close().unwrap();

        // The same-day restart: a fresh TableFile at the same path.
        let mut second = TableFile::new(path.clone(), MidsBuffer::schema());
        second.write_batch(&mids_batch(&[3, 4])).unwrap();
        second.close().unwrap();

        assert_eq!(read_seqs(&path), vec![0, 1, 2, 3, 4]);
        assert!(
            siblings_matching(dir.path(), ".prev-").is_empty(),
            "carried-over file must be removed after a successful copy"
        );
    }

    #[test]
    fn an_unreadable_existing_file_is_preserved_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mids.parquet");
        // A footerless crash leftover: bytes that are not readable parquet.
        std::fs::write(&path, b"PAR1 crashed mid-write, no footer").unwrap();

        let mut table = TableFile::new(path.clone(), MidsBuffer::schema());
        table.write_batch(&mids_batch(&[9])).unwrap();
        table.close().unwrap();

        // New capture is intact...
        assert_eq!(read_seqs(&path), vec![9]);
        // ...and the unreadable bytes were kept for manual salvage.
        let kept = siblings_matching(dir.path(), ".unrecovered-");
        assert_eq!(kept.len(), 1, "crash leftover must be preserved");
        assert_eq!(
            std::fs::read(&kept[0]).unwrap(),
            b"PAR1 crashed mid-write, no footer"
        );
    }

    #[test]
    fn max_existing_seq_scans_all_tables_including_l2_parts() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = "20260719_";

        let mut mids = TableFile::new(
            dir.path().join(format!("{prefix}{ALL_MIDS_FILE}")),
            MidsBuffer::schema(),
        );
        mids.write_batch(&mids_batch(&[0, 5])).unwrap();
        mids.close().unwrap();

        // The highest seq lives in an L2 part-file, not the mids table.
        let part_dir = dir.path().join(format!("{prefix}{L2_BOOK_DIR}"));
        std::fs::create_dir_all(&part_dir).unwrap();
        let mut book = TableFile::new(part_dir.join("part-0001.parquet"), BookBuffer::schema());
        let mut buf = BookBuffer::default();
        buf.push_book(
            42,
            1,
            1,
            &L2Book {
                coin: "BTC".into(),
                time_ms: 1,
                bids: vec![],
                asks: vec![crate::events::Level {
                    px: 1.0,
                    sz: 1.0,
                    n: 1,
                }],
            },
        );
        book.write_batch(&buf.drain_to_batch().unwrap()).unwrap();
        book.close().unwrap();

        assert_eq!(max_existing_seq(dir.path(), prefix), Some(42));
        // A different prefix (fresh day) sees nothing.
        assert_eq!(max_existing_seq(dir.path(), "20260720_"), None);
        // Unreadable files are skipped rather than failing the scan.
        std::fs::write(dir.path().join(format!("{prefix}{TRADES_FILE}")), b"junk").unwrap();
        assert_eq!(max_existing_seq(dir.path(), prefix), Some(42));
    }
}
