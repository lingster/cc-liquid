# Hyperliquid Digital Twin

This guide documents the **Digital Twin** subsystem: a local, deterministic
stand-in for the Hyperliquid exchange that lets you record real market data,
replay it offline, and run cc-liquid's full trading path — planning, order
placement, fills, PnL — **without touching real funds or the network**. It is
written so a fresh engineer can build, run, and extend the system without any
prior context.

The authoritative requirements document is
[`PRD_HYPERLIQUID_DIGITAL_TWIN.md`](https://github.com/crowdcent/cc-liquid/blob/main/PRD_HYPERLIQUID_DIGITAL_TWIN.md)
in the repository root; section references below (§7, §B.6, …) point into it.

---

## The big picture

cc-liquid talks to Hyperliquid through exactly two REST endpoints (`POST
/info`, `POST /exchange`) plus a WebSocket feed for market data. The twin
exploits this small surface: a Rust process (`hl-proxy`) speaks the same wire
protocol on loopback, so cc-liquid connects to it **with zero code changes** —
only its `base_url` is repointed.

```
                       ┌─────────────────────────────┐
                       │   cc-liquid (unchanged)     │
                       │   hyperliquid Python SDK    │
                       └─────────────┬───────────────┘
                                     │ POST /info, /exchange
                                     ▼
                       ┌─────────────────────────────┐
                       │   hl-proxy (Rust, loopback) │
                       │  ┌───────────────────────┐  │
   rpc_log.jsonl ◄─────┼──┤ capture log (always)  │  │
                       │  └───────────────────────┘  │
                       │   routing per mode:         │
                       │   • forward ──────────────────► real Hyperliquid
                       │   • playback ◄── recorded session (Parquet)
                       │   • --sim ◄── MatchingEngine + VirtualAccount
                       └─────────────────────────────┘
                                     ▲
                       ┌─────────────┴───────────────┐
                       │  hl-recorder (Rust)         │
                       │  live WS → Parquet session  │
                       └─────────────────────────────┘
```

Three binaries live in the `recorder/` Rust crate:

| Binary | Purpose |
|--------|---------|
| `hl-recorder` | Capture live tick data (L2 book, mids, trades) over WebSocket into a replayable Parquet **session** |
| `hl-proxy` | The wire-protocol proxy: forward, playback, and full offline simulation |
| `hl-viewer` | Desktop GUI (egui) for inspecting a recorded session |

---

## Build, test, verify

Everything Rust lives in `recorder/` (plain cargo, no special setup):

```bash
cd recorder
cargo build --release        # builds hl-recorder, hl-proxy, hl-viewer
cargo test                   # ~220 unit + integration tests, no network needed
```

Python side as usual from the repo root:

```bash
uv run pytest                # cc-liquid test suite
```

The single most useful smoke check is the end-to-end harness, which builds the
proxy, generates a synthetic session, and drives the **real cc-liquid CLI**
through it in both proxy modes (including a fill-determinism check):

```bash
uv run python recorder/scripts/e2e_cc_liquid.py
```

If that prints `E2E PASS`, the whole pipeline works on your machine.

---

## 1. Recording a session (`hl-recorder`)

A **session** is a directory of Parquet tables plus metadata — the
append-only event log everything else replays:

```
sessions/demo/
├── all_mids.parquet     # mid prices, one row per (event, coin)
├── l2_book.parquet      # L2 depth, exploded one row per level (or l2_book/part-*.parquet sharded)
├── trades.parquet       # public trade prints
├── manifest.json        # coins, window, network, event counts, schema version
└── meta.json            # verbatim live `meta` response (universe + szDecimals) at record time
```

```bash
# 5 minutes of BTC/ETH/SOL from mainnet (read-only):
hl-recorder --assets BTC,ETH,SOL --duration 300 --network mainnet --out sessions/demo

# Record until Ctrl-C; or source the coin list from the CrowdCent meta model:
hl-recorder --cc --out sessions/crowdcent     # needs CROWDCENT_API_KEY

# No network? Generate a deterministic synthetic session:
cargo run --example make_demo_session -- sessions/demo 300
```

Useful flags: `--shard-size N` (split WS subscriptions across connections for
large universes), `--l2-shards N` (parallel L2 Parquet writers), `--no-l2 /
--no-trades / --no-mids` (stream selection). The recorder survives
disconnects (auto-reconnect with backoff) and finalizes the Parquet footer on
SIGTERM/SIGINT.

`meta.json` matters: playback serves it verbatim, so simulated order rounding
uses the exact `szDecimals` that were live at record time. Sessions without
one (e.g. synthetic) get a universe synthesized from the manifest's coin list.

## 2. Inspecting and replaying a session

```bash
cargo run --bin hl-viewer -- sessions/demo          # GUI: book ladder, prices, navigation
cargo run --example replay_session -- sessions/demo BTC   # CLI summary of a replay
```

Programmatically, the replay engine is the core abstraction
(`recorder/src/replay/`): `load_session(dir)` → ordered events;
`MarketState::apply(event)` folds them; `ReplayEngine` drives tick-mode
(pull) or realtime-mode (wall-clock paced, with Nx speed clocks) playback.

## 3. The proxy (`hl-proxy`)

### Mode A — forward (live capture)

Everything forwards to the real exchange; every round-trip is captured.
This is how you record a *trading* session (the recorder only sees market
data; the proxy sees your orders and their real responses — §B.1):

```bash
hl-proxy --listen 127.0.0.1:8088 --network testnet --allow-trading \
    --out sessions/proxy-demo
```

> ⚠️ **Real order forwarding is testnet-only** and requires the explicit
> `--allow-trading` flag. On mainnet (or without the flag) `/exchange` is
> answered with a live-shaped `{"status":"err", ...}` — which cc-liquid
> handles gracefully — and the attempt is logged. Accidental live trading is
> structurally impossible.

### Mode B — playback (offline market data)

`/info` market reads (`allMids`, `meta`, `spotMeta`) come from a recorded
session, advancing one recorded tick per poll. Account reads and writes still
forward (use Mode C to go fully offline):

```bash
hl-proxy --listen 127.0.0.1:8088 --market-source playback \
    --session sessions/demo --out sessions/proxy-replay
```

### Mode C — playback + `--sim` (full offline simulator)

Adds the PRD §7 **MatchingEngine + VirtualAccount** on top of playback.
Account reads (`clearinghouseState`, `userFills`, `userFees`,
`frontendOpenOrders`) are answered by a virtual account, and `/exchange`
orders are matched against the replayed market. Nothing can reach a real
exchange — there is no upstream dependency at all:

```bash
hl-proxy --listen 127.0.0.1:8088 --market-source playback --session sessions/demo \
    --sim --start-balance 10000 --out sessions/sim-run
```

Simulation behaviour (all deterministic — same session + same orders + same
seed ⇒ identical fills and PnL):

| Flag | Values | Meaning |
|------|--------|---------|
| `--start-balance` | USD (default 10000) | Virtual account opening balance |
| `--fill-model` | `book` (default), `biased_offset:0.01`, `fixed_spread:0.002`, `random_spread:0.005`, `worst_case` | Fill-price overlay (§7.1.1): simulate bull/bear bias, constant or noisy spreads, worst consumed level |
| `--seed` | u64 | Seed for `random_spread` |
| `--queue` | `conservative` (default), `optimistic`, `disabled` | Resting-order queue model (§7.1.2) |
| `--end-of-window` | `stop` (default), `hold`, `loop` | Past the recorded window (§6): reject new orders / freeze final book / replay on repeat |

What the matcher supports: market-style IOC orders (walk the L2 book
level-by-level, partial fills, size-weighted average price; mid-price
fallback with unlimited liquidity for mids-only sessions), resting `Gtc`
orders (fill when later ticks cross the level, queue-aware), post-only `Alo`
(rejected if crossing), stop-loss/take-profit trigger orders (activate on the
mark, then execute), `reduce_only` (rejected if it would increase, clamped to
the position like live), and the $10 min-notional rule.

## 4. Pointing cc-liquid at the proxy

Two equivalent ways — no application code changes either way:

```bash
# One-off override:
uv run cc-liquid account --set base_url=http://127.0.0.1:8088
uv run cc-liquid rebalance --skip-confirm --set base_url=http://127.0.0.1:8088
```

```yaml
# Or persistently in cc-liquid-config.yaml (PRD §8 provider switch):
provider: twin            # live (default) | twin
twin_proxy:
  url: http://127.0.0.1:8088
```

`provider: twin` wins over `is_testnet`; switch back with
`--set provider=live`. A dummy signer works fine against the sim (orders are
not signature-checked there), e.g. `HYPERLIQUID_PRIVATE_KEY=0x111...1` with
any owner address in the profile.

**Worked example — fully offline rebalance:**

```bash
cd recorder && cargo build --bin hl-proxy && cargo run --example make_demo_session -- /tmp/demo 300
./target/debug/hl-proxy --market-source playback --session /tmp/demo --sim --out /tmp/sim-run &
cd .. && uv run cc-liquid rebalance --skip-confirm --set provider=twin
uv run cc-liquid account --set provider=twin    # shows the simulated positions & PnL
```

## 5. The capture log (`rpc_log.jsonl`)

Every request/response crossing the proxy — forwarded, played back, or
rejected — is appended to `<out>/rpc_log.jsonl` (§B.5), one JSON object per
line:

```jsonc
{
  "seq": 12,                      // monotonic, gap-free
  "ts_recv_ms": 1733836800123,    // request received
  "ts_resp_ms": 1733836800187,    // response returned
  "latency_ms": 64,               // upstream round-trip (0 for playback/rejected)
  "transport": "http",
  "endpoint": "/exchange",
  "method_tag": "bulk_orders",    // self-classified Appendix A method
  "request": { ... },             // verbatim body (signatures redactable)
  "response": { ... },            // verbatim body
  "status_code": 200,
  "source": "forward",            // forward | playback | rejected
  "network": "testnet"
}
```

`--redact-signatures` replaces raw signatures with a deterministic hash
placeholder so traces can be shared. The log is gitignored. It is the basis
for debugging ("what did the app actually send and receive?") and for future
log-backed lockstep replay.

---

## Code map (where to extend things)

### Rust — `recorder/src/`

| Module | Responsibility |
|--------|----------------|
| `events`, `parser`, `sequencer`, `wire` | Pure domain model + WS codec |
| `recorder`, `client`, `reconnect`, `source`, `sink`, `storage/` | Live capture pipeline (trait-based: `EventSource` → `EventSink`) |
| `replay/` | `load_session`, `MarketState` (deterministic fold), `ReplayEngine`, clocks, synthetic streams |
| `proxy/request` | Classify `/info`/`/exchange` bodies into Appendix A method tags |
| `proxy/log` | `RpcLogEntry` + `RpcSink` trait (JSONL + in-memory), signature redaction |
| `proxy/upstream` | `Upstream` trait (reqwest HTTPS impl + scripted test double) |
| `proxy/market` | `MarketDataProvider`: playback over a session, end-of-window policy, trade collection for the sim |
| `proxy/handler` | All routing: market-source toggle, testnet write guard, sim dispatch, capture logging |
| `proxy/server` | Minimal hand-rolled loopback HTTP/1.1 (no framework dependency) |
| `sim/order` | Wire-order parsing (asset index → coin via the served universe), `Universe`, status rendering |
| `sim/overlay` | Fill-price models + seeded xorshift RNG |
| `sim/matching` | Book walk, crossing checks, queue models, trigger activation — all pure functions |
| `sim/account` | `VirtualAccount`: positions, realized/unrealized PnL, fees, live-shaped JSON projections |
| `sim/engine` | `SimEngine`: validation, Ioc/Gtc/Alo semantics, trigger lifecycle, cancels, per-tick processing |

Conventions: red/green TDD (every module has a `#[cfg(test)]` suite; socket
-level tests in `recorder/tests/proxy.rs`), pure logic separated from I/O
behind traits, modules kept small. Run `cargo clippy --all-targets` and
`rustfmt` before committing.

Typical extension points:

- **New `/info` endpoint in playback** → add a `MethodTag` in
  `proxy/request.rs`, serve it in `proxy/handler.rs` (`serve_playback` or
  `serve_sim_info`).
- **More realistic fills** → `sim/matching.rs` is pure and exhaustively
  tested; e.g. funding payments or self-impact would slot into
  `SimEngine::on_tick`.
- **New overlay/market regime** → add a `FillOverlay` variant +
  `from_config` arm; it composes automatically with the matcher.

### Python — `src/cc_liquid/`

The only twin-related Python surface is configuration (`config.py`):
`provider: live|twin`, `TwinProxyConfig.url`, and `_set_base_url()` routing.
Business logic in `trader.py` is intentionally untouched — that is the core
design constraint of the whole subsystem (PRD §2.1).

### Tests & harness

| What | Where | Run with |
|------|-------|----------|
| Rust unit tests | `recorder/src/**` (inline) | `cargo test --lib` |
| Proxy over real sockets | `recorder/tests/proxy.rs` | `cargo test --test proxy` |
| Python config switch | `tests/test_config.py` | `uv run pytest` |
| Full-system e2e (real CLI, both modes, determinism) | `recorder/scripts/e2e_cc_liquid.py` | `uv run python recorder/scripts/e2e_cc_liquid.py` |

---

## Design decisions & known limitations

- **Self-impact**: the recorded book is exogenous — simulated fills consume
  recorded liquidity for pricing, but the future stream is not re-derived
  from your fills (§7.1). Fine for execution testing; not a market-impact
  model.
- **Mids-only sessions** degrade to mid-price matching with unlimited
  liquidity (documented in `sim/matching.rs`). Record L2 for depth realism.
- **Conservative queue** approximates queue position as: recorded size at
  your level must be consumed by prints at that price, or price must trade
  strictly through. Resting fills are all-or-nothing (no partial maker fills).
- **Sim ticks on polls**: trigger/resting evaluation happens each time the
  market cursor advances (i.e. per `allMids` poll), not per recorded event
  between polls.
- **No margin realism**: `totalMarginUsed` = notional / maxLeverage;
  `liquidationPx` is `null`; no funding, liquidations, or ADL (§2.2 non-goals).
- **WebSocket playback is not implemented** — cc-liquid constructs
  `Info(skip_ws=True)`, so its entire market-data need is covered by `/info`.

## Not yet implemented (from the PRD)

- In-process Python `TwinInfo`/`TwinExchange` shims and a `cc_flow`-protocol
  adapter (§3.3, §8). The `provider: twin` switch covers the same use case
  over the wire without duplicating the engine in Python; if in-process shims
  are ever needed, wrap a spawned `hl-proxy` rather than reimplementing.
- Log-backed lockstep replay (serve each captured request's exact recorded
  response by `seq`) — the capture log already contains everything needed.
- `/ws` playback (`wire::to_hl_message` exists for frame compatibility).
- `seek(t)` cursor control on the replay engine.
