# Product Requirements Document: Hyperliquid Digital Twin

**Version:** 0.1 (Draft for review)
**Date:** 2026-06-05
**Status:** Draft — pending clarifications
**Owner:** cc-liquid
**Related:** `PRD_TEXTUALIZE_REWRITE.md`, `cc_flow/exchanges/`

---

## 1. Executive Summary

cc-liquid currently talks directly to Hyperliquid through two SDK objects —
`hyperliquid.info.Info` (reads) and `hyperliquid.exchange.Exchange` (writes).
This PRD specifies a **Hyperliquid Digital Twin**: a local, deterministic,
drop-in replacement for those two objects that:

1. **Records** real market data (prices/order book) from the live Hyperliquid
   API on a tick-by-tick basis for a chosen time window and asset set.
2. **Stores** those recordings as an append-only, event-sourced log.
3. **Replays** the recorded time series deterministically, exposing the exact
   same method surface the application already consumes (`all_mids()`,
   `user_state()`, `bulk_orders()`, etc.).
4. **Simulates** order booking/fills against the replayed prices, maintaining a
   virtual account so the rest of cc-liquid (planning, rebalancing, stop-losses,
   PnL display) runs unmodified.

**First milestone:** record a **5-minute** window of tick data for a small set
of assets, then replay it and simulate booking trades within that window.

**Why:** safe, repeatable, offline testing of trading logic; deterministic test
fixtures; demos without spending real funds or hitting rate limits; reproducible
debugging of execution edge cases.

---

## 2. Goals & Non-Goals

### 2.1 Goals
- Provide `TwinInfo` and `TwinExchange` classes that are API-compatible with the
  Hyperliquid SDK methods cc-liquid actually uses (see §4).
- Switch the application between live and twin via configuration only — **no
  changes to `trader.py` business logic**.
- Record live market data tick-by-tick into a durable, replayable event log.
- Replay recordings deterministically with controllable clock/speed.
- Simulate order fills against replayed prices and maintain a virtual account
  (balance, positions, fills, fees).
- Ship the 5-minute record→replay→trade demo end-to-end with tests.

### 2.2 Non-Goals (v1)
- Funding payments, liquidations, ADL, margin-call simulation.
- Multi-user / multi-account simulation (single virtual account).
- Cross-asset margin netting realism beyond what `marginSummary` needs.
- Backtester replacement (`backtester.py` stays separate; see §9).

> **Decision (resolved):** v1 **does** include a full L2 depth matching engine
> (walk-the-book, partial fills) — see §7. This raises fidelity and scope versus
> a mid-price model.

---

## 3. Current Hyperliquid Usage (System-as-is)

All live calls originate from `src/cc_liquid/trader.py` via two SDK objects
constructed in `CCLiquid.__init__`:

```python
self.exchange = Exchange(account, base_url, vault_address=..., account_address=...)
self.info     = Info(base_url, skip_ws=True)
```

### 3.1 `Info` (read) methods used
| Method | Used in | Returns (shape relied upon) |
|---|---|---|
| `user_state(owner)` | account/positions/value | `{marginSummary:{accountValue,totalNtlPos,totalMarginUsed,totalRawUsd}, crossMarginSummary, assetPositions:[{position:{coin,szi,entryPx,liquidationPx,marginUsed}}], withdrawable}` |
| `all_mids()` | pricing for trades, marks, vintages | `{coin: "price_str"}` |
| `meta()` | tradeable universe + `szDecimals` | `{universe:[{name,szDecimals,isDelisted}]}` |
| `frontend_open_orders(owner)` | open orders, TP/SL detection | `[{coin,oid,isTrigger,...}]` |
| `user_fills(owner)` / `user_fills_by_time(owner,start,end)` | PnL aggregation | `[{coin,closedPnl,fee,sz,px,...}]` |
| `user_fees(owner)` | taker rate for fee estimate | `{userCrossRate, ...}` |

### 3.2 `Exchange` (write) methods used
| Method | Used in | Notes |
|---|---|---|
| `bulk_orders([order_request])` | trade execution, stop-losses | order dict: `{coin,is_buy,sz,limit_px,order_type:{limit:{tif}}|{trigger:{...}},reduce_only}`. Response: `{status, response:{data:{statuses:[{filled:{avgPx,fee,...}}|{resting:{oid}}|{error}]}}}` |
| `bulk_cancel([{coin,oid}])` | cancel orders / TP/SL | `{status,...}` |
| `_slippage_price(coin,is_buy,slippage)` | market-order limit px | internal SDK helper cc-liquid calls directly |

