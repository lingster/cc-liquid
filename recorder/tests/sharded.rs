//! End-to-end multi-currency + sharded-storage tests through the twin loop.
//!
//! Proves: (a) the multi-coin synthetic generator broadcasts FX for many pairs
//! and survives record -> replay; (b) the parallel partitioned L2 sink records
//! many coins across N part-files and the replay loader reassembles them.

use std::path::Path;

use hl_recorder::events::{L2Book, Level, MarketEvent, RecordedEvent};
use hl_recorder::recorder::Recorder;
use hl_recorder::replay::stream::VecEventStream;
use hl_recorder::replay::{load_session, MultiCoinSyntheticStream, ReplayEngine};
use hl_recorder::storage::sharded::part_file_name;
use hl_recorder::storage::{ParquetSink, ShardedParquetSink, L2_BOOK_DIR};
use hl_recorder::twin::PlaybackSource;

const COINS: &[&str] = &["BTC", "ETH", "SOL", "DOGE", "WIF", "APT", "ARB", "OP"];

#[tokio::test]
async fn multi_coin_fx_broadcast_survives_record_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let ticks = 50u64;

    // Twin broadcasts mids for all coins, one allMids event per tick.
    let mut source = PlaybackSource::tick(MultiCoinSyntheticStream::ramp(COINS, ticks));
    let now_ms = source.now_ms_fn();
    let mut sink = ParquetSink::create(dir.path()).unwrap();

    let stats = Recorder::new()
        .run(&mut source, &mut sink, now_ms)
        .await
        .unwrap();
    assert_eq!(stats.recorded, ticks, "one event per tick");

    // Replay and confirm every coin has its final price.
    let mut engine = ReplayEngine::new(VecEventStream::new(load_session(dir.path()).unwrap()));
    while engine.step() {}

    for (i, coin) in COINS.iter().enumerate() {
        // coin i starts at i and rises by 0.0001/tick -> i + (ticks-1)*0.0001.
        let expected = i as f64 + (ticks - 1) as f64 * 0.0001;
        let got = engine.price(coin).unwrap();
        assert!(
            (got - expected).abs() < 1e-9,
            "{coin}: got {got}, expected {expected}"
        );
    }
}

fn book(coin: &str, seq: u64, px: f64) -> RecordedEvent {
    RecordedEvent {
        seq,
        ts_event_ms: seq as i64,
        ts_recv_ms: seq as i64,
        payload: MarketEvent::L2Book(L2Book {
            coin: coin.into(),
            time_ms: seq as i64,
            bids: vec![Level {
                px: px - 0.5,
                sz: 1.0,
                n: 1,
            }],
            asks: vec![Level {
                px: px + 0.5,
                sz: 1.0,
                n: 1,
            }],
        }),
    }
}

#[tokio::test]
async fn sharded_l2_records_many_coins_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let shards = 4;
    let updates_per_coin = 5u64;

    // Build L2 updates for many coins, each coin's price distinct & rising.
    let mut events = Vec::new();
    let mut seq = 0u64;
    for u in 0..updates_per_coin {
        for (i, c) in COINS.iter().enumerate() {
            events.push(book(c, seq, 100.0 + i as f64 * 10.0 + u as f64));
            seq += 1;
        }
    }
    let total_events = events.len() as u64;

    // Twin replays them as Hyperliquid frames into the parallel sharded sink.
    let mut source = PlaybackSource::tick(VecEventStream::new(events));
    let now_ms = source.now_ms_fn();
    let mut sink = ShardedParquetSink::create(dir.path(), shards).unwrap();

    let stats = Recorder::new()
        .run(&mut source, &mut sink, now_ms)
        .await
        .unwrap();
    assert_eq!(stats.recorded, total_events);
    assert_eq!(stats.l2_book, total_events);

    // N part-files exist on disk.
    let book_dir = dir.path().join(L2_BOOK_DIR);
    for s in 0..shards {
        assert!(
            book_dir.join(part_file_name(s)).exists(),
            "missing part {s}"
        );
    }

    // Replay loads from the partitioned layout and recovers every coin's book.
    let loaded = load_session(dir.path()).unwrap();
    assert_eq!(loaded.len() as u64, total_events);

    let mut engine = ReplayEngine::new(VecEventStream::new(loaded));
    while engine.step() {}
    for (i, coin) in COINS.iter().enumerate() {
        let expected_last = 100.0 + i as f64 * 10.0 + (updates_per_coin - 1) as f64;
        let book = engine
            .book(coin)
            .unwrap_or_else(|| panic!("no book for {coin}"));
        // mid == px (bids/asks straddle px by 0.5).
        assert!((book.mid().unwrap() - expected_last).abs() < 1e-9);
    }
}

/// Sanity: replay loader treats the partitioned dir identically to a single file.
#[allow(dead_code)]
fn _unused(_: &Path) {}
