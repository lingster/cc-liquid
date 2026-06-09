# hl-recorder

Tick-by-tick market data **recorder** for the Hyperliquid Digital Twin
(see [`../PRD_HYPERLIQUID_DIGITAL_TWIN.md`](../PRD_HYPERLIQUID_DIGITAL_TWIN.md)).

It connects to the live Hyperliquid WebSocket API, captures the **L2 order book**,
**all-mids**, and **trades** streams, and persists them as an append-only,
replayable **Parquet** session. This is the *record* half of the
record → replay → simulate pipeline; the replay engine and matching engine
consume these sessions.

## Build & test

```bash
cargo build --release
cargo test                 # unit + offline pipeline tests (no network)
cargo test --test integration -- --ignored live_   # live mainnet smoke test
```

## Usage

```bash
# Record a 5-minute mainnet session for three assets
hl-recorder --assets BTC,ETH,SOL --duration 300 --network mainnet --out sessions/demo
```

| Flag | Default | Description |
|------|---------|-------------|
| `--assets` | (required) | Comma-separated coins, e.g. `BTC,ETH,SOL` |
| `--duration` | `300` | Recording length in seconds |
| `--network` | `mainnet` | `mainnet` or `testnet` |
| `--out` | (required) | Output session directory |
| `--no-l2` / `--no-trades` / `--no-mids` | off | Drop a stream from the recording |

Set `RUST_LOG=debug` for verbose subscription/transport logging.

## Output session layout

```
sessions/demo/
├── all_mids.parquet   # one row per (event, coin): seq, ts_event_ms, ts_recv_ms, coin, mid
├── l2_book.parquet    # one row per level: …, side (bid|ask), level_idx, px, sz, n
├── trades.parquet     # one row per trade: …, side, px, sz, trade_time_ms
└── manifest.json      # network, endpoint, coins, streams, window, counts, schema_version
```

Every event carries a monotonic, gap-free `seq` plus the exchange event time
(`ts_event_ms`) and local receive time (`ts_recv_ms`), so a replay engine can
deterministically fold the events back into market state.

## Architecture

Designed around two trait abstractions so the orchestration is fully testable
without network or disk (Dependency Inversion):

```
EventSource (trait)            EventSink (trait)
  ├─ WsSource  (live WS)         ├─ ParquetSink  (sessions on disk)
  └─ ScriptedSource (tests)      └─ MemorySink   (tests)
                 \             /
                  Recorder  →  parse_message → Sequencer → sink
```

| Module | Responsibility |
|--------|----------------|
| `events` | Pure domain model (`L2Book`, `Trade`, `AllMids`, `RecordedEvent`) |
| `subscription` | Build WS subscription messages |
| `parser` | Decode raw Hyperliquid JSON → domain events |
| `sequencer` | Assign monotonic `seq` + timestamps |
| `recorder` | Orchestrate source → parse → sink, with deadline support |
| `sink` / `storage::parquet_sink` | `EventSink` trait + Parquet writer |
| `source` / `client` | `EventSource` trait + live WebSocket client |
| `manifest` | Self-describing session metadata |
| `config` | Network endpoints + session config |

Built with red-green TDD: see the `#[cfg(test)]` modules and `tests/integration.rs`.

## Replay engine

The `replay` module is the *replay* half of the pipeline: it folds a recorded
(or synthetic) event stream into market state and plays it back. Hyperliquid's
smallest time unit is **1 ms** (every `time` field is a ms epoch), so that is the
tick resolution.

Two orthogonal abstractions compose into the playback modes (SOLID):

| Abstraction | Implementations |
|-------------|-----------------|
| `EventStream` (source) | `ParquetEventStream` (recorded session) · `SyntheticEventStream` (ramp) · `TimeRampEventStream` (time-encoded) |
| `Clock` (pacing) | `RealtimeClock` (wall-clock, speed-scaled) · `ManualClock` (deterministic tests) |

### Playback modes

1. **Tick mode** (`engine.step()` / `engine.next_price(coin)`) — pull-based; each
   call applies the next event and advances the cursor by one tick. `next_price`
   returns the *current* price then increments. Pairs naturally with the
   arithmetic ramp.
2. **Realtime mode** (`engine.run_realtime(&clock, on_tick)`) — push-based; paces
   the gaps between events via the clock so ticks elapse in real (or scaled)
   time. Pairs naturally with the time-encoded stream.

### Synthetic streams (no Parquet needed)

- **Ramp / test mode** — `SyntheticEventStream::ramp(coin, n)`: price starts at
  `0.0` and rises by `0.0001` each tick, `+1 ms` per tick. Best in tick mode.
- **Time-to-price mode** — `TimeRampEventStream`: price encodes the event's
  timestamp as `seconds.milliseconds` (e.g. `12_345 ms -> 12.345`, wrapping each
  minute). Best in realtime mode, where the reported price tracks the clock.

```rust
use hl_recorder::replay::{load_session_stream, ReplayEngine, RealtimeClock};

// Recorded session, tick mode:
let mut engine = ReplayEngine::new(load_session_stream("sessions/demo")?);
while engine.step() {
    let _btc = engine.price("BTC");
}

// Recorded session, realtime mode at 2x speed:
let clock = RealtimeClock::new(2.0);
engine.run_realtime(&clock, |state| {
    let _ = state.price("BTC");
}).await;
```

Replay a recorded session from the CLI:

```bash
cargo run --example replay_session -- sessions/demo BTC
```

## Digital-twin loop (playback → recorder)

`twin::PlaybackSource` makes a playback stream *look like the live exchange*: it
serializes events to Hyperliquid JSON (via `wire`) and feeds them to the recorder
through the same `EventSource` trait the live WebSocket client implements — so the
recorder cannot tell replay from live. It also publishes its logical clock, which
the recorder uses for `ts_recv_ms`, keeping replay deterministic and making
time-encoded prices line up exactly with recorded timestamps.

```bash
# Tick mode: increasing price 0.0 +0.0001/tick -> 1000 sequential ticks
cargo run --example twin_record -- sessions/twin_tick tick 1000

# Realtime mode: price encodes time as ss.mmm, paced by the clock
cargo run --example twin_record -- sessions/twin_realtime realtime 1000
```

The recorder can stop on a count (`Recorder::with_max_events(n)`) as well as a
duration. The `tests/twin.rs` integration tests assert the output Parquet holds
exactly 1000 gap-free, sequentially-numbered ticks whose timestamps match the
saved prices, in both tick and realtime modes.
