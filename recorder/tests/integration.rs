//! End-to-end pipeline tests.
//!
//! These wire the real components together (source -> recorder -> Parquet sink)
//! without any network, then read the Parquet back to prove the recording is
//! valid and replayable.

use std::fs::File;
use std::path::Path;

use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use hl_recorder::recorder::Recorder;
use hl_recorder::source::ScriptedSource;
use hl_recorder::storage::parquet_sink::{ALL_MIDS_FILE, L2_BOOK_FILE, TRADES_FILE};
use hl_recorder::storage::ParquetSink;

fn row_count(path: &Path) -> usize {
    let file = File::open(path).unwrap();
    ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap()
        .map(|b| b.unwrap().num_rows())
        .sum()
}

#[tokio::test]
async fn full_pipeline_records_session_to_parquet() {
    let dir = tempfile::tempdir().unwrap();

    let mut source = ScriptedSource::new(vec![
        r#"{"channel":"allMids","data":{"mids":{"BTC":"95000","ETH":"3200"}}}"#.into(),
        r#"{"channel":"subscriptionResponse","data":{}}"#.into(),
        r#"{"channel":"l2Book","data":{"coin":"BTC","time":1700000000000,
            "levels":[[{"px":"94999","sz":"1.5","n":3},{"px":"94998","sz":"2.0","n":4}],
                      [{"px":"95001","sz":"1.0","n":2}]]}}"#
            .into(),
        r#"{"channel":"trades","data":[
            {"coin":"BTC","side":"B","px":"95000","sz":"0.1","time":1700000000010},
            {"coin":"ETH","side":"A","px":"3200","sz":"1.0","time":1700000000020}]}"#
            .into(),
    ]);
    let mut sink = ParquetSink::create(dir.path()).unwrap();

    let mut clock = 1_000i64;
    let stats = Recorder::new()
        .run(&mut source, &mut sink, || {
            clock += 1;
            clock
        })
        .await
        .unwrap();

    assert_eq!(stats.recorded, 3); // allMids + l2Book + trades
    assert_eq!(stats.ignored, 1); // subscriptionResponse
    assert_eq!(stats.parse_errors, 0);

    // Exploded row counts: 2 mids, 3 book levels (2 bids + 1 ask), 2 trades.
    assert_eq!(row_count(&dir.path().join(ALL_MIDS_FILE)), 2);
    assert_eq!(row_count(&dir.path().join(L2_BOOK_FILE)), 3);
    assert_eq!(row_count(&dir.path().join(TRADES_FILE)), 2);
}

/// Live smoke test against mainnet. Network-gated: run explicitly with
/// `cargo test --test integration -- --ignored live_`.
#[tokio::test]
#[ignore = "requires network access to Hyperliquid mainnet"]
async fn live_records_btc_for_a_few_seconds() {
    use hl_recorder::client::WsSource;
    use hl_recorder::config::Network;
    use hl_recorder::subscription::{build_subscriptions, StreamSelection};

    let dir = tempfile::tempdir().unwrap();
    let coins = vec!["BTC".to_string()];
    let subs = build_subscriptions(&coins, &StreamSelection::default());

    let mut source = WsSource::connect(Network::Mainnet.ws_endpoint(), &subs)
        .await
        .expect("connect");
    let mut sink = ParquetSink::create(dir.path()).unwrap();

    let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
    let stats = Recorder::new()
        .run_until(
            &mut source,
            &mut sink,
            || chrono::Utc::now().timestamp_millis(),
            deadline,
        )
        .await
        .unwrap();

    assert!(stats.recorded > 0, "expected some live ticks in 5s");
}
