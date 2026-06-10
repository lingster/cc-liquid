//! Playback market-data source (PRD §B.6): serves `/info` market reads from a
//! recorded session instead of the live exchange.
//!
//! The provider folds the recorded event stream into a [`MarketState`] with the
//! same deterministic semantics as the replay engine. The cursor advances as
//! the app polls: each `all_mids()` call reveals the next recorded mids tick
//! (folding any interleaved book/trade events along the way). Past the end of
//! the window the final state is held, so a polling client keeps getting
//! consistent prices instead of errors.

use std::path::Path;

use anyhow::Context;
use serde_json::{json, Map, Value};

use crate::events::{MarketEvent, RecordedEvent};
use crate::manifest::Manifest;
use crate::replay::state::MarketState;
use crate::replay::stream::{EventStream, VecEventStream};

/// Optional per-session file holding a verbatim live `meta` response captured
/// at record time. When absent, a universe is synthesized from the manifest.
pub const META_FILE: &str = "meta.json";

/// `szDecimals` used for synthesized universe entries when the session has no
/// captured `meta.json`.
pub const DEFAULT_SZ_DECIMALS: u32 = 4;

/// Answers the playback-servable market reads (`allMids`, `meta`, `spotMeta`).
pub trait MarketDataProvider: Send {
    /// Current mids snapshot, advancing the playback cursor by one mids tick.
    /// Shape matches live: `{"BTC": "95000.0", ...}` (price strings).
    fn all_mids(&mut self) -> Value;
    /// Perp universe, shape matches live `meta`: `{"universe":[...]}`.
    fn meta(&mut self) -> Value;
    /// Spot universe. The twin is perp-only, so this is empty-but-valid:
    /// `{"universe":[],"tokens":[]}` — enough for SDK construction.
    fn spot_meta(&mut self) -> Value;
}

/// Playback over a recorded (or synthetic) event sequence.
pub struct PlaybackMarket {
    stream: VecEventStream,
    state: MarketState,
    /// Coins the session covers (used to backfill book-derived prices).
    coins: Vec<String>,
    meta: Value,
    /// Whether the stream contains any `allMids` events; when it does not, the
    /// cursor advances one event per poll instead of seeking the next mids tick.
    mids_driven: bool,
}

impl PlaybackMarket {
    /// Build from in-memory events (tests, synthetic feeds).
    pub fn from_events(events: Vec<RecordedEvent>, coins: Vec<String>, meta: Value) -> Self {
        let mids_driven = events
            .iter()
            .any(|e| matches!(e.payload, MarketEvent::AllMids(_)));
        Self {
            stream: VecEventStream::new(events),
            state: MarketState::new(),
            coins,
            meta,
            mids_driven,
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

    /// Advance the cursor: fold events until the next `allMids` tick has been
    /// applied (or a single event when the stream has no mids). Holds at the
    /// end of the window.
    fn advance(&mut self) {
        loop {
            let Some(ev) = self.stream.next_event() else {
                return; // end of window: hold final state
            };
            let was_mids = matches!(ev.payload, MarketEvent::AllMids(_));
            self.state.apply(&ev);
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
