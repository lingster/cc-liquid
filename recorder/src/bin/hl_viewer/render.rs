//! egui rendering helpers for the order-book view, split out of the binary
//! `hl-viewer.rs` to keep each source file under the 300-line module limit.
//!
//! These functions are pure presentation: they read an [`L2Book`] / level
//! slice and paint widgets. All navigation/playback decisions live in the
//! pure `hl_recorder::viewer` model.

use chrono::{DateTime, Utc};
use eframe::egui;
use hl_recorder::events::{L2Book, Level};
use hl_recorder::viewer::SessionData;

/// The per-coin snapshot timeline (ts_event_ms) the playback clock runs over.
pub fn timeline_for(data: &SessionData, coin: &str) -> Vec<i64> {
    data.snapshots(coin).iter().map(|s| s.ts_event_ms).collect()
}

/// Format a unix-millisecond timestamp as a human-readable UTC string, e.g.
/// `2024-01-02 03:04:05.678 UTC`. Falls back to the raw value if it is out of
/// chrono's representable range.
pub fn format_utc_ms(ts_ms: i64) -> String {
    match DateTime::<Utc>::from_timestamp_millis(ts_ms) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S%.3f UTC").to_string(),
        None => format!("{ts_ms} (raw)"),
    }
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
        ui.separator();
        ui.monospace(format_utc_ms(book.time_ms));
    });
}

/// Draw the L2 book as a depth bar chart: bids on the left, asks on the right,
/// best prices meeting at the centre divider. Each bar's height is the level's
/// volume (`sz`), scaled so `max_size` fills the chart — `max_size` is the
/// session-wide max for the coin, so bars keep a stable scale across playback.
pub fn render_depth_chart(ui: &mut egui::Ui, book: &L2Book, max_size: f64) {
    const HEIGHT: f32 = 160.0;
    const TOP_PAD: f32 = 6.0;
    let bid_color = egui::Color32::from_rgb(80, 200, 120);
    let ask_color = egui::Color32::from_rgb(220, 90, 90);

    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    let n_bids = book.bids.len();
    let n_asks = book.asks.len();
    let total = (n_bids + n_asks).max(1);
    let bar_w = rect.width() / total as f32;
    let baseline = rect.bottom();
    let usable_h = HEIGHT - TOP_PAD;
    // Guard against a flat/empty session (max_size <= 0) so we never divide by
    // zero; bars simply render at zero height.
    let safe_max = if max_size > 0.0 { max_size } else { 1.0 };
    let bar_height = |sz: f64| ((sz / safe_max).clamp(0.0, 1.0) as f32) * usable_h;

    // Draw each level and record its full-height column rect so the whole
    // column is hoverable (even when the bar itself is only a few px tall).
    struct Hit {
        col: egui::Rect,
        px: f64,
        sz: f64,
        is_bid: bool,
    }
    let mut hits: Vec<Hit> = Vec::with_capacity(total);

    let mut draw_bar = |x: f32, px: f64, sz: f64, color: egui::Color32, is_bid: bool| {
        let col =
            egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + bar_w, baseline));
        if sz.is_finite() {
            let h = bar_height(sz);
            let bar = egui::Rect::from_min_max(
                egui::pos2(x, baseline - h),
                egui::pos2(x + (bar_w - 1.0).max(1.0), baseline),
            );
            painter.rect_filled(bar, 0.0, color);
        }
        hits.push(Hit {
            col,
            px,
            sz,
            is_bid,
        });
    };

    // Left half: bids worst→best (best ends up adjacent to the centre). Book
    // bids are best-first, so iterate reversed.
    let mut x = rect.left();
    for lvl in book.bids.iter().rev() {
        draw_bar(x, lvl.px, lvl.sz, bid_color, true);
        x += bar_w;
    }
    // Right half: asks best→worst (best adjacent to the centre). Already
    // best-first, so iterate as delivered.
    for lvl in &book.asks {
        draw_bar(x, lvl.px, lvl.sz, ask_color, false);
        x += bar_w;
    }

    // Centre divider between the bid and ask halves.
    let center_x = rect.left() + bar_w * n_bids as f32;
    painter.line_segment(
        [
            egui::pos2(center_x, rect.top()),
            egui::pos2(center_x, baseline),
        ],
        egui::Stroke::new(1.0, egui::Color32::GRAY),
    );

    // Hover: highlight the column under the pointer and show price/volume.
    if let Some(pos) = resp.hover_pos() {
        if let Some(hit) = hits.iter().find(|h| h.col.contains(pos)) {
            painter.rect_stroke(
                hit.col,
                0.0,
                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(160)),
            );
            let side = if hit.is_bid { "bid" } else { "ask" };
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                egui::Id::new("depth_chart_tooltip"),
                |ui| {
                    ui.monospace(side);
                    ui.monospace(format!("price  {:.4}", hit.px));
                    ui.monospace(format!("volume {:.4}", hit.sz));
                },
            );
        }
    }
}

