//! Hyperliquid Digital Twin — market data recorder.
//!
//! Captures tick-by-tick L2 book, mids and trades from the live Hyperliquid
//! WebSocket API and persists them as an append-only, replayable Parquet
//! session. See `PRD_HYPERLIQUID_DIGITAL_TWIN.md`.

pub mod client;
pub mod config;
pub mod events;
pub mod manifest;
pub mod merge_source;
pub mod parser;
pub mod recorder;
pub mod replay;
pub mod sequencer;
pub mod sink;
pub mod source;
pub mod storage;
pub mod subscription;
pub mod twin;
pub mod wire;