> **Implication:** the twin must reproduce these exact return shapes. `_slippage_price`
> being a private SDK method that cc-liquid calls directly is a coupling point the
> twin must also implement.

### 3.3 Existing abstraction (`cc_flow/`)
`cc_flow/exchanges/base.py` already defines `Exchange`/`ExchangeInfo`/`ExchangeTrading`
protocols with a `MockExchange` (`cc_flow/exchanges/mock.py`) and `HyperliquidExchange`.
This is a **second, async, cleaner** interface.

> **Decision (resolved): target BOTH.** The twin's core (ReplayEngine,
> MatchingEngine, VirtualAccount) is interface-agnostic. Two thin adapter layers
> sit on top:
> 1. **`TwinInfo` / `TwinExchange`** — mimic the live `hyperliquid` SDK objects
>    used in `src/cc_liquid/trader.py` (primary, ships first, unblocks the demo).
> 2. **`TwinExchange(cc_flow)`** — implement the async `cc_flow/exchanges/base.py`
>    `ExchangeInfo` / `ExchangeTrading` protocols, sitting alongside `MockExchange`
>    and `HyperliquidExchange`.
> Both adapters wrap the same engine so behavior is identical across interfaces.

---

## 4. Architecture

```
            ┌──────────────────────────────────────────────┐
            │                cc-liquid app                 │
            │            (trader.py unchanged)             │
            └───────────────┬───────────────┬──────────────┘
                            │ Info-like     │ Exchange-like
                            ▼               ▼
                 ┌────────────────────────────────────┐
                 │         Exchange Provider           │
                 │   factory: live | twin (config)     │
                 └───────┬───────────────────┬─────────┘
                         │                   │
                   live  │                   │  twin
                         ▼                   ▼
              ┌──────────────────┐  ┌────────────────────────────┐
              │ hyperliquid SDK  │  │      Digital Twin          │
              │  Info / Exchange │  │  TwinInfo / TwinExchange   │
              └────────┬─────────┘  │  ┌──────────────────────┐  │
                       │            │  │ ReplayEngine (clock) │  │
        ┌──────────────▼─────────┐  │  │ MatchingEngine       │  │
        │     Recorder           │  │  │ VirtualAccount       │  │
        │ (live WS/poll → log)   │  │  └──────────┬───────────┘  │
        └──────────────┬─────────┘  └─────────────┼──────────────┘
                       ▼                          ▼
              ┌───────────────────────────────────────────┐
              │  Event Store (append-only recording log)  │
              │   tick events • session/meta manifest     │
              └───────────────────────────────────────────┘
```

### 4.1 Components
1. **Exchange Provider / Factory** — resolves `Info`-like and `Exchange`-like
   objects from config (`provider: live | twin`). The single switch point.
2. **Recorder** — connects to live Hyperliquid, captures tick data, writes the
   event log. Runs as a CLI command, independent of trading.
3. **Event Store** — append-only persisted log (the source of truth for replay).
4. **ReplayEngine** — reads the event log, owns the simulated clock, advances
   tick-by-tick (event sourcing: state = fold over events), supports
   speed/step/seek.
5. **MatchingEngine** — given the current replayed market state, decides how a
   submitted order fills (price, fee, partial/none).
6. **VirtualAccount** — virtual balance, positions, open orders, fills; produces
   `user_state()` / `user_fills()` / `frontend_open_orders()` outputs.
7. **TwinInfo / TwinExchange** — adapters exposing the §3 method surface, backed
   by ReplayEngine + VirtualAccount.

---

## 5. Recording System

### 5.1 Inputs
- Asset set (e.g. `["BTC","ETH","SOL"]`) or "all from `meta()`".
- Duration / window (start time, end time **or** duration, e.g. 5 minutes).
- Network (mainnet/testnet) and data granularity (see §11 — WS vs poll).
- Output session id / path.

### 5.2 What is recorded (tick events)
**Decision (resolved): record the full L2 book via WebSocket.** Event types:
- `meta_snapshot` — `meta()` universe + `szDecimals` at session start (and on change).
- `l2_book` — full L2 depth per coin per update: `{coin, levels:[bids[], asks[]]}`
  where each level is `{px, sz, n}`. This is the matching substrate.
- `all_mids` — streamed mid prices `{coin: price}` (cheap, used for marks/PnL
  display and as a fallback when a book snapshot is stale).
- `trades` — public trades stream `{coin, px, sz, side, ts}` (used to validate
  the matching model and optionally to advance the book).
- `user_fees_snapshot` — fee schedule at session start.

Each event: `{seq, ts_event_ms, ts_recv_ms, type, coin, payload}` — monotonic
`seq` for deterministic ordering; exchange timestamp + local receive timestamp.

