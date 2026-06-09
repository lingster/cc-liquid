//! egui rendering helpers for the order-book view, split out of the binary
//! `hl-viewer.rs` to keep each source file under the 300-line module limit.
//!
//! These functions are pure presentation: they read an [`L2Book`] / level
//! slice and paint widgets. All navigation/playback decisions live in the
//! pure `hl_recorder::viewer` model.

use eframe::egui;
use hl_recorder::events::{L2Book, Level};
use hl_recorder::viewer::SessionData;

/// The per-coin snapshot timeline (ts_event_ms) the playback clock runs over.
pub fn timeline_for(data: &SessionData, coin: &str) -> Vec<i64> {
    data.snapshots(coin).iter().map(|s| s.ts_event_ms).collect()
}

// Hyperliquid delivers L2 levels best-first, and the recorder preserves that
// order on the round-trip: `bids` is sorted descending by price and `asks`
// ascending by price. So `bids.first()` is the best (highest) bid and
// `asks.first()` is the best (lowest) ask, and rendering the slices as-is
// already satisfies the "bids desc / asks asc" requirement — no runtime sort.
pub fn render_top_of_book(ui: &mut egui::Ui, book: &L2Book) {
    let bid = book.bids.first().map(|l| l.px);
    let ask = book.asks.first().map(|l| l.px);
    let mid = match (bid, ask) {
        (Some(b), Some(a)) => Some((b + a) / 2.0),
        _ => None,
    };
    ui.horizontal(|ui| {
        ui.monospace(format!(
            "best bid {}",
            bid.map(|v| format!("{v:.4}")).unwrap_or_else(|| "—".into())
        ));
        ui.separator();
        ui.monospace(format!(
            "best ask {}",
            ask.map(|v| format!("{v:.4}")).unwrap_or_else(|| "—".into())
        ));
        ui.separator();
        ui.monospace(format!(
            "mid {}",
            mid.map(|v| format!("{v:.4}")).unwrap_or_else(|| "—".into())
        ));
        ui.separator();
        ui.monospace(format!("t={}", book.time_ms));
    });
}

/// Render one side of the book (bids or asks) as a px/sz/n grid in delivered
/// (best-first) order.
pub fn render_side(ui: &mut egui::Ui, title: &str, levels: &[Level], color: egui::Color32) {
    ui.colored_label(color, title);
    egui::Grid::new(title)
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            ui.monospace("px");
            ui.monospace("sz");
            ui.monospace("n");
            ui.end_row();
            for lvl in levels {
                ui.monospace(format!("{:.4}", lvl.px));
                ui.monospace(format!("{:.4}", lvl.sz));
                ui.monospace(format!("{}", lvl.n));
                ui.end_row();
            }
        });
}
