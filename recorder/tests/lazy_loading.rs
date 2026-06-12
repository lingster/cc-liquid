//! Lazy per-coin loading: the viewer must be able to list coins cheaply and
//! materialise a single coin's books without loading the whole session.

use hl_recorder::events::{L2Book, Level, MarketEvent, RecordedEvent};
use hl_recorder::replay::{list_session_coins, load_coin_session, session_time_span};
use hl_recorder::sink::EventSink;
use hl_recorder::storage::ParquetSink;
use hl_recorder::viewer::SessionData;

fn book(seq: u64, ts: i64, coin: &str, bid: f64, ask: f64) -> RecordedEvent {
    RecordedEvent {
        seq,
        ts_event_ms: ts,
        ts_recv_ms: ts,
        payload: MarketEvent::L2Book(L2Book {
            coin: coin.into(),
            time_ms: ts,
            bids: vec![Level {
                px: bid,
                sz: 1.0,
                n: 1,
            }],
            asks: vec![Level {
                px: ask,
                sz: 2.0,
                n: 1,
            }],
        }),
    }
}

/// Write a small multi-coin session (no manifest) to a temp dir.
fn write_session() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let events = vec![
        book(0, 10, "BTC", 100.0, 102.0),
        book(1, 20, "ETH", 50.0, 51.0),
        book(2, 30, "BTC", 104.0, 106.0),
        book(3, 40, "ETH", 52.0, 53.0),
        book(4, 50, "BTC", 108.0, 110.0),
    ];
    let mut sink = ParquetSink::create(dir.path()).unwrap();
    for e in &events {
        sink.write(e).unwrap();
    }
    sink.finalize().unwrap();
    dir
}

#[test]
fn list_session_coins_scans_l2_book_when_no_manifest() {
    let dir = write_session();
    // No manifest written by the sink, so this exercises the scan fallback.
    let coins = list_session_coins(dir.path()).unwrap();
    assert_eq!(coins, vec!["BTC".to_string(), "ETH".to_string()]);
}

#[test]
fn list_session_coins_prefers_manifest() {
    let dir = write_session();
    // A manifest with a different (broader) coin set must win over a scan.
    std::fs::write(
        dir.path().join("manifest.json"),
        r#"{"coins":["ETH","BTC","SOL"]}"#,
    )
    .unwrap();
    let coins = list_session_coins(dir.path()).unwrap();
    assert_eq!(
        coins,
        vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()]
    );
}

#[test]
fn load_coin_session_returns_only_that_coin_in_seq_order() {
    let dir = write_session();
    let btc = load_coin_session(dir.path(), "BTC", None).unwrap();
    assert_eq!(btc.len(), 3);
    // Ordered by seq, only BTC books.
    let coins: Vec<&str> = btc
        .iter()
        .map(|e| match &e.payload {
            MarketEvent::L2Book(b) => b.coin.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(coins, vec!["BTC", "BTC", "BTC"]);
    assert!(btc.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[test]
fn from_dir_coin_indexes_a_single_coin_only() {
    let dir = write_session();
    let data = SessionData::from_dir_coin(dir.path(), "ETH", None).unwrap();
    // Only ETH is present; BTC is never materialised.
    assert_eq!(data.coins(), vec!["ETH".to_string()]);
    assert_eq!(data.tick_count("ETH"), 2);
    assert_eq!(data.tick_count("BTC"), 0);
    // Mid series: (20, 50.5), (40, 52.5).
    assert_eq!(data.price_series("ETH"), vec![(20, 50.5), (40, 52.5)]);
}

#[test]
fn load_coin_session_unknown_coin_is_empty() {
    let dir = write_session();
    assert!(load_coin_session(dir.path(), "DOGE", None)
        .unwrap()
        .is_empty());
}

#[test]
fn load_coin_session_window_restricts_to_time_slice() {
    // BTC books are at ts 10, 30, 50. A [25, 45] window keeps only ts=30.
    let dir = write_session();
    let btc = load_coin_session(dir.path(), "BTC", Some((25, 45))).unwrap();
    assert_eq!(btc.len(), 1);
    assert_eq!(btc[0].ts_event_ms, 30);
    // A window before all data yields nothing.
    assert!(load_coin_session(dir.path(), "BTC", Some((0, 5)))
        .unwrap()
        .is_empty());
    // A window covering everything matches the unwindowed load.
    assert_eq!(
        load_coin_session(dir.path(), "BTC", Some((0, 1000)))
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn session_time_span_reads_manifest_timestamps() {
    let dir = write_session();
    // No manifest yet → None.
    assert_eq!(session_time_span(dir.path()), None);
    std::fs::write(
        dir.path().join("manifest.json"),
        r#"{"started_at":"2026-01-02T03:04:05Z","ended_at":"2026-01-03T03:04:05Z"}"#,
    )
    .unwrap();
    let (a, b) = session_time_span(dir.path()).unwrap();
    assert_eq!(b - a, 24 * 60 * 60 * 1000); // exactly 24h apart
}
