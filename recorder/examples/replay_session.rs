//! Replay a recorded session in tick mode and print a summary.
//!
//! Usage:
//!   cargo run --example replay_session -- <session_dir> <coin>
//!
//! Demonstrates loading a Parquet session and folding it into market state with
//! the tick-mode engine.

use hl_recorder::replay::{load_session_stream, ReplayEngine};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: replay_session <session_dir> <coin>");
    let coin = args.next().unwrap_or_else(|| "BTC".to_string());

    let stream = load_session_stream(&dir)?;
    let mut engine = ReplayEngine::new(stream);

    let mut ticks = 0u64;
    let mut first = None;
    let mut last = None;
    let mut first_ts = None;
    let mut last_ts = None;

    while engine.step() {
        ticks += 1;
        if let Some(px) = engine.price(&coin) {
            if first.is_none() {
                first = Some(px);
                first_ts = engine.cursor_ts_ms();
            }
            last = Some(px);
            last_ts = engine.cursor_ts_ms();
        }
    }

    println!("session : {dir}");
    println!("coin    : {coin}");
    println!("ticks   : {ticks}");
    match (first, last) {
        (Some(f), Some(l)) => {
            println!("first {coin} price: {f}  (ts={:?})", first_ts);
            println!("last  {coin} price: {l}  (ts={:?})", last_ts);
            if let Some(book) = engine.book(&coin) {
                println!(
                    "final book top: bid={:?} ask={:?}",
                    book.best_bid(),
                    book.best_ask()
                );
            }
        }
        _ => println!("no prices for {coin} in this session"),
    }
    Ok(())
}
