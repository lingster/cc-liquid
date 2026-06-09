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
