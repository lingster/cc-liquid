# hl-recorder

Tick-by-tick market data **recorder** for the Hyperliquid Digital Twin
(see [`../PRD_HYPERLIQUID_DIGITAL_TWIN.md`](../PRD_HYPERLIQUID_DIGITAL_TWIN.md)).
New to the subsystem? Start with the onboarding guide:
[`../docs/digital-twin.md`](../docs/digital-twin.md).

It connects to the live Hyperliquid WebSocket API, captures the **L2 order book**,
**all-mids**, and **trades** streams, and persists them as an append-only,
replayable **Parquet** session. This is the *record* half of the
record → replay → simulate pipeline; the replay engine and matching engine
consume these sessions.

For a step-by-step walkthrough of how the recorder talks to Hyperliquid (the
HTTP `meta` fetch, WebSocket subscriptions, sharding and auto-reconnect), see
[`docs/data-ingestion.md`](docs/data-ingestion.md).

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
| `--daily` | off | Rotate output files at midnight UTC with `YYYYMMDD_` prefixes; see *Daily rotation* |

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

## Daily rotation (`--daily`)

For multi-day recordings, a single unfinalized Parquet held open for days is a
large blast radius: a hard crash loses everything since the last footer.
`--daily` rotates the output at **midnight UTC** instead, so each completed day
is a fully finalized, immediately readable file set with a `YYYYMMDD_` prefix:

```
sessions/long-run/
├── 20260609_all_mids.parquet     # finalized at 2026-06-10T00:00Z
├── 20260609_l2_book/part-*.parquet
├── 20260609_trades.parquet
├── 20260610_all_mids.parquet     # currently being written
├── 20260610_l2_book/part-*.parquet
├── 20260610_trades.parquet
└── manifest.json                 # daily: true
```

```bash
hl-recorder --cc --daily --l2-shards 4 --out sessions/long-run   # run for weeks
```

**How the boundary is handled (no data loss):** rotation is keyed on each
event's `ts_recv_ms`. The first event of a new UTC day (1) opens the new day's
files *first* — ingestion continues into their in-memory buffers immediately —
then (2) hands the previous day's sink, including any rows still buffered, to a
**background thread** that drains it and writes the footer. The hot recording
path never waits on the midnight close. If opening the new day's files fails
(e.g. disk full), the previous sink stays installed and the error surfaces
instead of events being dropped.

**Parallelism:** three mechanisms compose — (1) the per-day background
finalizer thread at each rotation, (2) the `--l2-shards N` worker threads that
parallelize ZSTD/IO for the heavy L2 stream *within* each day, and (3)
`--shard-size` connection sharding on the network side. A multi-day,
full-universe capture typically runs `--daily --l2-shards 4 --shard-size 20`.

Rotation is forward-only (an event with a clock-skewed earlier timestamp stays
in the currently open day), and background finalize errors are surfaced at the
next flush/finalize rather than silently dropped. The replay loader,
`hl-viewer` and `hl-live --replay` read daily sessions transparently — `seq`
is globally monotonic across days, so the per-day files merge back into one
ordered event stream.

## Output session layout

```
sessions/demo/
├── all_mids.parquet   # one row per (event, coin): seq, ts_event_ms, ts_recv_ms, coin, mid
├── l2_book.parquet    # one row per level: …, side (bid|ask), level_idx, px, sz, n
├── trades.parquet     # one row per trade: …, side, px, sz, trade_time_ms
└── manifest.json      # network, endpoint, coins, streams, window, counts, schema_version, assets
```

Every event carries a monotonic, gap-free `seq` plus the exchange event time
(`ts_event_ms`) and local receive time (`ts_recv_ms`), so a replay engine can
deterministically fold the events back into market state.

**Tick-size metadata (`assets`)** — the manifest records, per recorded coin,
the exchange's price/size grid parameters taken from the same `info`/`meta`
fetch used for coin validation:

```json
"assets": { "BTC": { "sz_decimals": 5, "px_decimals": 1 } }
```

`px_decimals = 6 - szDecimals` is the maximum decimal places a perp price may
carry; combined with Hyperliquid's 5-significant-figure rule, the exact tick at
price `p` is `max(10^-px_decimals, 10^(floor(log10 p) - 4))`. Consumers (e.g.
the `orderbooker` model pipeline) use this instead of inferring tick sizes from
observed quotes. Sessions recorded before this field existed simply omit it.

## Live model harness (`hl-live`)

`hl-live` runs an exported [orderbooker](../orderbooker/) ONNX model against
live (or replayed) L2 data and scores its predictions once the target
snapshots arrive — the Rust port of `orderbooker live`, sharing this crate's
WebSocket client, parser and Parquet writers, with ONNX Runtime (`ort`) for
inference.

```bash
# Export a trained model from Python first
(cd ../orderbooker && uv run orderbooker export models/btc.pt --out models/btc.onnx)

# Live: 5 minutes on mainnet at the model's trained horizon
cargo run --release --bin hl-live -- ../orderbooker/models/btc.onnx --duration 300

# Explicit horizons + custom output
cargo run --release --bin hl-live -- model.onnx --horizons 50,100,200 --out results.parquet

# Deterministic replay of a recorded session (used for Python/Rust parity tests)
cargo run --release --bin hl-live -- model.onnx --replay sessions/demo5min
```

