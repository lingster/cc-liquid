//! Playback market-data source (PRD §B.6): serves `/info` market reads from a
//! recorded session instead of the live exchange.
//!
//! The provider folds the recorded event stream into a [`MarketState`] with the
//! same deterministic semantics as the replay engine. The cursor advances as
//! the app polls: each `all_mids()` call reveals the next recorded mids tick
//! (folding any interleaved book/trade events along the way). Behaviour past
//! the end of the window is configurable (PRD §6): `stop` and `hold` freeze
//! the final state (`stop` additionally tells the sim to reject new orders),
//! `loop` rewinds and replays the window.

use std::path::Path;
use std::str::FromStr;

use anyhow::Context;
use serde_json::{json, Map, Value};

use crate::events::{MarketEvent, RecordedEvent, Trade};
use crate::manifest::Manifest;
use crate::replay::state::MarketState;

/// Optional per-session file holding a verbatim live `meta` response captured
/// at record time. When absent, a universe is synthesized from the manifest.
pub const META_FILE: &str = "meta.json";

/// `szDecimals` used for synthesized universe entries when the session has no
/// captured `meta.json`.
pub const DEFAULT_SZ_DECIMALS: u32 = 4;

/// What happens when the recorded window is exhausted (PRD §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndOfWindow {
    /// Freeze the final state; the sim rejects orders placed past the end.
    #[default]
    Stop,
    /// Freeze the final book/mids and keep accepting orders against it.
    Hold,
    /// Rewind and replay the window on repeat (price seam documented).
    Loop,
}

impl FromStr for EndOfWindow {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "stop" => Ok(EndOfWindow::Stop),
            "hold" => Ok(EndOfWindow::Hold),
            "loop" => Ok(EndOfWindow::Loop),
            other => Err(format!(
                "unknown end-of-window mode `{other}` (use stop|hold|loop)"
            )),
        }
    }
}

/// Answers the playback-servable market reads (`allMids`, `meta`, `spotMeta`)
/// and exposes the folded market state for the simulation engine.
pub trait MarketDataProvider: Send {
    /// Current mids snapshot, advancing the playback cursor by one mids tick.
    /// Shape matches live: `{"BTC": "95000.0", ...}` (price strings).
    fn all_mids(&mut self) -> Value;
    /// Perp universe, shape matches live `meta`: `{"universe":[...]}`.
    fn meta(&mut self) -> Value;
    /// Spot universe. The twin is perp-only, so this is empty-but-valid:
    /// `{"universe":[],"tokens":[]}` — enough for SDK construction.
    fn spot_meta(&mut self) -> Value;
    /// The folded market state at the cursor (sim matching substrate).
    /// `None` for providers without replayed state.
    fn state(&self) -> Option<&MarketState> {
        None
    }
    /// Public trades folded since the previous call (sim queue accounting).
    fn take_trades(&mut self) -> Vec<Trade> {
        Vec::new()
    }
    /// Whether the window ended under [`EndOfWindow::Stop`].
    fn is_stopped(&self) -> bool {
        false
    }
}

/// Playback over a recorded (or synthetic) event sequence.
pub struct PlaybackMarket {
    events: Vec<RecordedEvent>,
    cursor: usize,
    state: MarketState,
    /// Coins the session covers (used to backfill book-derived prices).
    coins: Vec<String>,
    meta: Value,
    /// Whether the stream contains any `allMids` events; when it does not, the
    /// cursor advances one event per poll instead of seeking the next mids tick.
    mids_driven: bool,
    end_of_window: EndOfWindow,
    ended: bool,
    /// Trades folded since the last `take_trades()`.
    pending_trades: Vec<Trade>,
}

impl PlaybackMarket {
    /// Build from in-memory events (tests, synthetic feeds).
    pub fn from_events(events: Vec<RecordedEvent>, coins: Vec<String>, meta: Value) -> Self {
        let mids_driven = events
            .iter()
            .any(|e| matches!(e.payload, MarketEvent::AllMids(_)));
        Self {
            events,
            cursor: 0,
            state: MarketState::new(),
            coins,
            meta,
            mids_driven,
            end_of_window: EndOfWindow::default(),
            ended: false,
            pending_trades: Vec::new(),
        }
    }

    /// Load a recorded session directory: Parquet event tables + `manifest.json`,
    /// with an optional `meta.json` overriding the synthesized universe.
    pub fn load(session_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = session_dir.as_ref();
        let events = crate::replay::load_session(dir)
            .with_context(|| format!("loading session from {}", dir.display()))?;
        anyhow::ensure!(
            !events.is_empty(),
            "session at {} contains no events",
            dir.display()
        );

        let manifest_path = dir.join("manifest.json");
        let manifest = Manifest::from_json(
            &std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("reading {}", manifest_path.display()))?,
        )?;

