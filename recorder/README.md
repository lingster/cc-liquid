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

# Record until you stop it with Ctrl-C (no --duration); finalizes on exit
hl-recorder --assets BTC,ETH,SOL --out sessions/demo

# Or record the CrowdCent meta-model universe (coins of its latest release)
hl-recorder --cc --duration 300 --out sessions/crowdcent
```

| Flag | Default | Description |
|------|---------|-------------|
| `--assets` | — | Comma-separated coins, e.g. `BTC,ETH,SOL`. Optional when `--cc` is used |
| `--cc` | off | Source the coin list from the CrowdCent meta model instead of `--assets` |
| `--cc-challenge` | `hyperliquid-ranking` | CrowdCent challenge slug to pull the universe from |
| `--cc-url` | `https://crowdcent.com/api` | CrowdCent API base URL |
| `--duration` | `0` | Recording length in seconds. `0` = record until stopped by a signal (Ctrl-C / SIGTERM) |
| `--network` | `mainnet` | `mainnet` or `testnet` |
| `--out` | (required) | Output session directory |
| `--no-l2` / `--no-trades` / `--no-mids` | off | Drop a stream from the recording |
| `--shard-size` | `0` | Coins per WebSocket connection (`0` = single); see *Scaling* |
| `--l2-shards` | `1` | Parallel L2 Parquet part-files; see *Scaling* |
| `--flush-interval` | `300` | Flush buffered rows to disk at least every N seconds (`0` disables); see *Durability* |

Set `RUST_LOG=debug` for verbose subscription/transport logging.

**CrowdCent coin universe (`--cc`)** — downloads the challenge's consolidated
meta-model parquet, takes the unique asset ids of its most recent `release_date`,
and uses them as the recording coin list (then validated against the live perp
universe like any `--assets` list). Requires a `CROWDCENT_API_KEY`, read from the
environment or, as a fallback, a nearby `.env` file (searched from the working
directory upward). The same `.env` fallback applies to every env var, so e.g.
`RUST_LOG` can live there too.

```text
INFO  fetching coin universe from CrowdCent challenge `hyperliquid-ranking`
INFO  CrowdCent meta model yielded 172 coin(s)
INFO  recording 172 coin(s) on mainnet for 300s ...
```

**Coin validation** — requested `--assets` are checked against the live perp
universe (fetched from the `info` endpoint) before subscribing. Coins that are
not tradeable on the target network are dropped with a warning rather than
subscribed; a single bad symbol can no longer reset the shared connection. If
none of the requested coins are valid, the recorder exits with an error.

```text
WARN  ignoring coin(s) not tradeable on mainnet: ["NMR"]
INFO  recording 3 coin(s) on mainnet for 300s ...
```

**Self-healing transport** — the live source is wrapped in a reconnecting
`EventSource` (`reconnect::ReconnectSource`). On a dropped/reset connection it
transparently reconnects with exponential backoff and re-sends all
subscriptions, so a transient disconnect no longer ends a long recording — the
`--duration` deadline remains the authoritative stop.

## Durability

Rows are buffered in memory and written to Parquet on **three** triggers:

1. **Row-count threshold** — a per-table buffer reaching ~10k rows is encoded as
   a row group (bounds memory under bursty load).
2. **Time-based flush** (`--flush-interval`, default **300s**) — every N seconds
   all buffered rows are drained and the current row group is pushed to disk, so
   data lands incrementally even when no buffer hits the row-count threshold.
   `--flush-interval 0` disables it.
3. **Finalize on stop** — when the `--duration` deadline fires *or* a graceful
   shutdown signal arrives, buffers are drained and each file's **footer** is
   written, producing a valid, readable Parquet.

With `--duration 0` (the default) there is no deadline: the recorder runs
indefinitely and stops only on a shutdown signal, finalizing at that point.

