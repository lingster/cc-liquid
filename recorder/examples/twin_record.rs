//! Digital-twin demo: drive the recorder from synthetic playback (no network).
//!
//! Usage:
//!   cargo run --example twin_record -- <out_dir> <tick|realtime> [n]
//!
//! - tick:     increasing price 0.0 +0.0001/tick (pairs with tick mode)
//! - realtime: price encodes time as ss.mmm (pairs with realtime mode)

use hl_recorder::recorder::Recorder;
use hl_recorder::replay::synthetic::{SyntheticEventStream, TimeRampEventStream};
use hl_recorder::storage::ParquetSink;
use hl_recorder::twin::PlaybackSource;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .expect("usage: twin_record <out_dir> <tick|realtime> [n]");
    let mode = args.next().unwrap_or_else(|| "tick".to_string());
    let n: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);

    let mut sink = ParquetSink::create(&out)?;
    let mut recorder = Recorder::new().with_max_events(n);

    let stats = match mode.as_str() {
        "tick" => {
            let mut source = PlaybackSource::tick(SyntheticEventStream::ramp("BTC", n));
            let now_ms = source.now_ms_fn();
            recorder.run(&mut source, &mut sink, now_ms).await?
        }
        "realtime" => {
            // 1ms/tick from t=30_000ms, paced fast so the demo finishes quickly.
            let stream = TimeRampEventStream::new("BTC", 30_000, 1, Some(n));
            let mut source = PlaybackSource::realtime(stream, 1000.0);
            let now_ms = source.now_ms_fn();
            recorder.run(&mut source, &mut sink, now_ms).await?
        }
        other => anyhow::bail!("unknown mode `{other}` (use tick|realtime)"),
    };

    println!("mode={mode} recorded={} -> {out}", stats.recorded);
    Ok(())
}
