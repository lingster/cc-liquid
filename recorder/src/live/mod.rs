//! Live inference harness: stream L2 books, run an exported ONNX model,
//! score predictions against the actual mids once they arrive.
//!
//! This is the Rust port of `orderbooker`'s Python live harness. The grid
//! binning, causal normalization and labelling are bit-compatible with the
//! Python training pipeline (same math in f64, cast to f32 at the same
//! points), so a model trained in Python behaves identically here.

pub mod grid;
pub mod harness;
pub mod ledger;
pub mod model;
pub mod results;
