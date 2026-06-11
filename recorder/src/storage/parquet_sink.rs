//! Single-file Parquet implementation of [`EventSink`].
//!
//! Events are written to three flat ("exploded") tables in a session directory,
//! as specified in `PRD_HYPERLIQUID_DIGITAL_TWIN.md` §5.4. The reusable column
//! buffers and schemas live in [`crate::storage::tables`]; for a parallel,
//! partitioned L2 writer see [`crate::storage::sharded`].

use std::path::Path;

use crate::events::{MarketEvent, RecordedEvent};
use crate::sink::EventSink;
use crate::storage::tables::{BookBuffer, MidsBuffer, TableFile, TradesBuffer};
// Re-export the table file names so existing `storage::parquet_sink::*` paths
// continue to resolve.
pub use crate::storage::tables::{ALL_MIDS_FILE, L2_BOOK_FILE, TRADES_FILE};

const DEFAULT_FLUSH_THRESHOLD: usize = 10_000;

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

    /// Create a sink whose table files carry a name prefix (e.g. `20260610_`),
    /// used by the daily-rotating sink to keep each UTC day in its own files.
    pub fn create_prefixed(dir: impl AsRef<Path>, prefix: &str) -> anyhow::Result<Self> {
        Self::new_inner(dir, prefix, DEFAULT_FLUSH_THRESHOLD)
    }

    pub fn with_flush_threshold(
        dir: impl AsRef<Path>,
        flush_threshold: usize,
    ) -> anyhow::Result<Self> {
        Self::new_inner(dir, "", flush_threshold)
    }

    fn new_inner(
        dir: impl AsRef<Path>,
        prefix: &str,
        flush_threshold: usize,
    ) -> anyhow::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            flush_threshold: flush_threshold.max(1),
            mids_buf: MidsBuffer::default(),
            book_buf: BookBuffer::default(),
            trades_buf: TradesBuffer::default(),
            mids_file: TableFile::new(
                dir.join(format!("{prefix}{ALL_MIDS_FILE}")),
                MidsBuffer::schema(),
            ),
            book_file: TableFile::new(
                dir.join(format!("{prefix}{L2_BOOK_FILE}")),
                BookBuffer::schema(),
            ),
            trades_file: TableFile::new(
                dir.join(format!("{prefix}{TRADES_FILE}")),
                TradesBuffer::schema(),
            ),
        })
    }

    fn append(&mut self, event: &RecordedEvent) {
        let (seq, te, tr) = (event.seq, event.ts_event_ms, event.ts_recv_ms);
        match &event.payload {
            MarketEvent::AllMids(m) => {
                for (coin, mid) in &m.mids {
                    self.mids_buf.push(seq, te, tr, coin, *mid);
                }
            }
            MarketEvent::L2Book(b) => self.book_buf.push_book(seq, te, tr, b),
            MarketEvent::Trades(ts) => self.trades_buf.push_trades(seq, te, tr, ts),
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

    /// Drain every non-empty buffer into its file (encoding a row group) without
    /// closing. Shared by the periodic `flush` and the final drain.
    fn drain_all(&mut self) -> anyhow::Result<()> {
        if !self.mids_buf.is_empty() {
            let batch = self.mids_buf.drain_to_batch()?;
            self.mids_file.write_batch(&batch)?;
        }
        if !self.book_buf.is_empty() {
            let batch = self.book_buf.drain_to_batch()?;
            self.book_file.write_batch(&batch)?;
        }
        if !self.trades_buf.is_empty() {
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

    fn flush(&mut self) -> anyhow::Result<()> {
        self.drain_all()?;
        self.mids_file.flush()?;
        self.book_file.flush()?;
        self.trades_file.flush()?;
        Ok(())
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        self.drain_all()?;
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
    use std::fs::File;

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

        assert_eq!(read_row_count(&dir.path().join(ALL_MIDS_FILE)), 2);
        assert_eq!(read_row_count(&dir.path().join(L2_BOOK_FILE)), 2);
        assert_eq!(read_row_count(&dir.path().join(TRADES_FILE)), 1);
    }

    #[test]
    fn empty_session_still_writes_schema_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = ParquetSink::create(dir.path()).unwrap();
        sink.finalize().unwrap();

        assert_eq!(read_row_count(&dir.path().join(ALL_MIDS_FILE)), 0);
        assert_eq!(read_row_count(&dir.path().join(L2_BOOK_FILE)), 0);
        assert_eq!(read_row_count(&dir.path().join(TRADES_FILE)), 0);
    }

    #[test]
    fn explicit_flush_between_writes_preserves_all_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = ParquetSink::create(dir.path()).unwrap();
        for seq in 0..3 {
            sink.write(&rec(
                seq,
                MarketEvent::Trades(vec![Trade {
                    coin: "BTC".into(),
                    side: Side::Buy,
                    px: 100.0,
                    sz: 1.0,
                    time_ms: seq as i64,
                }]),
            ))
            .unwrap();
        }
        // Mid-session flush (e.g. the time-based flush) writes a row group...
        sink.flush().unwrap();
        // ...and recording continues into a second row group.
        sink.write(&rec(
            3,
            MarketEvent::Trades(vec![Trade {
                coin: "ETH".into(),
                side: Side::Sell,
                px: 50.0,
                sz: 2.0,
                time_ms: 3,
            }]),
        ))
        .unwrap();
        sink.finalize().unwrap();

        assert_eq!(read_row_count(&dir.path().join(TRADES_FILE)), 4);
    }

    #[test]
    fn periodic_flush_preserves_all_rows() {
        let dir = tempfile::tempdir().unwrap();
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
