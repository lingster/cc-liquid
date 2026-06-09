//! Replay engine for recorded Hyperliquid sessions.
//!
//! Reconstructs market state from a recorded (or synthetic) event stream and
//! plays it back in tick mode (pull, 1 ms resolution) or realtime mode
//! (wall-clock paced). See `PRD_HYPERLIQUID_DIGITAL_TWIN.md` §6.

pub mod clock;
pub mod engine;
pub mod parquet_stream;
pub mod state;
pub mod stream;
pub mod synthetic;

pub use clock::{Clock, ManualClock, RealtimeClock};
pub use engine::ReplayEngine;
pub use parquet_stream::{load_session, load_session_stream};
pub use state::{BookState, MarketState};
pub use stream::{EventStream, VecEventStream};
pub use synthetic::{
    time_to_price, SyntheticEventStream, TimeRampEventStream, DEFAULT_STEP, TICK_UNIT_MS,
};