### 5.3 Source of ticks
**Decision (resolved): WebSocket subscriptions** via the SDK `Info(skip_ws=False)`,
subscribing to `l2Book` (per coin), `allMids`, and `trades`. This is true
tick-by-tick and is the only source that supports the L2 matching engine.
The Recorder must handle reconnects, gap detection, and back-pressure, and stamp
every message with a local receive time.

### 5.4 Storage format
**Decision (resolved): Parquet (polars-native).** One session directory:
- `l2_book.parquet`, `all_mids.parquet`, `trades.parquet` — partitioned/sorted by
  `(coin, seq)` for fast seek and columnar scans. (L2 levels stored as nested
  list columns or an exploded `level_idx` schema — to be finalized in design.)
- `manifest.json` — assets, window, network, source, subscriptions, event counts
  per stream, schema version, checksum, SDK version.
- Append strategy: buffer to row-groups and flush periodically so a crash loses
  at most one buffer; finalize/compact on session close.

---

## 6. Replay & Event Sourcing

- **Deterministic fold:** simulated market state at time *t* is the result of
  applying all events with `ts_event_ms <= t` in `seq` order. Same log + same
  order stream ⇒ identical results (required for tests).
- **Clock control:** `as_fast_as_possible` (tests), `realtime`, `Nx` speed, and
  manual `step()` / `seek(t)`.
- **Cursor:** the engine holds the current position in the log; `all_mids()` etc.
  reflect the market state at the cursor.
- **Order/market interleaving:** when the app submits an order, it is timestamped
  at the current cursor and matched against market state at that cursor (and,
  for resting orders, against subsequent ticks).
- **End-of-window behavior (configurable):**
  - `stop` (default) — replay ends at the boundary; orders past the end are
    rejected (honest for a fixed 5-min capture).
  - `hold` — freeze the final book; keep accepting orders against static
    liquidity for post-window experimentation.
  - `loop` — replay the window on repeat for soak/continuous tests (documents the
    price discontinuity at the seam).

---

## 7. Order Simulation (MatchingEngine + VirtualAccount)

### 7.1 v1 fill model — full L2 depth matching (deterministic)
The MatchingEngine matches submitted orders against the **replayed L2 book** at
the cursor (reconstructed by folding `l2_book` events up to `ts`).
- **Market / aggressive limit (IOC-like):** walk the opposing side of the book
  level-by-level, consuming size until the order is filled or liquidity/limit is
  exhausted. Produce a size-weighted `avgPx` across consumed levels; remainder is
  **partially filled** (and cancelled for IOC). Apply taker fee.
- **Resting limit (Gtc/Alo):** post into a virtual book at `limit_px`. Fill when
  later replayed book/trade events cross the level. **Queue position** modeled
  conservatively: a resting order fills only after the recorded size ahead of it
  at that price is consumed (approximated from `l2_book` size deltas and the
  `trades` stream). Apply maker fee on fills; otherwise stays resting (returns
  `oid`).
- **Trigger (stop-loss `sl`):** activate when the replayed mid/last-trade crosses
  `triggerPx`, then execute as market/limit per `isMarket` using the rules above.
- **Self-impact assumption:** the twin treats the recorded book as exogenous
  (the simulated order consumes recorded liquidity but the recorded future book
  is **not** re-derived from our fills). This is a documented limitation of
  replay-based matching.
- **reduce_only / min-notional / szDecimals / price-tick rounding:** honored to
  mirror live validation paths cc-liquid relies on.

