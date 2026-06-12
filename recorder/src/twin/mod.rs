//! Digital-twin API layer.
//!
//! Components here make a replayed (or synthetic) event stream *look like* the
//! live Hyperliquid feed, so the rest of the system — including the recorder —
//! can consume the twin exactly as it consumes the real exchange.

pub mod playback_source;

pub use playback_source::PlaybackSource;
