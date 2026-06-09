//! Storage backends for recorded events.

pub mod parquet_sink;
pub mod sharded;
pub mod tables;

pub use parquet_sink::ParquetSink;
pub use sharded::ShardedParquetSink;
pub use tables::{ALL_MIDS_FILE, L2_BOOK_DIR, L2_BOOK_FILE, TRADES_FILE};
