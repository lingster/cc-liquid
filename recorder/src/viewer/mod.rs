//! Pure, egui-free model logic for the session viewer.
//!
//! This module tree holds everything the desktop UI needs *except* rendering:
//! indexing recorded events into per-coin L2 snapshots ([`SessionData`]),
//! tick navigation ([`Navigator`]), and wall-clock-paced playback math
//! ([`PlaybackClock`]). All of it is deterministic and unit-tested; the eframe
//! binary (`src/bin/hl-viewer.rs`) is a thin shell over these types.

pub mod config;
pub mod navigator;
pub mod playback;
pub mod session_data;

pub use config::{PriceChartConfig, ViewerConfig};
pub use navigator::Navigator;
pub use playback::{PlaybackClock, PlaybackState};
pub use session_data::SessionData;