        let meta_path = dir.join(META_FILE);
        let meta = if meta_path.is_file() {
            serde_json::from_str(&std::fs::read_to_string(&meta_path)?)
                .with_context(|| format!("parsing {}", meta_path.display()))?
        } else {
            synthesize_meta(&manifest.coins, DEFAULT_SZ_DECIMALS)
        };

        Ok(Self::from_events(events, manifest.coins, meta))
    }

    /// Set the end-of-window behaviour (builder style).
    pub fn with_end_of_window(mut self, mode: EndOfWindow) -> Self {
        self.end_of_window = mode;
        self
    }

    fn next_event_idx(&mut self) -> Option<usize> {
        if self.cursor >= self.events.len() {
            match self.end_of_window {
                EndOfWindow::Loop if !self.events.is_empty() => self.cursor = 0,
                EndOfWindow::Stop => {
                    self.ended = true;
                    return None;
                }
                _ => return None,
            }
        }
        let idx = self.cursor;
        self.cursor += 1;
        Some(idx)
    }

    /// Advance the cursor: fold events until the next `allMids` tick has been
    /// applied (or a single event when the stream has no mids).
    fn advance(&mut self) {
        loop {
            let Some(idx) = self.next_event_idx() else {
                return; // end of window (stop/hold): final state persists
            };
            let ev = &self.events[idx];
            if let MarketEvent::Trades(trades) = &ev.payload {
                self.pending_trades.extend(trades.iter().cloned());
            }
            let was_mids = matches!(ev.payload, MarketEvent::AllMids(_));
            self.state.apply(ev);
            if was_mids || !self.mids_driven {
                return;
            }
        }
    }
}

impl MarketDataProvider for PlaybackMarket {
    fn all_mids(&mut self) -> Value {
        self.advance();
        let mut out = Map::new();
        for (coin, px) in self.state.all_mids() {
            out.insert(coin, Value::String(format_px(px)));
        }
        // Backfill session coins whose price is only known from books/trades.
        for coin in &self.coins {
            if !out.contains_key(coin) {
                if let Some(px) = self.state.price(coin) {
                    out.insert(coin.clone(), Value::String(format_px(px)));
                }
            }
        }
        Value::Object(out)
    }

    fn meta(&mut self) -> Value {
        self.meta.clone()
    }

    fn spot_meta(&mut self) -> Value {
        json!({"universe": [], "tokens": []})
    }

    fn state(&self) -> Option<&MarketState> {
        Some(&self.state)
    }

    fn take_trades(&mut self) -> Vec<Trade> {
        std::mem::take(&mut self.pending_trades)
    }

    fn is_stopped(&self) -> bool {
        self.ended
    }
}

