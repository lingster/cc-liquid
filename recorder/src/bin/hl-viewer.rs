//! `hl-viewer` — egui/eframe desktop UI for inspecting recorded sessions.
//!
//! This binary is a *thin* shell: it owns the real wall clock and the egui
//! widgets, and delegates every decision to the pure model in
//! `hl_recorder::viewer` (indexing, navigation, playback math). Run with:
//!
//! ```text
//! hl-viewer <session_dir>      # e.g. hl-viewer sessions/smoke
//! ```

use std::time::Instant;

use eframe::egui;
use hl_recorder::viewer::{layout, Navigator, PlaybackClock, Section, SessionData, ViewerConfig};

#[path = "hl_viewer/render.rs"]
mod render;
use render::{
    format_utc_ms, render_depth_chart, render_price_chart, render_side, render_top_of_book,
    timeline_for, PriceChartColors,
};

/// Default config filename, looked up in the working directory. Override with a
/// second CLI argument: `hl-viewer <session_dir> [config.yaml]`.
const DEFAULT_CONFIG: &str = "hl-viewer-config.yaml";

fn main() -> eframe::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: hl-viewer <session_dir> [config.yaml]");
        std::process::exit(2);
    });
    let cfg_path = std::env::args().nth(2).unwrap_or_else(|| DEFAULT_CONFIG.to_string());
    let config = ViewerConfig::load_or_default(&cfg_path);

    let data = match SessionData::from_dir(&dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to load session `{dir}`: {e:#}");
            std::process::exit(1);
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "hl-viewer",
        options,
        Box::new(move |_cc| Ok(Box::new(ViewerApp::new(dir, data, config)))),
    )
}

/// eframe application state: wires the pure model to egui.
struct ViewerApp {
    dir: String,
    data: SessionData,
    nav: Navigator,
    playback: PlaybackClock,
    /// Stopwatch reset to `now` whenever Play (re)starts.
    play_started: Option<Instant>,
    /// Case-insensitive substring filter applied to the coin list.
    coin_filter: String,
    /// User colour preferences loaded from YAML.
    config: ViewerConfig,
    /// Last session-open error, shown in the menu bar until the next success.
    load_error: Option<String>,
    /// Top-to-bottom order of the draggable content sections.
    section_order: Vec<Section>,
}

impl ViewerApp {
    fn new(dir: String, data: SessionData, config: ViewerConfig) -> Self {
        let first_coin = data.coins().first().cloned().unwrap_or_default();
        let nav = Navigator::new(first_coin.clone());
        let playback = PlaybackClock::new(timeline_for(&data, &first_coin));
        Self {
            dir,
            data,
            nav,
            playback,
            play_started: None,
            coin_filter: String::new(),
            config,
            load_error: None,
            section_order: layout::default_order(),
        }
    }

    /// Rebuild playback timeline + reset navigation after a coin change.
    fn switch_coin(&mut self, coin: &str) {
        self.nav.set_coin(coin);
        self.playback = PlaybackClock::new(timeline_for(&self.data, coin));
        self.play_started = None;
    }

