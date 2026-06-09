//! High-throughput, partitioned L2 storage.
//!
//! The L2 book is by far the heaviest stream (many coins × many levels × many
//! ticks). [`ShardedParquetSink`] partitions it across **N Parquet part-files**,
//! each written by its own background worker thread. The recording thread only
//! buffers rows and hands finished [`RecordBatch`]es to workers over channels,
//! so ZSTD compression and disk I/O run in parallel across cores while ordering
//! within each coin is preserved (a coin always maps to the same shard).
//!
//! Layout:
//! ```text
//! session/
//! ├── all_mids.parquet           # single file (light)
//! ├── trades.parquet             # single file (light)
//! └── l2_book/
//!     ├── part-0000.parquet      # coins where hash(coin) % N == 0
//!     ├── part-0001.parquet
//!     └── ...                    # N parts, written concurrently
//! ```
//! Mids and trades stay single-file; only the L2 hotspot is parallelized.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread::JoinHandle;

use arrow::record_batch::RecordBatch;

use crate::events::{MarketEvent, RecordedEvent};
use crate::sink::EventSink;
use crate::storage::tables::{
    BookBuffer, MidsBuffer, TableFile, TradesBuffer, ALL_MIDS_FILE, L2_BOOK_DIR, TRADES_FILE,
};

const DEFAULT_FLUSH_THRESHOLD: usize = 10_000;
/// Bounded so a slow disk applies back-pressure instead of growing memory.
const WORKER_QUEUE_DEPTH: usize = 8;

/// Map a coin to one of `num_shards` partitions deterministically.
pub fn shard_for(coin: &str, num_shards: usize) -> usize {
    let mut h = DefaultHasher::new();
    coin.hash(&mut h);
    (h.finish() % num_shards as u64) as usize
}

/// Name of the L2 part-file for a shard index.
pub fn part_file_name(shard: usize) -> String {
    format!("part-{shard:04}.parquet")
}

/// A background L2 writer owning one part-file.
struct BookWorker {
    tx: SyncSender<RecordBatch>,
    handle: JoinHandle<anyhow::Result<()>>,
}

impl BookWorker {
    fn spawn(path: PathBuf) -> Self {
        let (tx, rx) = sync_channel::<RecordBatch>(WORKER_QUEUE_DEPTH);
        let handle = std::thread::spawn(move || -> anyhow::Result<()> {
            let mut file = TableFile::new(path, BookBuffer::schema());
            while let Ok(batch) = rx.recv() {
                file.write_batch(&batch)?;
            }
            // Channel closed: flush and finalize (writes an empty schema file
            // if this shard never received any rows).
            file.close()
        });
        Self { tx, handle }
    }

    fn send(&self, batch: RecordBatch) -> anyhow::Result<()> {
        self.tx
            .send(batch)
            .map_err(|_| anyhow::anyhow!("L2 writer worker terminated early"))
    }

    /// Close the channel and join, propagating any worker error.
    fn finish(self) -> anyhow::Result<()> {
        drop(self.tx);
        self.handle
            .join()
            .map_err(|_| anyhow::anyhow!("L2 writer worker panicked"))?
    }
}

/// Partitioned, parallel sink. Implements [`EventSink`] so it is a drop-in
/// replacement for [`crate::storage::ParquetSink`].
pub struct ShardedParquetSink {
    flush_threshold: usize,
    // L2 partitions (one buffer + one worker per shard).
    book_bufs: Vec<BookBuffer>,
    workers: Vec<BookWorker>,
    // Light single-file streams.
    mids_buf: MidsBuffer,
    trades_buf: TradesBuffer,
    mids_file: TableFile,
    trades_file: TableFile,
}

impl ShardedParquetSink {
    /// Create a sink with `num_shards` parallel L2 writers (clamped to >= 1).
    pub fn create(dir: impl AsRef<Path>, num_shards: usize) -> anyhow::Result<Self> {
        Self::with_flush_threshold(dir, num_shards, DEFAULT_FLUSH_THRESHOLD)
    }

    pub fn with_flush_threshold(
        dir: impl AsRef<Path>,
        num_shards: usize,
        flush_threshold: usize,
    ) -> anyhow::Result<Self> {
        let num_shards = num_shards.max(1);
        let dir = dir.as_ref();
        let book_dir = dir.join(L2_BOOK_DIR);
        std::fs::create_dir_all(&book_dir)?;

        let mut book_bufs = Vec::with_capacity(num_shards);
        let mut workers = Vec::with_capacity(num_shards);
        for shard in 0..num_shards {
            book_bufs.push(BookBuffer::default());
            workers.push(BookWorker::spawn(book_dir.join(part_file_name(shard))));
        }

        Ok(Self {
            flush_threshold: flush_threshold.max(1),
            book_bufs,
            workers,
            mids_buf: MidsBuffer::default(),
            trades_buf: TradesBuffer::default(),
            mids_file: TableFile::new(dir.join(ALL_MIDS_FILE), MidsBuffer::schema()),
            trades_file: TableFile::new(dir.join(TRADES_FILE), TradesBuffer::schema()),
        })
    }

    pub fn num_shards(&self) -> usize {
        self.workers.len()
    }

    fn flush_shard(&mut self, shard: usize) -> anyhow::Result<()> {
        if !self.book_bufs[shard].is_empty() {
            let batch = self.book_bufs[shard].drain_to_batch()?;
            self.workers[shard].send(batch)?;
        }
        Ok(())
    }
}

