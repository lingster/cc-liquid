//! Hyperliquid Digital Twin — market data recorder.
//!
//! Captures tick-by-tick L2 book, mids and trades from the live Hyperliquid
//! WebSocket API and persists them as an append-only, replayable Parquet
//! session. See `PRD_HYPERLIQUID_DIGITAL_TWIN.md`.

pub mod client;
pub mod config;
pub mod crowdcent;
pub mod events;
pub mod info;
pub mod live;
pub mod live_ofi;
pub mod manifest;
pub mod merge_source;
pub mod notify;
pub mod parser;
pub mod proxy;
pub mod reconnect;
pub mod recorder;
pub mod replay;
pub mod sequencer;
pub mod sim;
pub mod sink;
pub mod source;
pub mod storage;
pub mod subscription;
pub mod twin;
pub mod universe;
pub mod viewer;
pub mod watchdog;
pub mod wire;