    /// Load a different session directory in place, resetting navigation and
    /// playback. On failure the old session stays loaded and the error is shown
    /// in the menu bar.
    fn open_session(&mut self, dir: String) {
        match SessionData::from_dir(&dir) {
            Ok(data) => {
                let first = data.coins().first().cloned().unwrap_or_default();
                self.data = data;
                self.nav = Navigator::new(first.clone());
                self.playback = PlaybackClock::new(timeline_for(&self.data, &first));
                self.play_started = None;
                self.coin_filter.clear();
                self.dir = dir;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(format!("open failed: {e:#}")),
        }
    }

    /// Apply paced playback: advance the navigator to the tick the wall clock
    /// says we should be on. Auto-pauses at end of session.
    fn drive_playback(&mut self, ctx: &egui::Context) {
        // Nothing to play on an empty timeline: never request a repaint here,
        // otherwise an empty session would spin a CPU core at 100% (D1).
        if !self.playback.is_playing() || self.playback.is_empty() {
            return;
        }
        let elapsed_ms = self
            .play_started
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let idx = self.playback.target_index(elapsed_ms);
        self.nav.seek_index(&self.data, idx);
        if self.playback.reached_end(elapsed_ms) {
            self.playback.pause();
            self.play_started = None;
        } else {
            // Keep animating.
            ctx.request_repaint();
        }
    }

    fn toggle_play(&mut self) {
        // Refuse to enter the Playing state when there is nothing to play (D1):
        // an empty timeline must never animate or request repaints.
        if self.playback.is_empty() {
            return;
        }
        if self.playback.is_playing() {
            self.playback.pause();
            self.play_started = None;
        } else {
            self.playback.play(self.nav.index());
            self.play_started = Some(Instant::now());
        }
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drive_playback(ctx);

        // Menu bar spans the full width at the very top.
        let to_open = egui::TopBottomPanel::top("menu_bar")
            .show(ctx, |ui| self.menu_bar(ui))
            .inner;
        if let Some(dir) = to_open {
            self.open_session(dir);
        }

        egui::SidePanel::left("coins_ladder")
            .resizable(true)
            .default_width(220.0)
            // Bound the width so a child widget that wants to expand (the coin
            // filter box) can never drive the panel into a grow-every-frame
            // feedback loop that swallows the central book view.
            .width_range(160.0..=400.0)
            .show(ctx, |ui| self.left_panel(ui));

        egui::TopBottomPanel::top("controls").show(ctx, |ui| self.controls(ui));

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| self.footer(ui));

        // The price chart and order book are draggable sections: grab a
        // section's ⠿ handle and drop it above/below the other to reorder them.
        egui::CentralPanel::default().show(ctx, |ui| self.sections(ui));
    }
}

impl ViewerApp {
    fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("session");
        ui.label(egui::RichText::new(&self.dir).monospace().weak());
        ui.separator();

