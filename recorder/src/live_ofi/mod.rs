//! Live inference harness for `deepofi` multi-horizon return regressors.
//!
//! Rust port of the deepOFI Python pipeline (per-level order flow imbalance
//! features, causal per-feature z-score, windowed ONNX regression). The
//! feature math follows `deepofi.features`/`deepofi.normalization` exactly
//! (same f64 accumulation over f32-rounded features), so a model trained in
//! Python behaves identically here.

pub mod harness;
pub mod ledger;
pub mod model;
pub mod results;
pub mod state;
