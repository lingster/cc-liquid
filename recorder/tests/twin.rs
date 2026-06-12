//! Digital-twin loop tests: playback -> recorder -> Parquet.
//!
//! These prove the twin (a synthetic playback stream serialized as Hyperliquid
//! frames) feeds the recorder through the real `EventSource`/`EventSink` path
//! and produces a Parquet session with exactly N sequentially-numbered ticks
//! whose timestamps match the saved prices.

use std::fs::File;
use std::path::Path;

use arrow::array::{Float64Array, Int64Array, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use hl_recorder::recorder::Recorder;
use hl_recorder::replay::synthetic::{time_to_price, SyntheticEventStream, TimeRampEventStream};
use hl_recorder::storage::parquet_sink::ALL_MIDS_FILE;
use hl_recorder::storage::ParquetSink;
use hl_recorder::twin::PlaybackSource;

const N: u64 = 1000;

/// Load the `all_mids` table as `(seq, ts_event_ms, mid)` rows, in file order.
fn load_mids(dir: &Path) -> Vec<(u64, i64, f64)> {
    let file = File::open(dir.join(ALL_MIDS_FILE)).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let seq = batch
            .column_by_name("seq")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        let ts = batch
            .column_by_name("ts_event_ms")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let mid = batch
            .column_by_name("mid")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        for i in 0..batch.num_rows() {
            rows.push((seq.value(i), ts.value(i), mid.value(i)));
        }
    }
    rows
}

fn assert_sequential(rows: &[(u64, i64, f64)], n: u64) {
    assert_eq!(rows.len() as u64, n, "expected exactly {n} recorded ticks");
    for (expected_seq, row) in rows.iter().enumerate() {
        assert_eq!(
            row.0, expected_seq as u64,
            "seq must be gap-free and sequential"
        );
    }
}

#[tokio::test]
async fn tick_mode_records_1000_sequential_ticks_with_price_matching_tick() {
    let dir = tempfile::tempdir().unwrap();

    // Twin playback: increasing price 0.0, +0.0001/tick, 1ms/tick, bounded to N.
    let mut source = PlaybackSource::tick(SyntheticEventStream::ramp("BTC", N));
    let now_ms = source.now_ms_fn(); // recorder stamps with playback's logical time
    let mut sink = ParquetSink::create(dir.path()).unwrap();

    let stats = Recorder::new()
        .with_max_events(N)
        .run(&mut source, &mut sink, now_ms)
        .await
        .unwrap();
    assert_eq!(stats.recorded, N);

    let rows = load_mids(dir.path());
    assert_sequential(&rows, N);

    // For the ramp: price == tick * 0.0001 and ts == tick (1ms/tick from t=0).
    for (seq, ts, mid) in rows {
        assert_eq!(ts, seq as i64, "timestamp must equal the tick index");
        let expected = seq as f64 * 0.0001;
        assert!(
            (mid - expected).abs() < 1e-12,
            "price {mid} != expected {expected} at seq {seq}"
        );
    }
}

#[tokio::test]
async fn realtime_mode_records_1000_ticks_with_timestamp_matching_price() {
    let dir = tempfile::tempdir().unwrap();

    // Time-to-price twin: price encodes the timestamp as ss.mmm. Start at a
    // known base so we can verify exactly. 1ms interval, scaled fast so the test
    // is quick while still exercising realtime pacing.
    let base_ts = 30_000; // 30.000s within the minute
    let stream = TimeRampEventStream::new("BTC", base_ts, 1, Some(N));
    // speed=1000 -> realtime pacing collapses ~1s of logical time to ~1ms waits.
    let mut source = PlaybackSource::realtime(stream, 1000.0);
    let now_ms = source.now_ms_fn();
    let mut sink = ParquetSink::create(dir.path()).unwrap();

    let stats = Recorder::new()
        .with_max_events(N)
        .run(&mut source, &mut sink, now_ms)
        .await
        .unwrap();
    assert_eq!(stats.recorded, N);

    let rows = load_mids(dir.path());
    assert_sequential(&rows, N);

    // Timestamp matches the saved price exactly: price == time_to_price(ts),
    // and ts advances 1ms per tick from the base.
    for (seq, ts, mid) in rows {
        assert_eq!(ts, base_ts + seq as i64, "timestamp must advance 1ms/tick");
        let expected = time_to_price(ts);
        assert!(
            (mid - expected).abs() < 1e-12,
            "saved price {mid} must match time_to_price(ts={ts}) = {expected}"
        );
    }
}
