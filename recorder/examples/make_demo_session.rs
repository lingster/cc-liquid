//! Generate a small synthetic recorded session for proxy/replay demos.
//!
//! Usage:
//!   cargo run --example make_demo_session -- <out_dir> [ticks]
//!
//! Writes an allMids-only Parquet session (BTC/ETH/SOL, one tick per second,
//! gently ramping prices) plus a manifest — enough to drive
//! `hl-proxy --market-source playback` fully offline.

use hl_recorder::events::{AllMids, MarketEvent, RecordedEvent};
use hl_recorder::manifest::{Counts, Manifest, SCHEMA_VERSION};
use hl_recorder::sink::EventSink;
use hl_recorder::storage::ParquetSink;

const COINS: &[(&str, f64)] = &[("BTC", 95_000.0), ("ETH", 3_200.0), ("SOL", 150.0)];

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .expect("usage: make_demo_session <out_dir> [ticks]");
    let ticks: u64 = args.next().map(|t| t.parse()).transpose()?.unwrap_or(300);

    let start_ms = chrono::Utc::now().timestamp_millis();
    let mut sink = ParquetSink::create(&out)?;
    for i in 0..ticks {
        // Deterministic gentle ramp: +1bp per tick.
        let drift = 1.0 + 0.0001 * i as f64;
        let event = RecordedEvent {
            seq: i,
            ts_event_ms: start_ms + (i as i64) * 1000,
            ts_recv_ms: start_ms + (i as i64) * 1000,
            payload: MarketEvent::AllMids(AllMids {
                mids: COINS
                    .iter()
                    .map(|(c, base)| (c.to_string(), base * drift))
                    .collect(),
            }),
        };
        sink.write(&event)?;
    }
    sink.finalize()?;

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        network: "mainnet".into(),
        endpoint: "synthetic".into(),
        coins: COINS.iter().map(|(c, _)| c.to_string()).collect(),
        streams: vec!["allMids".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        ended_at: chrono::Utc::now().to_rfc3339(),
        duration_secs: ticks,
        counts: Counts {
            recorded: ticks,
            ignored: 0,
            parse_errors: 0,
            all_mids: ticks,
            l2_book: 0,
            trades: 0,
        },
        recorder_version: env!("CARGO_PKG_VERSION").into(),
    };
    std::fs::write(
        std::path::Path::new(&out).join("manifest.json"),
        manifest.to_json()?,
    )?;

    println!("wrote {ticks}-tick demo session to {out}");
    Ok(())
}
