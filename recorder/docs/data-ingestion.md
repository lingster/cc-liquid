# How the recorder connects to Hyperliquid

This is the engineer's guide to the **ingestion path**: everything between
"start the binary" and "a decoded `RecordedEvent` is handed to the sink". It
explains where the recorder talks to Hyperliquid, what it sends, what it
receives, and how the layers are split so the network code stays thin and
testable.

If you only remember one thing: the recorder uses **two** Hyperliquid surfaces —
a one-shot **HTTP `info`** call at startup to learn the coin universe, and a
long-lived **WebSocket** stream for the actual market data.

---

## The two endpoints

Both are chosen from the target `Network` (`src/config.rs`):

| Network | WebSocket (`ws_endpoint`) | HTTP info (`info_endpoint`) |
|---------|---------------------------|-----------------------------|
| Mainnet | `wss://api.hyperliquid.xyz/ws` | `https://api.hyperliquid.xyz/info` |
| Testnet | `wss://api.hyperliquid-testnet.xyz/ws` | `https://api.hyperliquid-testnet.xyz/info` |

`--network mainnet|testnet` selects the pair. There are no other URLs in the
codebase — everything funnels through `Network::ws_endpoint()` /
`Network::info_endpoint()`.

---

## Step 1 — Learn (and validate) the coin universe over HTTP

Before opening any socket, the recorder fetches the list of tradeable perps so a
bad symbol can't poison the recording.

- **`src/info.rs`** — `fetch_perp_universe()` does a single
  `POST {"type":"meta"}` to the `info` endpoint. It is deliberately defensive:
  bounded timeouts (15s total / 5s connect) so a stalled endpoint can't hang
  startup, and a 16 MiB body cap (`MAX_META_BYTES`) so a hostile/broken endpoint
  can't exhaust memory. It only does I/O — no parsing rules live here.
- **`src/universe.rs`** — the pure half:
  - `parse_perp_universe()` pulls `universe[].name` out of the `meta` body into a
    `HashSet<String>`, erroring on missing/empty/invalid bodies.
  - `validate_coins(requested, available)` splits the requested coins into
    `kept` (order-preserved) and `dropped` (not tradeable on this network).

Coins in `dropped` are logged and skipped; if **nothing** is kept the recorder
exits with an error. This same `meta` fetch also supplies the per-coin tick-size
metadata (`szDecimals` → `px_decimals`) saved into the session manifest.

```text
WARN  ignoring coin(s) not tradeable on mainnet: ["NMR"]
INFO  recording 3 coin(s) on mainnet for 300s ...
```

> The split between `info.rs` (network) and `universe.rs` (pure rules) is the
> recurring pattern in this crate — transport is a thin shell, logic is pure and
> unit-tested.

---

## Step 2 — Build the subscription frames

`src/subscription.rs` turns the kept coin list + a `StreamSelection` into the
ordered list of JSON messages to send. These are pure functions with no
transport, so the wire format is tested independently.

Three streams (`StreamSelection::default()` enables all three):

| Stream | Scope | Frame |
|--------|-------|-------|
| `allMids` | **global** (whole universe, coin-less) | `{"type":"allMids"}` |
| `l2Book` | **per coin** | `{"type":"l2Book","coin":"BTC"}` |
| `trades` | **per coin** | `{"type":"trades","coin":"BTC"}` |

Each is wrapped in the subscribe envelope:
`{"method":"subscribe","subscription":{...}}`. The `--no-mids` / `--no-l2` /
`--no-trades` flags flip the booleans in `StreamSelection`.

So for `BTC,ETH` with all streams you get **5** frames: one `allMids` + two
`l2Book` + two `trades`.

---

## Step 3 — Open the WebSocket and send the frames

`src/client.rs` is the live transport (`WsSource`). It is the only file that
touches the socket and contains no business logic.

- `WsSource::connect(endpoint, subscriptions)` — `tokio-tungstenite`
  `connect_async`, splits the stream into independent read/write halves, then
  sends each subscription frame as a text message. With `RUST_LOG=debug` each
  sent frame is logged.
- It implements the **`EventSource`** trait (`src/source.rs`) — the single
  abstraction the recorder consumes. `next_message()`:
  - returns `Text` frames to the caller,
  - answers server `Ping` with `Pong` to keep the connection alive,
  - treats `Close`/end-of-stream as `None` (the session ends),
  - ignores other frame types.

Because the recorder depends only on `EventSource`, tests drive it with
`ScriptedSource` (in-memory canned frames) — no network needed.

---