impl EventSink for ShardedParquetSink {
    fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
        let (seq, te, tr) = (event.seq, event.ts_event_ms, event.ts_recv_ms);
        match &event.payload {
            MarketEvent::AllMids(m) => {
                for (coin, mid) in &m.mids {
                    self.mids_buf.push(seq, te, tr, coin, *mid);
                }
                if self.mids_buf.len() >= self.flush_threshold {
                    let batch = self.mids_buf.drain_to_batch()?;
                    self.mids_file.write_batch(&batch)?;
                }
            }
            MarketEvent::L2Book(b) => {
                let shard = shard_for(&b.coin, self.num_shards());
                self.book_bufs[shard].push_book(seq, te, tr, b);
                if self.book_bufs[shard].len() >= self.flush_threshold {
                    self.flush_shard(shard)?;
                }
            }
            MarketEvent::Trades(ts) => {
                self.trades_buf.push_trades(seq, te, tr, ts);
                if self.trades_buf.len() >= self.flush_threshold {
                    let batch = self.trades_buf.drain_to_batch()?;
                    self.trades_file.write_batch(&batch)?;
                }
            }
        }
        Ok(())
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        // Drain residual L2 buffers to their workers.
        for shard in 0..self.num_shards() {
            self.flush_shard(shard)?;
        }
        // Close workers (each finalizes its part-file), surfacing the first error.
        let mut first_err: Option<anyhow::Error> = None;
        for worker in self.workers.drain(..) {
            if let Err(e) = worker.finish() {
                first_err.get_or_insert(e);
            }
        }

        // Flush and close the light single-file streams.
        if !self.mids_buf.is_empty() {
            let batch = self.mids_buf.drain_to_batch()?;
            self.mids_file.write_batch(&batch)?;
        }
        if !self.trades_buf.is_empty() {
            let batch = self.trades_buf.drain_to_batch()?;
            self.trades_file.write_batch(&batch)?;
        }
        self.mids_file.close()?;
        self.trades_file.close()?;

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Level, MarketEvent, Trade};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    fn book(coin: &str, seq: u64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: seq as i64,
            ts_recv_ms: seq as i64,
            payload: MarketEvent::L2Book(L2Book {
                coin: coin.into(),
                time_ms: seq as i64,
                bids: vec![Level {
                    px: 100.0,
                    sz: 1.0,
                    n: 1,
                }],
                asks: vec![Level {
                    px: 101.0,
                    sz: 1.0,
                    n: 1,
                }],
            }),
        }
    }

    fn read_rows(path: &Path) -> usize {
        let file = File::open(path).unwrap();
        ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap()
            .map(|b| b.unwrap().num_rows())
            .sum()
    }

    fn total_l2_rows(dir: &Path, num_shards: usize) -> usize {
        (0..num_shards)
            .map(|s| read_rows(&dir.join(L2_BOOK_DIR).join(part_file_name(s))))
            .sum()
    }

    #[test]
    fn shard_assignment_is_deterministic_and_in_range() {
        for coin in ["BTC", "ETH", "SOL", "DOGE", "WIF"] {
            let a = shard_for(coin, 4);
            let b = shard_for(coin, 4);
            assert_eq!(a, b, "stable mapping");
            assert!(a < 4, "within range");
        }
    }

    #[test]
    fn parallel_writers_preserve_all_l2_rows() {
        let dir = tempfile::tempdir().unwrap();
        let shards = 4;
        let mut sink = ShardedParquetSink::with_flush_threshold(dir.path(), shards, 3).unwrap();

        // Many coins across many ticks; each book = 2 level rows.
        let coins = ["BTC", "ETH", "SOL", "DOGE", "WIF", "APT", "ARB", "OP"];
        let mut seq = 0u64;
        for _ in 0..10 {
            for c in coins {
                sink.write(&book(c, seq)).unwrap();
                seq += 1;
            }
        }
        sink.finalize().unwrap();

        let expected_rows = coins.len() * 10 * 2;
        assert_eq!(total_l2_rows(dir.path(), shards), expected_rows);
    }

    #[test]
    fn each_coin_lands_in_exactly_one_part() {
        let dir = tempfile::tempdir().unwrap();
        let shards = 3;
        let mut sink = ShardedParquetSink::create(dir.path(), shards).unwrap();
        // Two distinct coins, multiple updates each.
        for seq in 0..6 {
            let coin = if seq % 2 == 0 { "BTC" } else { "ETH" };
            sink.write(&book(coin, seq)).unwrap();
        }
        sink.finalize().unwrap();

        // BTC rows only in its shard; ETH rows only in its shard.
        let btc_shard = shard_for("BTC", shards);
        let eth_shard = shard_for("ETH", shards);
        // 3 updates each * 2 levels = 6 rows per coin.
        assert_eq!(
            read_rows(&dir.path().join(L2_BOOK_DIR).join(part_file_name(btc_shard))),
            if btc_shard == eth_shard { 12 } else { 6 }
        );
    }

    #[test]
    fn also_writes_mids_and_trades_single_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = ShardedParquetSink::create(dir.path(), 2).unwrap();
        sink.write(&RecordedEvent {
            seq: 0,
            ts_event_ms: 0,
            ts_recv_ms: 0,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), 1.0), ("ETH".into(), 2.0)],
            }),
        })
        .unwrap();
        sink.write(&RecordedEvent {
            seq: 1,
            ts_event_ms: 1,
            ts_recv_ms: 1,
            payload: MarketEvent::Trades(vec![Trade {
                coin: "BTC".into(),
                side: crate::events::Side::Buy,
                px: 1.0,
                sz: 1.0,
                time_ms: 1,
            }]),
        })
        .unwrap();
        sink.finalize().unwrap();

        assert_eq!(read_rows(&dir.path().join(ALL_MIDS_FILE)), 2);
        assert_eq!(read_rows(&dir.path().join(TRADES_FILE)), 1);
    }
}