/// Render a price the way Hyperliquid does: a decimal string.
fn format_px(px: f64) -> String {
    let s = format!("{px}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Build a live-shaped `meta` response from a plain coin list.
pub fn synthesize_meta(coins: &[String], sz_decimals: u32) -> Value {
    let universe: Vec<Value> = coins
        .iter()
        .map(|c| {
            json!({
                "name": c,
                "szDecimals": sz_decimals,
                "maxLeverage": 50,
                "isDelisted": false,
            })
        })
        .collect();
    json!({ "universe": universe })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book, Level, MarketEvent, RecordedEvent};

    fn mids_ev(seq: u64, prices: &[(&str, f64)]) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 1000 + seq as i64,
            payload: MarketEvent::AllMids(AllMids {
                mids: prices.iter().map(|(c, p)| (c.to_string(), *p)).collect(),
            }),
        }
    }

    fn book_ev(seq: u64, coin: &str, bid: f64, ask: f64) -> RecordedEvent {
        let level = |px| Level { px, sz: 1.0, n: 1 };
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 1000 + seq as i64,
            payload: MarketEvent::L2Book(L2Book {
                coin: coin.into(),
                time_ms: 1000 + seq as i64,
                bids: vec![level(bid)],
                asks: vec![level(ask)],
            }),
        }
    }

    #[test]
    fn each_poll_reveals_the_next_recorded_mids_tick() {
        let events = vec![
            mids_ev(0, &[("BTC", 95000.0)]),
            book_ev(1, "BTC", 94990.0, 95010.0),
            mids_ev(2, &[("BTC", 95100.0)]),
        ];
        let mut m = PlaybackMarket::from_events(events, vec!["BTC".into()], json!({}));
        assert_eq!(m.all_mids()["BTC"], "95000.0");
        // Second poll folds the interleaved book event and the next mids tick.
        assert_eq!(m.all_mids()["BTC"], "95100.0");
    }

    #[test]
    fn holds_final_state_past_end_of_window() {
        let events = vec![mids_ev(0, &[("ETH", 3200.5)])];
        let mut m = PlaybackMarket::from_events(events, vec!["ETH".into()], json!({}));
        assert_eq!(m.all_mids()["ETH"], "3200.5");
        // Stream exhausted: keeps serving the last snapshot, never errors.
        assert_eq!(m.all_mids()["ETH"], "3200.5");
        assert_eq!(m.all_mids()["ETH"], "3200.5");
    }

    #[test]
    fn backfills_book_only_coins_from_book_mid() {
        let events = vec![
            mids_ev(0, &[("BTC", 95000.0)]),
            book_ev(1, "SOL", 149.0, 151.0),
            mids_ev(2, &[("BTC", 95001.0)]),
        ];
        let mut m =
            PlaybackMarket::from_events(events, vec!["BTC".into(), "SOL".into()], json!({}));
        let first = m.all_mids();
        assert_eq!(first["BTC"], "95000.0");
        assert!(first.get("SOL").is_none(), "no SOL data folded yet");
        let second = m.all_mids();
        assert_eq!(second["SOL"], "150.0", "book mid backfilled");
    }

    #[test]
    fn mids_free_sessions_advance_one_event_per_poll() {
        let events = vec![
            book_ev(0, "BTC", 100.0, 102.0),
            book_ev(1, "BTC", 104.0, 106.0),
        ];
        let mut m = PlaybackMarket::from_events(events, vec!["BTC".into()], json!({}));
        assert_eq!(m.all_mids()["BTC"], "101.0");
        assert_eq!(m.all_mids()["BTC"], "105.0");
        assert_eq!(m.all_mids()["BTC"], "105.0"); // hold
    }

    #[test]
    fn prices_render_as_decimal_strings_like_live() {
        let events = vec![mids_ev(0, &[("BTC", 95000.0), ("ETH", 3200.5)])];
        let mut m =
            PlaybackMarket::from_events(events, vec!["BTC".into(), "ETH".into()], json!({}));
        let mids = m.all_mids();
        assert_eq!(mids["BTC"], "95000.0");
        assert_eq!(mids["ETH"], "3200.5");
    }

    #[test]
    fn synthesized_meta_has_live_shape() {
        let meta = synthesize_meta(&["BTC".into(), "ETH".into()], 4);
        let universe = meta["universe"].as_array().unwrap();
        assert_eq!(universe.len(), 2);
        assert_eq!(universe[0]["name"], "BTC");
        assert_eq!(universe[0]["szDecimals"], 4);
        assert_eq!(universe[0]["isDelisted"], false);
    }

    #[test]
    fn spot_meta_is_empty_but_valid_for_sdk_construction() {
        let mut m = PlaybackMarket::from_events(
            vec![mids_ev(0, &[("BTC", 1.0)])],
            vec!["BTC".into()],
            json!({}),
        );
        let sm = m.spot_meta();
        assert_eq!(sm["universe"], json!([]));
        assert_eq!(sm["tokens"], json!([]));
    }

    #[test]
    fn loads_a_recorded_parquet_session_with_synthesized_meta() {
        use crate::manifest::{Counts, Manifest, SCHEMA_VERSION};
        use crate::sink::EventSink;
        use crate::storage::ParquetSink;

        let dir = tempfile::tempdir().unwrap();
        let mut sink = ParquetSink::create(dir.path()).unwrap();
        sink.write(&mids_ev(0, &[("BTC", 95000.0)])).unwrap();
        sink.write(&mids_ev(1, &[("BTC", 95050.0)])).unwrap();
        sink.finalize().unwrap();
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            network: "mainnet".into(),
            endpoint: "wss://api.hyperliquid.xyz/ws".into(),
            coins: vec!["BTC".into()],
            streams: vec!["allMids".into()],
            started_at: "2026-06-09T00:00:00Z".into(),
            ended_at: "2026-06-09T00:05:00Z".into(),
            duration_secs: 300,
            counts: Counts {
                recorded: 2,
                ignored: 0,
                parse_errors: 0,
                all_mids: 2,
                l2_book: 0,
                trades: 0,
            },
            recorder_version: "0.1.0".into(),
        };
        std::fs::write(
            dir.path().join("manifest.json"),
            manifest.to_json().unwrap(),
        )
        .unwrap();

        let mut m = PlaybackMarket::load(dir.path()).unwrap();
        assert_eq!(m.all_mids()["BTC"], "95000.0");
        assert_eq!(m.all_mids()["BTC"], "95050.0");
        let meta = m.meta();
        assert_eq!(meta["universe"][0]["name"], "BTC");
    }

    #[test]
    fn meta_json_in_session_dir_overrides_synthesized_universe() {
        use crate::sink::EventSink;
        use crate::storage::ParquetSink;

        let dir = tempfile::tempdir().unwrap();
        let mut sink = ParquetSink::create(dir.path()).unwrap();
        sink.write(&mids_ev(0, &[("BTC", 1.0)])).unwrap();
        sink.finalize().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{"schema_version":1,"network":"mainnet","endpoint":"","coins":["BTC"],
                "streams":["allMids"],"started_at":"","ended_at":"","duration_secs":0,
                "counts":{"recorded":1,"ignored":0,"parse_errors":0,"all_mids":1,"l2_book":0,"trades":0},
                "recorder_version":"0.1.0"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(META_FILE),
            r#"{"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]}"#,
        )
        .unwrap();

        let mut m = PlaybackMarket::load(dir.path()).unwrap();
        assert_eq!(m.meta()["universe"][0]["szDecimals"], 5);
    }

    #[test]
    fn empty_session_fails_to_load() {
        use crate::storage::ParquetSink;
        let dir = tempfile::tempdir().unwrap();
        let sink = ParquetSink::create(dir.path()).unwrap();
        crate::sink::EventSink::finalize(&mut { sink }).unwrap();
        assert!(PlaybackMarket::load(dir.path()).is_err());
    }
}

