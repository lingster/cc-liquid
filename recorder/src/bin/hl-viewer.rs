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
use render::{render_side, render_top_of_book, timeline_for};

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
        let coins = self.data.coins();
        let mut pending: Option<String> = None;
        for coin in &coins {
            let selected = coin == self.nav.coin();
            if ui.selectable_label(selected, coin).clicked() && !selected {
                pending = Some(coin.clone());
            }
        }
        if let Some(c) = pending {
            self.switch_coin(&c);
        }

        ui.separator();
        ui.label("price ladder (all ticks)");
        let ladder = self.data.price_ladder(self.nav.coin());
        egui::ScrollArea::vertical().show(ui, |ui| {
            // Show highest price first, like a depth ladder.
            for px in ladder.iter().rev() {
                ui.monospace(format!("{px:>12.4}"));
            }
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

        ui.columns(2, |cols| {
            render_side(
                &mut cols[0],
                "BIDS",
                &book.bids,
                egui::Color32::from_rgb(80, 200, 120),
            );
            render_side(
                &mut cols[1],
                "ASKS",
                &book.asks,
                egui::Color32::from_rgb(220, 90, 90),
            );
        });
    }
}