The grid binning, causal normalization and labelling are bit-compatible with
the Python training pipeline (verified: max probability divergence vs PyTorch
< 1e-7 across a full session replay). Results land in a Parquet file with the
same columns as the Python harness. Full guide (flags, output format, parity
procedure): [`../orderbooker/docs/rust-harness.md`](../orderbooker/docs/rust-harness.md).

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
| `proxy` | Digital Twin Proxy (`hl-proxy`): wire-level capture, forward & playback |

Built with red-green TDD: see the `#[cfg(test)]` modules and `tests/integration.rs`.

The full connect → subscribe → ingest path (endpoints, coin validation,
subscription frames, sharding, reconnect) is documented in
[`docs/data-ingestion.md`](docs/data-ingestion.md).

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

## Digital Twin Proxy (`hl-proxy`)

The wire-level half of the twin (PRD **Appendix B**): a standalone loopback
HTTP process speaking Hyperliquid's REST surface (`POST /info`,
`POST /exchange`), so **cc-liquid runs against it with zero code changes** —
only `base_url` is repointed:

```bash
uv run cc-liquid account --set base_url=http://127.0.0.1:8088
```

Three knobs:

1. **Market source** — `/info` market reads (`allMids`, `meta`, `spotMeta`) are
   either **forwarded** to the live exchange (default) or served from a
   **playback** of a recorded session via the replay engine (deterministic,
   fully offline).
2. **Simulator (`--sim`)** — adds the PRD §7 matching engine + virtual account
   on top of playback: account reads (`clearinghouseState`, `userFills`,
   `userFees`, `frontendOpenOrders`) are answered from the virtual account and
   `/exchange` orders are **L2-matched against the replayed market** (partial
   fills, resting Gtc/Alo orders with queue models, stop-loss triggers,
   reduce-only, min-notional). Without `--sim`, account reads and writes
   forward upstream.
3. **Capture logging (always on)** — every round-trip (forwarded, played back,
   or rejected) is appended to `rpc_log.jsonl`, self-classified by Appendix A
   method tag (`all_mids`, `bulk_orders`, …). `--redact-signatures` stores a
   hash placeholder instead of raw signatures for shareable traces.

> ⚠️ **Real writes are testnet-only.** Without `--sim`, `POST /exchange`
> forwards only with `--network testnet --allow-trading`; otherwise the proxy
> answers a live-shaped `{"status":"err",…}` and logs the attempt. With
> `--sim`, orders go to the matching engine and *nothing* can reach a real
> exchange.

```bash
# Capture real testnet traffic (incl. orders) while forwarding live:
hl-proxy --listen 127.0.0.1:8088 --network testnet --allow-trading \
    --out sessions/proxy-demo

# Serve a recorded session as the market feed, log everything:
hl-proxy --listen 127.0.0.1:8088 --market-source playback \
    --session sessions/demo --out sessions/proxy-replay

# Full offline simulator: trade against the recording (PRD §7 + §B.6):
hl-proxy --listen 127.0.0.1:8088 --market-source playback --session sessions/demo \
    --sim --start-balance 10000 --fill-model biased_offset:0.01 \
    --queue conservative --end-of-window hold --out sessions/sim-run
```

Sim options (PRD §7.1.1–§7.1.2, §6 — all deterministic/seedable):

| Flag | Values | Meaning |
|------|--------|---------|
| `--fill-model` | `book` (default), `biased_offset:<frac>`, `fixed_spread:<frac>`, `random_spread:<max_frac>`, `worst_case` | Fill-price overlay over the L2 match |
| `--seed` | u64 | Seed for `random_spread` (same seed ⇒ same fills) |
| `--queue` | `conservative` (default), `optimistic`, `disabled` | Resting-order queue model |
| `--end-of-window` | `stop` (default), `hold`, `loop` | Behaviour past the recorded window |
| `--start-balance` | USD | Virtual account opening balance |

Generate a synthetic session and prove the full loop end-to-end (builds the
proxy, runs the real `cc-liquid account` and `rebalance` through it in both
modes, checks the capture log and fill determinism):

```bash
cargo run --example make_demo_session -- sessions/demo 300
uv run python recorder/scripts/e2e_cc_liquid.py   # from the repo root
```

cc-liquid can also switch by config instead of `--set base_url=...`:

```yaml
provider: twin            # live (default) | twin
twin_proxy:
  url: http://127.0.0.1:8088
```

Module layout mirrors the crate's style — pure, individually-tested pieces:
`proxy::request` (classification), `proxy::log` (`RpcSink`: JSONL/memory),
`proxy::upstream` (`Upstream`: reqwest/scripted), `proxy::market`
(`MarketDataProvider`: playback, end-of-window policy), `proxy::handler`
(routing + write guard + sim), `proxy::server` (minimal loopback HTTP), and
`sim::{order,overlay,matching,account,engine}` (PRD §7 simulation core).

`hl-recorder` saves a verbatim `meta.json` snapshot into every session
(PRD §5.2), so playback serves the exact universe/`szDecimals` seen at record
time; sessions without one get a universe synthesized from the manifest's
coin list.

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