## Step 4 — (Optional) Sharded connections for the full universe

A single WebSocket subscribing to hundreds of `l2Book` streams is a bottleneck.
`--shard-size N` splits the coins into groups of `N` and opens **one WebSocket
per group**, read concurrently and merged into one stream.

- `connect_sharded()` in `src/client.rs` builds the shards (`shard_coins`),
  connects a `WsSource` per shard, and hands them to
  `merge_source::MergeSource::spawn()` — one reader task per shard, fanned into a
  single bounded channel (`SHARD_CHANNEL_DEPTH = 4096`, for back-pressure).
- **`allMids` is global**, so it is subscribed on **shard 0 only** (see the
  `all_mids && i == 0` guard) to avoid duplicate universe broadcasts; every shard
  carries its own `l2Book` / `trades` for its coins.

`MergeSource` is itself an `EventSource`, so downstream code can't tell a sharded
source from a single socket. (This is the network-side parallelism; the
storage-side counterpart is `--l2-shards`, which is a separate concern — see the
README's *Scaling* section.)

---

## Step 5 — Survive disconnects

Long recordings outlive individual connections (server resets, network blips).
`src/reconnect.rs` wraps any `EventSource` in `ReconnectSource`, which is also an
`EventSource`.

- It holds a `connect` **factory** (a closure returning a fresh source). When the
  inner source ends or errors, it re-runs the factory — which **re-sends all
  subscriptions** — and keeps yielding messages.
- Backoff is exponential: `base_backoff` 500ms → `max_backoff` 5s, up to
  `max_consecutive_failures` (default 1000, set high so the **recording deadline**,
  not the policy, is the practical stop).
- A reconnect backoff is just a pending future, so the recorder's `select!` over
  the `--duration` deadline can still fire and stop the run cleanly.

---

## Step 6 — Decode, sequence, persist

Once frames arrive, the network is out of the picture. `src/recorder.rs`
(`Recorder::run_until`) drives the loop with `tokio::select!`:

1. Pull a raw frame from the `EventSource` (biased toward draining messages,
   checking the stop/deadline future between them).
2. `parser::parse_message` decodes the JSON into a `MarketEvent`
   (`AllMids` / `L2Book` / `Trade`). Control frames like `pong` parse to "ignore";
   a malformed frame is logged and counted but **never aborts the session**.
3. `sequencer::Sequencer` stamps a monotonic, gap-free `seq` plus timestamps
   (`ts_event_ms` from the exchange, `ts_recv_ms` from the local clock).
4. The wrapped event is written to the `EventSink` (`ParquetSink` in production,
   `MemorySink` in tests).

The sink is **always finalized** — on deadline, on signal, or on error — so a
partial recording stays a valid, readable Parquet.

---

## The whole path at a glance

```
                 ┌─────────────── startup ───────────────┐
  HTTP POST /info {"type":"meta"}  ──►  info.rs  ──►  universe.rs
       (once)                                       (parse + validate coins)
                 └────────────────────┬─────────────────┘
                                      │ kept coins + StreamSelection
                                      ▼
                            subscription.rs  (build subscribe frames)
                                      │
                                      ▼
   WebSocket  wss://…/ws  ──►  client.rs WsSource ─┐
                                                   │ (× shards via MergeSource)
                                                   ▼
                                  reconnect.rs ReconnectSource   (re-subscribe on drop)
                                                   │  EventSource::next_message()
                                                   ▼
   recorder.rs ──► parser.rs ──► sequencer.rs ──► EventSink (ParquetSink)
   (select! loop)   (JSON→event)  (seq+timestamps)   (sessions/…/*.parquet)
```

## Where to look / extend

| Concern | File |
|---------|------|
| Endpoint URLs, network selection, session config | `src/config.rs` |
| HTTP `meta` fetch (universe + tick sizes) | `src/info.rs` |
| Universe parsing + coin validation (pure) | `src/universe.rs` |
| Subscription frame construction (pure) | `src/subscription.rs` |
| Live WebSocket transport + sharded connect | `src/client.rs` |
| `EventSource` trait + test sources | `src/source.rs` |
| Multi-shard fan-in | `src/merge_source.rs` |
| Auto-reconnect wrapper | `src/reconnect.rs` |
| Orchestration loop (source → parse → sink) | `src/recorder.rs` |
| JSON → domain event decoding | `src/parser.rs` |

The live end-to-end path is exercised by the network-gated test in
`tests/integration.rs` (run with `cargo test --test integration -- --ignored live_`).
Everything else runs offline against scripted sources and in-memory sinks.