/// Colours for [`render_price_chart`]; sourced from the user's YAML config.
pub struct PriceChartColors {
    /// The full price history line (spanning the whole time range).
    pub full: egui::Color32,
    /// The overlay up to the current playback position.
    pub elapsed: egui::Color32,
}

/// Draw the coin's mid-price over time. The X axis always spans the full series
/// (`series` is `(ts_ms, price)` in ascending time), so the chart shows the
/// entire loaded range regardless of playback position. The whole series is
/// drawn in `colors.full`; the portion up to `current_ts` is overdrawn in
/// `colors.elapsed` with a playhead marker, so users can see where in time the
/// replay has reached. Hovering shows the time + price of the nearest point.
pub fn render_price_chart(
    ui: &mut egui::Ui,
    series: &[(i64, f64)],
    current_ts: i64,
    colors: &PriceChartColors,
) -> Option<i64> {
    let height = ui.available_height().max(80.0);
    let width = ui.available_width();
    // Click sense (which also reports hover) so the chart can both show a
    // tooltip and seek playback when clicked.
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let painter = ui.painter_at(rect);

    if series.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "no price series for this coin",
            egui::FontId::monospace(12.0),
            egui::Color32::GRAY,
        );
        return None;
    }

    let plot = rect.shrink(6.0);
    let t0 = series.first().unwrap().0;
    let t1 = series.last().unwrap().0;
    let t_span = (t1 - t0).max(1) as f64;
    let (mut p_min, mut p_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(_, p) in series {
        p_min = p_min.min(p);
        p_max = p_max.max(p);
    }

    let x_of = |t: i64| plot.left() + ((t - t0) as f64 / t_span) as f32 * plot.width();
    let y_of = |p: f64| {
        if p_max <= p_min {
            plot.center().y // flat price: draw down the middle
        } else {
            plot.bottom() - ((p - p_min) / (p_max - p_min)) as f32 * plot.height()
        }
    };

    // Full price history in the "full" colour.
    let full_pts: Vec<egui::Pos2> = series.iter().map(|&(t, p)| egui::pos2(x_of(t), y_of(p))).collect();
    painter.add(egui::Shape::line(
        full_pts,
        egui::Stroke::new(1.0, colors.full),
    ));

    // Elapsed overlay up to the playback position, plus a playhead.
    let elapsed_pts: Vec<egui::Pos2> = series
        .iter()
        .filter(|&&(t, _)| t <= current_ts)
        .map(|&(t, p)| egui::pos2(x_of(t), y_of(p)))
        .collect();
    if elapsed_pts.len() >= 2 {
        painter.add(egui::Shape::line(
            elapsed_pts.clone(),
            egui::Stroke::new(1.5, colors.elapsed),
        ));
    }
    if let Some(&head) = elapsed_pts.last() {
        painter.line_segment(
            [egui::pos2(head.x, plot.top()), egui::pos2(head.x, plot.bottom())],
            egui::Stroke::new(0.5, colors.elapsed.gamma_multiply(0.6)),
        );
        painter.circle_filled(head, 3.0, colors.elapsed);
    }

    // Hover: snap to the nearest point by X and show its time + price.
    if let Some(pos) = resp.hover_pos() {
        let mut best = 0usize;
        let mut best_dx = f32::MAX;
        for (i, &(t, _)) in series.iter().enumerate() {
            let dx = (x_of(t) - pos.x).abs();
            if dx < best_dx {
                best_dx = dx;
                best = i;
            }
        }
        let (t, p) = series[best];
        let pt = egui::pos2(x_of(t), y_of(p));
        painter.circle_stroke(pt, 4.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        egui::show_tooltip_at_pointer(
            ui.ctx(),
            ui.layer_id(),
            egui::Id::new("price_chart_tooltip"),
            |ui| {
                ui.monospace(format_utc_ms(t));
                ui.monospace(format!("price {p:.4}"));
            },
        );
    }

    // Click: map the pointer X back to a timestamp across the full range so the
    // caller can seek there (the navigator snaps to the nearest snapshot).
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let frac = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64;
            return Some(t0 + (frac * t_span).round() as i64);
        }
    }
    None
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
