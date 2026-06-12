//! `hl-viewer` — egui/eframe desktop UI for inspecting recorded sessions.
//!
//! This binary is a *thin* shell: it owns the real wall clock and the egui
//! widgets, and delegates every decision to the pure model in
//! `hl_recorder::viewer` (indexing, navigation, playback math). Run with:
//!
//! ```text
//! hl-viewer <session_dir>      # e.g. hl-viewer sessions/smoke
//! ```

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use hl_recorder::replay::{list_session_coins, session_time_span};
use hl_recorder::viewer::{layout, Navigator, PlaybackClock, Section, SessionData, ViewerConfig};

/// A successfully loaded session slice: the session directory, its full coin
/// list, and the single coin whose books were materialised (lazy per-coin
/// loading — only one coin is held in memory at a time).
struct LoadedSession {
    dir: String,
    coins: Vec<String>,
    coin: String,
    data: SessionData,
}

/// Result delivered from the background loader thread.
type LoadResult = Result<LoadedSession, String>;

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
    init_tracing();
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: hl-viewer <session_dir> [config.yaml]");
        std::process::exit(2);
    });
    let cfg_path = std::env::args().nth(2).unwrap_or_else(|| DEFAULT_CONFIG.to_string());
    let config = ViewerConfig::load_or_default(&cfg_path);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 760.0]),
        ..Default::default()
    };
    // The session is loaded lazily on a background thread (see ViewerApp::new),
    // so the window appears immediately even for multi-GB sessions.
    eframe::run_native(
        "hl-viewer",
        options,
        Box::new(move |_cc| Ok(Box::new(ViewerApp::new(dir, config)))),
    )
}

/// Initialise tracing once. Honours `RUST_LOG` (e.g. `RUST_LOG=debug` for
/// per-file load timings); defaults to `info`.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
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
    /// In-flight background session load. The heavy parquet decode runs off the
    /// UI thread so the window stays responsive; the result arrives here.
    pending_load: Option<Receiver<LoadResult>>,
    /// Cached full mid-price series for the selected coin. Built once per coin
    /// switch (it can be millions of points) so the chart doesn't rebuild it
    /// every frame; the chart downsamples this to the pixel width when drawing.
    price_series: Vec<(i64, f64)>,
    /// Full coin universe for the session (from the manifest), independent of
    /// which single coin's books are currently materialised in `data`.
    all_coins: Vec<String>,
    /// The coin whose books are loaded (or being loaded) in `data`.
    selected_coin: String,
    /// Bounded LRU of recently-loaded coins' books (most-recently-used last).
    /// Switching back to a cached coin is instant — important for huge sessions
    /// where a fresh per-coin load re-scans the whole table (~20s). Cleared when
    /// a new session folder is opened or the load window changes.
    coin_cache: Vec<(String, SessionData)>,
    /// Session wall-clock span `(start_ms, end_ms)` from the manifest, used as
    /// the extent of the window range slider. `None` when unknown.
    session_span: Option<(i64, i64)>,
    /// Active load window `[start_ms, end_ms]` restricting how much of each coin
    /// is loaded. `None` loads the whole coin.
    window: Option<(i64, i64)>,
    /// Working values for the window range slider, applied on "Load window".
    window_edit: (i64, i64),
}

/// Max coins kept in [`ViewerApp::coin_cache`] (excludes the current coin).
/// Bounds memory: on a multi-GB session each coin can be hundreds of MB.
const COIN_CACHE_CAP: usize = 3;

/// One day in milliseconds — the default load window for large sessions.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Default load window for a session of wall-clock `span`: the first 24h when
/// the session is longer than a day, else the whole thing (`None` = no filter).
fn default_window(span: Option<(i64, i64)>) -> Option<(i64, i64)> {
    match span {
        Some((a, b)) if b - a > DAY_MS => Some((a, a + DAY_MS)),
        _ => None,
    }
}

impl ViewerApp {
    /// Build an empty app and kick off the initial lazy load of `dir`.
    fn new(dir: String, config: ViewerConfig) -> Self {
        let mut app = Self {
            dir: String::new(),
            data: SessionData::default(),
            nav: Navigator::new(String::new()),
            playback: PlaybackClock::new(Vec::new()),
            play_started: None,
            coin_filter: String::new(),
            config,
            load_error: None,
            section_order: layout::default_order(),
            pending_load: None,
            price_series: Vec::new(),
            all_coins: Vec::new(),
            selected_coin: String::new(),
            coin_cache: Vec::new(),
            session_span: None,
            window: None,
            window_edit: (0, 0),
        };
        app.open_folder(dir);
        app
    }

