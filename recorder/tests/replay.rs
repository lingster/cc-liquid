//! End-to-end replay tests across the three playback modes.
//!
//! Mode 1 (tick + recorded): write a session, load it, step through it.
//! Mode 2 (synthetic ramp + tick): price 0.0 +0.0001 per tick.
//! Mode 3 (time-to-price + realtime): price tracks the clock as `ss.mmm`.

use hl_recorder::events::{AllMids, L2Book, Level, MarketEvent, RecordedEvent};
use hl_recorder::replay::{
    load_session_stream, ManualClock, ReplayEngine, SyntheticEventStream, TimeRampEventStream,
};
use hl_recorder::sink::EventSink;
use hl_recorder::storage::ParquetSink;

fn rec(seq: u64, ts: i64, payload: MarketEvent) -> RecordedEvent {
    RecordedEvent {
        seq,
        ts_event_ms: ts,
        ts_recv_ms: ts,
        payload,
    }
}

#[tokio::test]
async fn recorded_session_replays_in_tick_mode() {
    let dir = tempfile::tempdir().unwrap();

    // Write a tiny session: a mid update then a book that moves the price.
    let events = vec![
        rec(
            0,
            10,
            MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), 100.0)],
            }),
        ),
        rec(
            1,
            20,
            MarketEvent::L2Book(L2Book {
                coin: "BTC".into(),
                time_ms: 20,
                bids: vec![Level {
                    px: 200.0,
                    sz: 1.0,
                    n: 1,
                }],
                asks: vec![Level {
                    px: 202.0,
                    sz: 1.0,
                    n: 1,
                }],
            }),
        ),
    ];
    let mut sink = ParquetSink::create(dir.path()).unwrap();
    for e in &events {
        sink.write(e).unwrap();
    }
    sink.finalize().unwrap();

    // Load and replay in tick mode.
    let stream = load_session_stream(dir.path()).unwrap();
    let mut engine = ReplayEngine::new(stream);

    assert!(engine.step());
    assert_eq!(engine.price("BTC"), Some(100.0)); // from allMids
    assert_eq!(engine.cursor_ts_ms(), Some(10));

    assert!(engine.step());
    // allMids still pins price to 100 even though the book mid is 201,
    // because explicit mids take precedence (documented in MarketState::price).
    assert_eq!(engine.price("BTC"), Some(100.0));
    assert_eq!(engine.book("BTC").map(|b| b.best_ask()), Some(Some(202.0)));

    assert!(!engine.step());
    assert!(engine.is_ended());
}

#[test]
fn synthetic_ramp_drives_tick_mode_full() {
    let mut engine = ReplayEngine::new(SyntheticEventStream::ramp("TEST", 5));
    let mut prices = Vec::new();
    while engine.step() {
        prices.push(engine.price("TEST").unwrap());
    }
    assert_eq!(prices.len(), 5);
    assert_eq!(prices[0], 0.0);
    assert!((prices[4] - 0.0004).abs() < 1e-12);
}

#[tokio::test]
async fn time_mode_tracks_clock_in_realtime() {
    // Tick every 250ms from t=10_000ms: prices 10.000, 10.250, 10.500, 10.750.
    let stream = TimeRampEventStream::new("TEST", 10_000, 250, Some(4));
    let mut engine = ReplayEngine::new(stream);
    let clock = ManualClock::new();

    let mut prices = Vec::new();
    let ticks = engine
        .run_realtime(&clock, |st| prices.push(st.price("TEST").unwrap()))
        .await;

    assert_eq!(ticks, 4);
    assert!((prices[0] - 10.000).abs() < 1e-12);
    assert!((prices[1] - 10.250).abs() < 1e-12);
    assert!((prices[3] - 10.750).abs() < 1e-12);
    // Realtime pacing waited 250ms between each of the 4 ticks (3 gaps).
    assert_eq!(clock.sleeps(), vec![250, 250, 250]);
}
