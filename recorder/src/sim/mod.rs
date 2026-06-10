//! Order simulation core (PRD §7): matching engine, virtual account and
//! fill-price overlays for the digital twin.
//!
//! Everything here is pure and deterministic — no I/O, no wall clock, no
//! global RNG — so the same recorded session plus the same order stream
//! always produces identical fills and PnL (PRD §10.5).

pub mod account;
pub mod engine;
pub mod matching;
pub mod order;
pub mod overlay;

pub use account::VirtualAccount;
pub use engine::{SimConfig, SimEngine};
pub use matching::{FillResult, QueueModel};
pub use order::{OrderOutcome, OrderRequest, OrderType, Tif, TriggerKind, Universe};
pub use overlay::FillOverlay;