    /// Open a *new* session folder. Resets to a clean loading state for the new
    /// directory immediately — clearing the old coin list and data — so a coin
    /// click during the (possibly slow) load can't target the previous session
    /// (the stale-`dir` race). Then discovers coins and loads the first one.
    fn open_folder(&mut self, dir: String) {
        self.dir = dir.clone();
        self.all_coins.clear();
        self.selected_coin.clear();
        self.data = SessionData::default();
        self.nav = Navigator::new(String::new());
        self.playback = PlaybackClock::new(Vec::new());
        self.play_started = None;
        self.price_series.clear();
        // Cached coins belong to the previous session; drop them.
        self.coin_cache.clear();
        // Discover the session span (cheap: manifest only) and default to a 24h
        // window so a multi-day session doesn't load in full.
        self.session_span = session_time_span(&dir);
        self.window = default_window(self.session_span);
        self.window_edit = self.window.or(self.session_span).unwrap_or((0, 0));
        let window = self.window;
        self.start_load(dir, String::new(), None, window);
    }

    /// Select `coin`: serve it instantly from the LRU cache if present,
    /// otherwise start a background load. Used by the coin-list clicks.
    fn select_coin(&mut self, coin: String) {
        if coin == self.selected_coin {
            return;
        }
        if let Some(data) = self.cache_take(&coin) {
            tracing::info!(coin = %coin, "coin served from cache (instant)");
            // A cached hit supersedes any in-flight load.
            self.pending_load = None;
            self.cache_current();
            self.set_current(coin, data);
        } else {
            let dir = self.dir.clone();
            let known = self.all_coins.clone();
            let window = self.window;
            self.start_load(dir, coin, Some(known), window);
        }
    }

