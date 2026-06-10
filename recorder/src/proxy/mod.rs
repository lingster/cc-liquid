//! Digital Twin Proxy — network-level capture & replay of the Hyperliquid
//! wire protocol. See `PRD_HYPERLIQUID_DIGITAL_TWIN.md` Appendix B.
//!
//! The proxy is a standalone HTTP process (`hl-proxy`) that speaks the exact
//! Hyperliquid REST surface cc-liquid consumes (`POST /info`, `POST /exchange`),
//! so the app can be repointed at it via `base_url` alone. Market reads can be
//! toggled between live forwarding and deterministic playback of a recorded
//! session; every round-trip is captured to an append-only JSONL log.

pub mod handler;
pub mod log;
pub mod market;
pub mod request;
pub mod server;
pub mod upstream;

pub use handler::{MarketSource, ProxyConfig, ProxyHandler, ProxyResponse};
pub use log::{JsonlSink, MemorySink, ResponseSource, RpcLogEntry, RpcSink, RPC_LOG_FILE};
pub use market::{MarketDataProvider, PlaybackMarket};
pub use request::{classify, Endpoint, MethodTag};
pub use upstream::{HttpUpstream, ScriptedUpstream, Upstream, UpstreamResponse};
