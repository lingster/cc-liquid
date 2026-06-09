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
use hl_recorder::viewer::{Navigator, PlaybackClock, SessionData};

#[path = "hl_viewer/render.rs"]
mod render;
use render::{render_depth_chart, render_side, render_top_of_book, timeline_for};

fn main() -> eframe::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: hl-viewer <session_dir>");
        std::process::exit(2);
    });

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
        Box::new(move |_cc| Ok(Box::new(ViewerApp::new(dir, data)))),
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
}

impl ViewerApp {
    fn new(dir: String, data: SessionData) -> Self {
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
        }
    }

    /// Rebuild playback timeline + reset navigation after a coin change.
    fn switch_coin(&mut self, coin: &str) {
        self.nav.set_coin(coin);
        self.playback = PlaybackClock::new(timeline_for(&self.data, coin));
        self.play_started = None;
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

        egui::SidePanel::left("coins_ladder")
            .resizable(true)
            .default_width(220.0)
            // Bound the width so a child widget that wants to expand (the coin
            // filter box) can never drive the panel into a grow-every-frame
            // feedback loop that swallows the central book view.
            .width_range(160.0..=400.0)
            .show(ctx, |ui| self.left_panel(ui));

        egui::TopBottomPanel::top("controls").show(ctx, |ui| self.controls(ui));

        egui::CentralPanel::default().show(ctx, |ui| self.book_view(ui));
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
        ui.horizontal_top(|ui| {
            render_side(ui, "BIDS", &book.bids, egui::Color32::from_rgb(80, 200, 120));
            ui.separator();
            render_side(ui, "ASKS", &book.asks, egui::Color32::from_rgb(220, 90, 90));
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