    /// Move the currently-loaded coin's books into the LRU cache.
    fn cache_current(&mut self) {
        if self.selected_coin.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.data);
        if data.is_empty() {
            return;
        }
        let coin = std::mem::take(&mut self.selected_coin);
        self.cache_put(coin, data);
    }

    /// Insert/refresh a coin in the LRU cache, evicting the least-recently-used
    /// entries beyond [`COIN_CACHE_CAP`].
    fn cache_put(&mut self, coin: String, data: SessionData) {
        self.coin_cache.retain(|(c, _)| c != &coin);
        self.coin_cache.push((coin, data));
        while self.coin_cache.len() > COIN_CACHE_CAP {
            self.coin_cache.remove(0);
        }
    }

    /// Remove and return a coin's cached books, if present.
    fn cache_take(&mut self, coin: &str) -> Option<SessionData> {
        let pos = self.coin_cache.iter().position(|(c, _)| c == coin)?;
        Some(self.coin_cache.remove(pos).1)
    }

    /// Install `coin`/`data` as the current view, resetting navigation,
    /// playback and the cached price series.
    fn set_current(&mut self, coin: String, data: SessionData) {
        self.data = data;
        self.nav = Navigator::new(coin.clone());
        self.playback = PlaybackClock::new(timeline_for(&self.data, &coin));
        self.play_started = None;
        self.price_series = self.data.price_series(&coin);
        self.selected_coin = coin;
        self.load_error = None;
    }

    /// Start a background load on a worker thread so the UI never blocks on a
    /// multi-GB parquet decode. `coin` empty ⇒ load the first coin of the
    /// (freshly discovered) session; otherwise load that specific coin.
    /// `known_coins` skips re-listing when only switching coin within a session.
    fn start_load(
        &mut self,
        dir: String,
        coin: String,
        known_coins: Option<Vec<String>>,
        window: Option<(i64, i64)>,
    ) {
        tracing::info!(dir = %dir, coin = %coin, ?window, "starting background load");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result: LoadResult = (|| {
                let coins = match known_coins {
                    Some(c) => c,
                    None => list_session_coins(&dir).map_err(|e| format!("{e:#}"))?,
                };
                let coin = if coin.is_empty() {
                    coins.first().cloned().unwrap_or_default()
                } else {
                    coin
                };
                let started = Instant::now();
                let data = if coin.is_empty() {
                    SessionData::default()
                } else {
                    SessionData::from_dir_coin(&dir, &coin, window).map_err(|e| format!("{e:#}"))?
                };
                tracing::info!(
                    coin = %coin,
                    coins = coins.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "coin load complete"
                );
                Ok(LoadedSession {
                    dir,
                    coins,
                    coin,
                    data,
                })
            })();
            if let Err(ref e) = result {
                tracing::error!(error = %e, "background load failed");
            }
            // Receiver gone (a newer load superseded this one) is fine to ignore.
            let _ = tx.send(result);
        });
        self.pending_load = Some(rx);
        self.load_error = None;
    }

    /// Poll the background loader; apply the new session or surface its error.
    /// Returns whether a load is still in flight (so the UI keeps repainting).
    fn poll_pending_load(&mut self, ctx: &egui::Context) -> bool {
        let Some(rx) = &self.pending_load else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(loaded)) => {
                self.pending_load = None;
                self.apply_loaded(loaded);
                false
            }
            Ok(Err(msg)) => {
                self.pending_load = None;
                self.load_error = Some(format!("open failed: {msg}"));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Still loading: keep the event loop ticking so we poll again
                // and the spinner animates, without busy-spinning a core.
                ctx.request_repaint_after(Duration::from_millis(100));
                true
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_load = None;
                self.load_error = Some("open failed: loader thread terminated".into());
                false
            }
        }
    }

    /// Swap in a freshly loaded coin slice, caching the outgoing coin so a
    /// switch back to it is instant.
    fn apply_loaded(&mut self, loaded: LoadedSession) {
        let LoadedSession {
            dir,
            coins,
            coin,
            data,
        } = loaded;
        self.dir = dir;
        self.all_coins = coins;
        self.cache_current();
        self.set_current(coin, data);
        // If we had no manifest span, fall back to the loaded coin's own range
        // so the window slider still has a sensible extent.
        if self.session_span.is_none() {
            self.session_span = self.data.time_range(&self.selected_coin);
        }
        self.window_edit = self
            .window
            .or(self.session_span)
            .unwrap_or(self.window_edit);
    }

    /// Apply the edited window: reload the current coin restricted to it. The
    /// cache is dropped because every coin's data is window-specific.
    fn apply_window(&mut self, window: Option<(i64, i64)>) {
        self.window = window;
        self.coin_cache.clear();
        let dir = self.dir.clone();
        let known = self.all_coins.clone();
        let coin = self.selected_coin.clone();
        self.start_load(dir, coin, Some(known), window);
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
        // Apply any finished background load before drawing this frame.
        self.poll_pending_load(ctx);
        self.drive_playback(ctx);

        // Menu bar spans the full width at the very top.
        let to_open = egui::TopBottomPanel::top("menu_bar")
            .show(ctx, |ui| self.menu_bar(ui))
            .inner;
        if let Some(dir) = to_open {
            // New folder: reset state for the new dir, then load its first coin.
            self.open_folder(dir);
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
        // Coin list comes from the manifest universe (all_coins), independent
        // of which single coin's books are currently loaded.
        let coins = self.all_coins.clone();
        let needle = self.coin_filter.to_lowercase();
        let selected_coin = self.selected_coin.clone();
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
                    let selected = *coin == selected_coin;
                    if ui.selectable_label(selected, coin).clicked() && !selected {
                        pending = Some(coin.clone());
                    }
                }
                if shown == 0 {
                    ui.weak("no matches");
                }
            });
        if let Some(c) = pending {
            // Instant from cache if we've loaded it before, else background load.
            self.select_coin(c);
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

        self.window_controls(ui);
    }

    /// Load-window range control: two handles over the session span select the
    /// `[start, end]` slice loaded for each coin. Reloading is expensive, so the
    /// window only applies when "Load window" is pressed (not on every drag).
    fn window_controls(&mut self, ui: &mut egui::Ui) {
        let Some((span_lo, span_hi)) = self.session_span else {
            return; // unknown span (no manifest): no window slider
        };
        if span_hi <= span_lo {
            return;
        }

        ui.separator();
        let mut apply: Option<Option<(i64, i64)>> = None;
        ui.horizontal(|ui| {
            ui.label("window");
            // Clamp the working values into the span and keep start <= end.
            let (mut lo, mut hi) = self.window_edit;
            lo = lo.clamp(span_lo, span_hi);
            hi = hi.clamp(span_lo, span_hi);

            ui.add(egui::Slider::new(&mut lo, span_lo..=span_hi).show_value(false));
            ui.add(egui::Slider::new(&mut hi, span_lo..=span_hi).show_value(false));
            if lo > hi {
                std::mem::swap(&mut lo, &mut hi);
            }
            self.window_edit = (lo, hi);

            let dirty = self.window != Some((lo, hi));
            if ui
                .add_enabled(dirty, egui::Button::new("Load window"))
                .clicked()
            {
                apply = Some(Some((lo, hi)));
            }
            // Quick presets.
            if ui.button("24h").clicked() {
                let end = (span_lo + DAY_MS).min(span_hi);
                self.window_edit = (span_lo, end);
                apply = Some(Some((span_lo, end)));
            }
            if ui
                .add_enabled(self.window.is_some(), egui::Button::new("Full"))
                .clicked()
            {
                self.window_edit = (span_lo, span_hi);
                apply = Some(None);
            }
        });
        // Show the selected window as UTC, plus its duration in hours.
        let (lo, hi) = self.window_edit;
        let hours = (hi - lo) as f64 / 3_600_000.0;
        ui.monospace(format!(
            "{}  →  {}   ({hours:.1}h)",
            format_utc_ms(lo),
            format_utc_ms(hi)
        ));

        if let Some(window) = apply {
            self.apply_window(window);
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
            if self.pending_load.is_some() {
                ui.separator();
                ui.spinner();
                ui.label("loading session…");
            }
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
        // Use the cached full series; render_price_chart downsamples it to the
        // available pixel width before drawing.
        render_price_chart(ui, &self.price_series, current_ts, &colors)
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