        ui.label("coins");
        ui.horizontal(|ui| {
            // Reserve room for the clear button; use a finite width (never
            // f32::INFINITY, which makes the resizable panel grow each frame).
            let field_width = (ui.available_width() - 28.0).max(40.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.coin_filter)
                    .hint_text("filter…")
                    .desired_width(field_width),
            );
            if !self.coin_filter.is_empty() && ui.small_button("✕").clicked() {
                self.coin_filter.clear();
            }
        });
        let coins = self.data.coins();
        let needle = self.coin_filter.to_lowercase();
        let mut pending: Option<String> = None;
        let mut shown = 0usize;
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .id_salt("coin_list")
            .show(ui, |ui| {
                for coin in &coins {
                    if !needle.is_empty() && !coin.to_lowercase().contains(&needle) {
                        continue;
                    }
                    shown += 1;
                    let selected = coin == self.nav.coin();
                    if ui.selectable_label(selected, coin).clicked() && !selected {
                        pending = Some(coin.clone());
                    }
                }
                if shown == 0 {
                    ui.weak("no matches");
                }
            });
        if let Some(c) = pending {
            self.switch_coin(&c);
        }

        ui.separator();
        ui.label("price ladder (all ticks)");
        let ladder = self.data.price_ladder_counts(self.nav.coin());
        egui::ScrollArea::vertical()
            .id_salt("price_ladder")
            .show(ui, |ui| {
                // Show highest price first, like a depth ladder, with an
                // aggregated occurrence count to the right of each rung.
                egui::Grid::new("price_ladder_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.monospace("price");
                        ui.monospace("count");
                        ui.end_row();
                        for (px, count) in ladder.iter().rev() {
                            ui.monospace(format!("{px:>12.4}"));
                            ui.monospace(format!("{count:>6}"));
                            ui.end_row();
                        }
                    });
            });
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let len = self.nav.len(&self.data);
        ui.horizontal(|ui| {
            if ui.button("⏮ prev").clicked() {
                self.playback.pause();
                self.play_started = None;
                self.nav.prev(&self.data);
            }
            let play_label = if self.playback.is_playing() {
                "⏸ pause"
            } else {
                "▶ play"
            };
            // Disabled on an empty timeline so playback can never start (D1).
            let can_play = !self.playback.is_empty();
            if ui
                .add_enabled(can_play, egui::Button::new(play_label))
                .clicked()
            {
                self.toggle_play();
            }
            if ui.button("next ⏭").clicked() {
                self.playback.pause();
                self.play_started = None;
                self.nav.next(&self.data);
            }

            ui.separator();
            ui.label("speed");
            let mut speed = self.playback.speed();
            if ui
                .add(egui::Slider::new(&mut speed, 0.1..=50.0).logarithmic(true))
                .changed()
            {
                self.playback.set_speed(speed);
            }

            ui.separator();
            let tick = if len == 0 { 0 } else { self.nav.index() + 1 };
            ui.monospace(format!("tick {tick}/{len}"));
        });

        // Timeline scrubber over ts_event_ms (jumps to nearest snapshot).
        if let Some((lo, hi)) = self.data.time_range(self.nav.coin()) {
            let cur_ts = self
                .nav
                .current_snapshot(&self.data)
                .map(|s| s.ts_event_ms)
                .unwrap_or(lo);
            let mut t = cur_ts;
            let resp = ui.add(
                egui::Slider::new(&mut t, lo..=hi.max(lo.saturating_add(1)))
                    .text("ts_event_ms")
                    .integer(),
            );
            if resp.changed() {
                self.playback.pause();
                self.play_started = None;
                self.nav.seek_to_time(&self.data, t);
            }
        }
    }

    /// Top menu bar. Returns a folder path when the user picks one via
    /// `File ▸ Open`, so the caller can (re)load it outside this borrow.
    fn menu_bar(&self, ui: &mut egui::Ui) -> Option<String> {
        let mut to_open = None;
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open session folder…").clicked() {
                    ui.close_menu();
                    let mut dialog = rfd::FileDialog::new().set_title("Open session folder");
                    if !self.dir.is_empty() {
                        dialog = dialog.set_directory(&self.dir);
                    }
                    if let Some(path) = dialog.pick_folder() {
                        to_open = Some(path.display().to_string());
                    }
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            if let Some(err) = &self.load_error {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), err);
            }
        });
        to_open
    }

    /// Persistent footer showing the version and exact build so it is always
    /// obvious which binary is running (e.g. when a stale release lingers).
    fn footer(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "hl-viewer v{}  ·  built {}  ·  {}",
                    env!("CARGO_PKG_VERSION"),
                    env!("HL_BUILD_DATETIME"),
                    env!("HL_GIT_HASH"),
                ))
                .monospace()
                .weak(),
            );
        });
    }

    /// Render the draggable content sections (price chart, order book) in the
    /// user's current order. Each section has a ⠿ drag handle; dropping it on
    /// the other section's top/bottom half reorders them (50%-midpoint snap).
    fn sections(&mut self, ui: &mut egui::Ui) {
        let order = self.section_order.clone();
        // Captured out of the drag/closure scope, applied after the loop.
        let mut dropped: Option<(Section, Section, bool)> = None;
        let mut seek_ts: Option<i64> = None;

        egui::ScrollArea::vertical()
            .id_salt("sections")
            .show(ui, |ui| {
                for &section in &order {
                    let frame = egui::Frame::group(ui.style()).inner_margin(6.0);
                    let (inner, payload) =
                        ui.dnd_drop_zone::<Section, ()>(frame, |ui| {
                            ui.horizontal(|ui| {
                                // Only the handle is the drag source, so the
                                // chart/book bodies stay fully interactive.
                                ui.dnd_drag_source(
                                    egui::Id::new(("section_handle", section)),
                                    section,
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("⠿").strong().monospace(),
                                        )
                                        .on_hover_text("drag to reorder");
                                    },
                                );
                                ui.label(egui::RichText::new(section.title()).strong());
                            });
                            ui.separator();
                            match section {
                                Section::PriceChart => {
                                    // Bound the chart's height; it reads
                                    // available_height, which is unbounded in a
                                    // scroll area.
                                    ui.allocate_ui(
                                        egui::vec2(ui.available_width(), 220.0),
                                        |ui| {
                                            if let Some(ts) = self.price_chart_panel(ui) {
                                                seek_ts = Some(ts);
                                            }
                                        },
                                    );
                                }
                                Section::OrderBook => self.book_view(ui),
                            }
                        });

                    if let Some(dragged) = payload {
                        // Snap: pointer past the section's vertical midpoint ⇒
                        // drop below it, otherwise above.
                        let after = ui
                            .input(|i| i.pointer.interact_pos())
                            .map(|p| p.y > inner.response.rect.center().y)
                            .unwrap_or(false);
                        dropped = Some((*dragged, section, after));
                    }
                }
            });

        if let Some((dragged, reference, after)) = dropped {
            self.section_order =
                layout::reordered(&self.section_order, dragged, reference, after);
        }
        if let Some(ts) = seek_ts {
            self.playback.pause();
            self.play_started = None;
            self.nav.seek_to_time(&self.data, ts);
        }
    }

    /// Price panel: the selected coin's mid-price over the full loaded time
    /// range, with a light-blue overlay tracking the current playback position.
    /// Returns a timestamp when the user clicks the chart, so the caller can
    /// seek playback there.
    fn price_chart_panel(&self, ui: &mut egui::Ui) -> Option<i64> {
        let coin = self.nav.coin();
        ui.horizontal(|ui| {
            ui.heading(format!("{coin} — price"));
            if let Some((lo, hi)) = self.data.time_range(coin) {
                ui.separator();
                ui.monospace(format!("{} … {}", format_utc_ms(lo), format_utc_ms(hi)));
            }
            ui.separator();
            ui.weak("click to seek");
        });

        let series = self.data.price_series(coin);
        let current_ts = self
            .nav
            .current_snapshot(&self.data)
            .map(|s| s.ts_event_ms)
            .unwrap_or(i64::MIN);
        let [fr, fg, fb] = self.config.price_chart.full_color;
        let [er, eg, eb] = self.config.price_chart.elapsed_color;
        let colors = PriceChartColors {
            full: egui::Color32::from_rgb(fr, fg, fb),
            elapsed: egui::Color32::from_rgb(er, eg, eb),
        };
        render_price_chart(ui, &series, current_ts, &colors)
    }

    fn book_view(&mut self, ui: &mut egui::Ui) {
        let Some(book) = self.nav.current_book(&self.data).cloned() else {
            ui.centered_and_justified(|ui| {
                ui.label("no L2 snapshots for the selected coin");
            });
            return;
        };

        ui.heading(format!("{} — order book", book.coin));
        render_top_of_book(ui, &book);
        ui.separator();

        // Place BIDS and ASKS directly next to each other (rather than each
        // filling half the panel) so the two ladders are easy to compare.
        // The grids live in a fixed-height ScrollArea: a bare `Grid` inside a
        // horizontal layout can misreport its height and shove every later
        // widget (the depth chart) off the bottom of the panel, so we pin the
        // region's height to keep the layout cursor predictable.
        egui::ScrollArea::vertical()
            .id_salt("ladders")
            // Shrink vertically to the actual rows (~20) so the depth chart
            // sits directly beneath the ladders; the max_height only caps
            // pathologically deep books (raised so a typical full book isn't
            // clipped behind the scrollbar).
            .auto_shrink([false, true])
            .max_height(600.0)
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    render_side(
                        ui,
                        "BIDS",
                        &book.bids,
                        egui::Color32::from_rgb(80, 200, 120),
                    );
                    ui.separator();
                    render_side(ui, "ASKS", &book.asks, egui::Color32::from_rgb(220, 90, 90));
                });
            });

        ui.separator();
        // Depth chart: bids left / asks right, volume axis fixed to the coin's
        // session-wide size range so bars don't rescale during playback.
        let (vol_min, vol_max) = self.data.size_range(&book.coin).unwrap_or((0.0, 0.0));
        ui.horizontal(|ui| {
            ui.label("depth (volume)");
            ui.separator();
            ui.monospace(format!("vol {vol_min:.4} … {vol_max:.4}"));
        });
        render_depth_chart(ui, &book, vol_max);
    }
}