**Graceful shutdown** — `SIGTERM`, `SIGINT` (Ctrl-C), `SIGHUP` and `SIGQUIT` are
caught and trigger the same finalize path as the deadline, so stopping a
recording (systemd stop, Ctrl-C, container shutdown) — including an unlimited
run — still writes the footer instead of leaving a truncated file.

```text
WARN  received SIGTERM; finalizing session early (writing Parquet footer)
INFO  done: recorded=23 (mids=7, l2=0, trades=16), ignored=3, errors=0
```

> A Parquet file is only valid once its footer is written, which happens at
> finalize. The periodic flush bounds memory and persists row groups, but a file
> is guaranteed readable only after a finalize. `SIGKILL` (`kill -9`) **cannot**
> be intercepted by any process, so a hard-kill mid-recording can still leave an
> unfinalized file — use a graceful signal to stop a session.

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

## Inspecting a session (`hl-viewer`)

`hl-viewer` is a desktop GUI ([egui](https://github.com/emilk/egui)) for
examining a recorded session tick-by-tick.

```bash
cargo run --bin hl-viewer -- sessions/demo
```

- **Coin picker** — choose any coin present in the session.
- **Order book** — the L2 book at the current tick: bids (descending) and asks
  (ascending) with `px` / `sz` / `n`, plus best bid, best ask and mid.
- **Price ladder** — every distinct price the coin trades at across the file.
- **Tick navigation** — *Prev* / *Next* step one L2 snapshot at a time (clamped
  at both ends).
- **Timeline** — drag the slider to scrub the current tick to any point in time
  (jumps to the nearest snapshot).
- **Playback** — *Play* / *Pause* replays the book in real time with a speed
  multiplier, animating the order-book movements.

All navigation, indexing and playback-pacing logic lives in the pure,
unit-tested `viewer` module (`session_data`, `navigator`, `playback`); the egui
binary is a thin rendering shell over it (Dependency Inversion again).

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
| `reconnect` | Self-healing `EventSource` wrapper (backoff reconnect) |
| `universe` / `info` | Fetch + validate the tradeable coin universe |
| `viewer` | Pure model for the `hl-viewer` GUI (session, navigation, playback) |
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

## Scaling to many currencies

The whole pipeline is coin-keyed, and two features let it scale to the full
Hyperliquid universe:

**Connection sharding** — `--shard-size N` splits the coins across multiple
WebSocket connections (one per group), merged concurrently via
`merge_source::MergeSource` (one reader task per shard). `allMids` is global, so
it is subscribed only on the first shard. This parallelizes network I/O for
full-universe L2 capture.

**Parallel partitioned L2 storage** — `--l2-shards N` writes the heavy L2 stream
to **N Parquet part-files** under `l2_book/`, each written by its own background
worker thread (`storage::ShardedParquetSink`). The recording thread only buffers
rows and ships `RecordBatch`es to workers over bounded channels, so ZSTD
compression and disk I/O run in parallel across cores. A coin always hashes to the
same part, so ordering per coin is preserved and each coin lives in exactly one
file. Mids/trades stay single-file (they are light). The replay loader reads both
the single-file and partitioned layouts transparently.

```bash
# Full-universe-style capture: many connections + parallel L2 writers
hl-recorder --assets BTC,ETH,SOL,DOGE --duration 300 \
    --shard-size 2 --l2-shards 4 --out sessions/universe
# -> sessions/universe/l2_book/part-0000.parquet .. part-0003.parquet
```

**Multi-coin synthetic FX** — `replay::MultiCoinSyntheticStream` broadcasts mids
for an arbitrary set of pairs in one `allMids` event per tick (each coin on its
own ramp), so the twin can synthesize FX for many pairs without a recording.

What scales now: mids/FX for the entire universe (one global subscription),
multi-coin record/replay, and parallel L2 writes. The remaining bottleneck for
the *full* universe with L2 is central JSON parsing (single consumer); parsing in
the shard tasks would be the next step.