#[cfg(test)]
mod end_of_window_tests {
    use super::*;
    use crate::events::{AllMids, Side};

    fn mids_ev(seq: u64, px: f64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 1000 + seq as i64,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), px)],
            }),
        }
    }

    fn trades_ev(seq: u64, px: f64, sz: f64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 1000 + seq as i64,
            payload: MarketEvent::Trades(vec![Trade {
                coin: "BTC".into(),
                side: Side::Sell,
                px,
                sz,
                time_ms: 1000 + seq as i64,
            }]),
        }
    }

    fn market(mode: EndOfWindow) -> PlaybackMarket {
        PlaybackMarket::from_events(
            vec![mids_ev(0, 100.0), mids_ev(1, 101.0)],
            vec!["BTC".into()],
            json!({}),
        )
        .with_end_of_window(mode)
    }

    #[test]
    fn stop_holds_final_state_and_flags_ended() {
        let mut m = market(EndOfWindow::Stop);
        assert_eq!(m.all_mids()["BTC"], "100.0");
        assert_eq!(m.all_mids()["BTC"], "101.0");
        assert!(!m.is_stopped(), "not yet past the end");
        assert_eq!(m.all_mids()["BTC"], "101.0", "held final state");
        assert!(m.is_stopped());
    }

    #[test]
    fn hold_keeps_serving_without_stopping() {
        let mut m = market(EndOfWindow::Hold);
        m.all_mids();
        m.all_mids();
        assert_eq!(m.all_mids()["BTC"], "101.0");
        assert!(!m.is_stopped(), "hold never stops accepting");
    }

    #[test]
    fn loop_rewinds_to_the_start_of_the_window() {
        let mut m = market(EndOfWindow::Loop);
        assert_eq!(m.all_mids()["BTC"], "100.0");
        assert_eq!(m.all_mids()["BTC"], "101.0");
        assert_eq!(m.all_mids()["BTC"], "100.0", "looped back to tick 0");
        assert!(!m.is_stopped());
    }

    #[test]
    fn trades_folded_during_advance_are_collectable_once() {
        let mut m = PlaybackMarket::from_events(
            vec![
                mids_ev(0, 100.0),
                trades_ev(1, 99.5, 0.4),
                mids_ev(2, 101.0),
            ],
            vec!["BTC".into()],
            json!({}),
        );
        m.all_mids(); // tick 0: no trades yet
        assert!(m.take_trades().is_empty());
        m.all_mids(); // folds the trade batch + tick 2
        let trades = m.take_trades();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].px, 99.5);
        assert!(m.take_trades().is_empty(), "drained");
    }

    #[test]
    fn state_exposes_the_cursor_market() {
        let mut m = market(EndOfWindow::Hold);
        m.all_mids();
        assert_eq!(m.state().unwrap().price("BTC"), Some(100.0));
    }

    #[test]
    fn end_of_window_parses() {
        assert_eq!("stop".parse::<EndOfWindow>().unwrap(), EndOfWindow::Stop);
        assert_eq!("LOOP".parse::<EndOfWindow>().unwrap(), EndOfWindow::Loop);
        assert!("rewind".parse::<EndOfWindow>().is_err());
    }
}