### 7.1.1 Configurable market-condition / spread overlay
On top of the L2 matching, a pluggable **fill-price model** lets the operator
simulate market regimes without re-recording. Selected via config; applied to the
effective fill price (and/or resting-fill threshold):
- `book` (default) — pure L2 walk, no overlay; realistic replay of recorded data.
- `biased_offset` — shift the fill price a fixed % for/against the taker to
  emulate **bull/bear** conditions (e.g. "buys fill 1% above ask, sells 1% below
  bid").
- `fixed_spread` — impose a constant spread/slippage around the recorded mid.
- `random_spread` — sample slippage from a configured distribution (seeded for
  determinism) to stress-test under noisy fills.
- `worst_case` — fill at the far edge of consumed liquidity.

Models are composable with the matcher (overlay adjusts price; matcher still
governs available size/partial fills) and **seedable** so runs stay reproducible.
The overlay is the primary lever for "simulate different market conditions."

### 7.1.2 Resting-order queue model (configurable)
- `conservative` (default) — fills only after recorded size-ahead at the level is
  consumed (approx from L2 deltas + `trades`).
- `optimistic` — fills as soon as the replayed price touches the level.
- `disabled` — resting orders rejected (market/aggressive only).

### 7.2 VirtualAccount state
- **Opening state (configurable):** either a fixed starting balance from config
  **or** seeded from a real account's `user_state` snapshot captured at record
  time (stored in the session manifest). Default: fixed balance, fully offline.
- Positions (`coin → szi, entryPx, marginUsed`), realized/unrealized PnL, fees,
  open orders with `oid` allocation.
- Recomputes `marginSummary` (`accountValue`, `totalNtlPos`, `totalMarginUsed`,
  `totalRawUsd`, `withdrawable`) from positions × current marks so
  `get_portfolio_info()` renders correctly.
- Emits fills consumable by `user_fills()` / `aggregate_pnl_by_currency()`.

### 7.3 Outputs match live shapes
`bulk_orders` returns the `statuses:[{filled:{avgPx,fee}}|{resting:{oid}}|{error}]`
structure; `bulk_cancel` returns `{status:"ok"}`; `user_state`, `all_mids`,
`meta`, `frontend_open_orders`, `user_fills`, `user_fees` match §3.1.

---

## 8. Integration / Switch Mechanism

- Add config: `provider: live | twin` (+ twin sub-config: session path, start
  balance, clock mode, speed, fee model).
- Introduce a factory that `CCLiquid.__init__` uses instead of constructing
  `Info`/`Exchange` directly — the **only** change to `trader.py`
  (constructor wiring), preserving all downstream logic. `TwinInfo`/`TwinExchange`
  expose the live SDK surface (§3) including `_slippage_price`.
- **Both interfaces (resolved):** the same engine is also exposed through a
  `cc_flow`-protocol adapter (`ExchangeInfo`/`ExchangeTrading`, async) so the
  newer `cc_flow` stack can run against the twin alongside `MockExchange`.
  The SDK-surface adapter ships first to unblock the demo; the `cc_flow` adapter
  follows, sharing 100% of the engine code.

---

## 9. Relationship to existing backtester
- `backtester.py` is a daily-bar portfolio simulator for strategy research; the
  twin is an **intraday, API-faithful execution simulator** for the live trading
  path. They are complementary, not merged in v1.

---

## 10. First Milestone — 5-Minute Demo (Acceptance)

1. `cc-liquid twin record --assets BTC,ETH,SOL --duration 5m --network mainnet
   --out sessions/demo` captures a 5-minute **L2 + mids + trades** tick log from
   live mainnet Hyperliquid (read-only) into Parquet + manifest.
2. `cc-liquid twin replay sessions/demo` (or `provider=twin`) replays it,
   reconstructing the L2 book by folding events.
3. With `provider=twin`, run an existing flow (e.g. `rebalance` or a scripted
   set of orders) and observe **L2-matched** fills (incl. partial fills), updated
   virtual positions, and PnL — entirely offline.
4. Toggle a market-condition overlay (e.g. `biased_offset` +1%/−1%) and confirm
   fill prices shift accordingly while size/partial-fill logic is unchanged.
5. Determinism test: same session + same orders + same seed ⇒ identical fills/PnL.

**Definition of Done:** demo runs offline; `trader.py` business logic unchanged
(constructor wiring only); `TwinInfo`/`TwinExchange` return live-compatible shapes
(§3); L2 matching + configurable overlay/queue/end-of-window work; deterministic
replay test passes; recorder produces a valid, documented Parquet session artifact;
a `cc_flow`-protocol adapter wraps the same engine.

---

## 11. Decisions (all resolved)

| # | Decision | Choice |
|---|---|---|
| 1 | Fill realism (v1) | **Full L2 depth matching** — walk-the-book, partial fills |
| 2 | Tick source | **WebSocket** `l2Book` + `allMids` + `trades` (true tick) |
| 3 | Storage format | **Parquet** (polars-native), one dir per session + manifest |
| 4 | Integration target | **Both** — live SDK surface first, then `cc_flow` async protocols |
| 5 | Account seeding | **Configurable** — fixed balance (default) or `user_state` snapshot |
| 6 | Network (first demo) | **Mainnet** (read-only recording) |
| 7 | End-of-window | **Configurable** — `stop` (default) / `hold` / `loop` |
| 8 | Fill-price / spread | **Configurable overlay** — `book` / `biased_offset` / `fixed_spread` / `random_spread` / `worst_case`, seedable |
| 9 | Resting queue | **Configurable** — `conservative` (default) / `optimistic` / `disabled` |

All overlay/queue/end-of-window models are config-driven and seeded for
deterministic, reproducible runs (see §7.1.1–7.1.2, §6).
```
