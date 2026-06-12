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

---

## Appendix A — Hyperliquid API Surface Used by cc-liquid

This appendix catalogues every Hyperliquid endpoint and SDK method called by
`src/cc_liquid/trader.py` (`CCLiquid` class). The digital twin's
`TwinInfo` / `TwinExchange` shims must reproduce every response shape listed here
so that downstream business logic in `trader.py` is exercised without change.

### A.1 Base URLs

| Environment | REST / info base URL | WebSocket URL |
|-------------|----------------------|---------------|
| Mainnet (default) | `https://api.hyperliquid.xyz` | `wss://api.hyperliquid.xyz/ws` |
| Testnet | `https://api.hyperliquid-testnet.xyz` | `wss://api.hyperliquid-testnet.xyz/ws` |

The active URL is set by `Config._set_base_url()` and passed to both SDK
objects at construction time. The recorder uses the same WS URLs for its live
capture streams.

### A.2 Read-Only Interface — `hyperliquid.info.Info`

Constructed once in `CCLiquid.__init__` as:

```python
self.info = Info(self.config.base_url, skip_ws=True)
```

| Method | Call site(s) | Purpose | Response shape (relevant fields) |
|--------|-------------|---------|----------------------------------|
| `Info(base_url, skip_ws=True)` | `__init__` | Constructor | — |
| `info.user_state(owner)` | `get_raw_user_state()`, `get_portfolio_info()` | Full account snapshot | `{marginSummary:{accountValue,totalNtlPos,totalMarginUsed,totalRawUsd}, crossMarginSummary:{accountValue,totalNtlPos,totalMarginUsed,totalRawUsd}, assetPositions:[{position:{coin,szi,entryPx,liquidationPx,marginUsed,unrealizedPnl}}], withdrawable}` |
| `info.all_mids()` | `get_portfolio_info()`, `plan_rebalance()`, `execute_plan()`, `aggregate_pnl_by_currency()` | Current mid prices for all perp pairs | `{"BTC": "95000.0", "ETH": "3200.5", …}` — coin → price string |
| `info.meta()` | `_get_sz_decimals()`, `plan_rebalance()` | Tradeable universe and size-decimal rules | `{universe:[{name:str, szDecimals:int, isDelisted:bool}, …]}` |
| `info.frontend_open_orders(owner)` | `get_open_orders()` | Open orders including TP/SL triggers | `[{coin, oid, side, limitPx, sz, isTrigger, triggerPx, tpsl, reduceOnly, orderType, timestamp}, …]` |
| `info.user_fills(owner)` | `get_fills()` | Complete fill history | `[{coin, px, sz, side, time, startPosition, dir, closedPnl, hash, oid, crossed, fee, tid}, …]` |
| `info.user_fills_by_time(owner, start_time_ms, end_time_ms)` | `get_fills()` (when date range given) | Fills in a time window | same shape as `user_fills` |
| `info.user_fees(owner)` | `plan_rebalance()` | Fee tier / taker rate | `{userCrossRate:str, userAddRate:str, feeSchedule:{…}}` |

**Derived usage:** `_get_sz_decimals()` caches `info.meta()["universe"]` as
`{coin: szDecimals}` for order-size rounding; `get_portfolio_info()` computes
`cross_leverage`, `cross_margin_used`, and `cross_maintenance_margin` from the
`crossMarginSummary` fields; `plan_rebalance()` reads `userCrossRate` from fees.

### A.3 Write Interface — `hyperliquid.exchange.Exchange`

Constructed once in `CCLiquid.__init__` as:

```python
self.exchange = Exchange(
    self.account,                              # eth_account.LocalAccount (agent wallet)
    self.config.base_url,
    vault_address=self.config.HYPERLIQUID_VAULT_ADDRESS or None,
    account_address=self.config.HYPERLIQUID_ADDRESS,   # owner / vault address
)
```

| Method | Call site(s) | Purpose | Key arguments | Response shape |
|--------|-------------|---------|---------------|----------------|
| `Exchange(account, base_url, vault_address, account_address)` | `__init__` | Constructor — binds agent key for signing | agent `LocalAccount`, base URL, optional vault, owner address | — |
| `exchange.bulk_orders(orders)` | `execute_plan()`, `_place_resting_orders()` | Submit one or more orders atomically | `orders: [{coin, is_buy, sz, limit_px, order_type, reduce_only}]`; `order_type` is `{"limit":{"tif":"Ioc"}}` for market fills or `{"trigger":{…}}` for TP/SL | `{status:"ok", response:{type:"order", data:{statuses:[{filled:{totalSz,avgPx,fee}}|{resting:{oid}}|{error:str}]}}}` |
| `exchange.bulk_cancel(cancels)` | `cancel_all_orders()`, `_cancel_stale_orders()` | Cancel one or more open orders | `cancels: [{coin, oid}]` | `{status:"ok", response:{type:"cancel", data:{statuses:["success"|{error:str}]}}}` |
| `exchange._slippage_price(coin, is_buy, slippage)` | `execute_plan()` | Compute slippage-adjusted limit price for market-order semantics | `coin: str`, `is_buy: bool`, `slippage: float` (e.g. 0.001) | `float` — rounded limit px |

**Note on `_slippage_price`:** this is a private SDK helper (`_`-prefixed). It
reads the current mid from `info.all_mids()` internally and applies the
configured `execution.slippage_tolerance`. The twin's `TwinExchange` must
replicate this method signature and behaviour (using the virtual mid) so that
`execute_plan()` can call it without modification.

### A.4 WebSocket Subscriptions (recorder)

The recorder (`hl-recorder`) connects to the exchange WS endpoint and subscribes
to the following channels. These are the same data feeds that the digital twin
replays:

| Channel | Subscription payload | One event covers |
|---------|----------------------|-----------------|
| `allMids` | `{"method":"subscribe","subscription":{"type":"allMids"}}` | All current mid prices — one map per push |
| `l2Book` | `{"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC"}}` (one per coin) | Full L2 order book snapshot for one coin |
| `trades` | `{"method":"subscribe","subscription":{"type":"trades","coin":"BTC"}}` (one per coin) | Batch of recent trades for one coin |

`allMids` is global (one subscription regardless of how many coins are recorded).
`l2Book` and `trades` subscriptions are per-coin; with connection sharding
(`--shard-size N`) they are split across multiple WS connections while `allMids`
remains on the first connection only.

### A.5 Twin Compatibility Requirements

For the twin to be a transparent drop-in for `trader.py`:

1. **`TwinInfo`** must implement every method in §A.2, returning identical Python
   types and field names. Response values are derived from the virtual `MarketState`
   (folded from the replay stream) and the `VirtualAccount`.
2. **`TwinExchange`** must implement `bulk_orders`, `bulk_cancel`, and
   `_slippage_price` from §A.3. Orders are routed to the `MatchingEngine` (§7.1);
   fills update `VirtualAccount` (§7.2); returned `statuses` shapes are identical
   to live responses.
3. **Address semantics are preserved:** the owner / vault address is used for
   `Info` queries; the agent wallet is used only for signing. The twin reads both
   from the same `Config` object and applies the same routing.
4. **`skip_ws=True`** on `Info` construction is respected (no live WS needed for
   the read path in the twin).

---

## Appendix B — Digital Twin Proxy (Network-Level Capture & Replay)

### B.1 Motivation

The §8 integration swaps `Info` / `Exchange` for Python shims *inside* the
process. The **Digital Twin Proxy** is a complementary mechanism that operates
one layer lower — at the **network boundary**. It is a standalone process that
speaks the exact Hyperliquid HTTP + WebSocket wire protocol (Appendix A), so
cc-liquid can talk to it **with zero code changes** — only its `base_url` (and WS
URL) are repointed via config.

Two capabilities the in-process shim cannot give us cheaply:

1. **Full request/response capture of the *write* path.** The market-data
   recorder (`hl-recorder`) only captures WS market data. The proxy additionally
   captures real `POST /exchange` order/cancel requests *and their real
   responses* — the actual signed actions cc-liquid emitted and the fills/oids
   Hyperliquid returned. This is a complete, faithful trace of a live trading
   session.
2. **Language- and SDK-agnostic.** Because it intercepts at the wire level, it
   works for the current SDK path, the future `cc_flow` path, or any other client
   — nothing in the app needs to know the twin exists.

### B.2 Modes

The proxy has **two orthogonal knobs**, which keeps v1 simple and matches the
existing split of responsibilities (market data is already recorded by
`hl-recorder` and served by the replay/playback engine):

**1. Market-data source (the toggle).** Where `/info` *market* reads
(`allMids`, `meta`) are answered from:

| `market_source` | Upstream call? | Response source |
|-----------------|----------------|-----------------|
| **`forward`** (default) | **Yes** — to real Hyperliquid | Live mids/meta, returned verbatim |
| **`playback`** | No | A **fixed recorded session** folded by the replay engine (`MarketState`) — fully offline, deterministic |

This is exactly "toggle between a fixed playback or just forward to/from
Hyperliquid directly." Only pure market-data reads are toggleable; account- and
order-specific reads (`clearinghouseState`, `userFills`, `userFees`,
`frontendOpenOrders`) and the write path always forward to the exchange in v1,
because no virtual account/matching engine exists yet (that is the §7 follow-up,
which would later let `playback` serve those too).

**2. Capture logging (always on).** Every request/response crossing — forwarded
*or* served from playback — is appended to the capture log (§B.5). In `forward`
the log holds the real upstream responses (including the write path that
`hl-recorder` cannot see); in `playback` it holds the synthesized market
responses.

> ⚠️ **Write capture is testnet-only in v1.** `POST /exchange` (orders/cancels)
> is forwarded **only** when `network = testnet` *and* `--allow-trading` is set.
> On mainnet — or without `--allow-trading` — the proxy **rejects** `/exchange`
> with an error response and logs the attempt. This makes accidental live order
> placement structurally impossible in v1. Read endpoints (`/info`) may still
> forward to mainnet (read-only) when capturing real market data.

### B.3 Surface to intercept

Hyperliquid's REST surface is small — the Appendix A logical methods map onto
just two POST endpoints plus the WS stream. The proxy mirrors exactly these:

| Wire endpoint | Body discriminator | Appendix A method(s) |
|---------------|--------------------|----------------------|
| `POST /info` | `{"type":"allMids"}` | `info.all_mids()` |
| `POST /info` | `{"type":"clearinghouseState","user":…}` | `info.user_state(owner)` |
| `POST /info` | `{"type":"meta"}` | `info.meta()` |
| `POST /info` | `{"type":"frontendOpenOrders","user":…}` | `info.frontend_open_orders(owner)` |
| `POST /info` | `{"type":"userFills","user":…}` | `info.user_fills(owner)` |
| `POST /info` | `{"type":"userFillsByTime","user":…,"startTime":…,"endTime":…}` | `info.user_fills_by_time(...)` |
| `POST /info` | `{"type":"userFees","user":…}` | `info.user_fees(owner)` |
| `POST /exchange` | `{"action":{"type":"order",…},"nonce","signature","vaultAddress"}` | `exchange.bulk_orders(...)` |
| `POST /exchange` | `{"action":{"type":"cancel",…},…}` | `exchange.bulk_cancel(...)` |
| `GET /ws` (upgrade) | `allMids` / `l2Book` / `trades` subscriptions | recorder market-data feeds |

Because the proxy keys on the `type` discriminator, the request log is
self-classifying: each captured entry is tagged with the logical Appendix A
method it corresponds to.

### B.4 TLS strategy

cc-liquid's `base_url` is configurable, so **no MITM certificate is required**.
The proxy listens on plain HTTP/WS at `http://127.0.0.1:<port>` (loopback);
cc-liquid is configured with `base_url: http://127.0.0.1:<port>`. In `capture`
mode the proxy makes the real **HTTPS** call upstream to `api.hyperliquid.xyz`.
This keeps the client trust chain untouched and avoids cert injection.

### B.5 Capture log format

Each intercepted exchange round-trip is appended to a structured, append-only log
(JSONL recommended for the variable-schema RPC log; market-data WS frames may
additionally feed the existing Parquet session for replay-engine consumption):

```jsonc
{
  "seq": 1024,                       // monotonic, gap-free
  "ts_recv_ms": 1733836800123,       // when proxy received the client request
  "ts_resp_ms": 1733836800187,       // when proxy returned the response
  "latency_ms": 64,                  // upstream round-trip (capture mode)
  "transport": "http",               // http | ws
  "endpoint": "/exchange",           // /info | /exchange | /ws
  "method_tag": "bulk_orders",       // resolved Appendix A method
  "request": { /* verbatim JSON body */ },
  "response": { /* verbatim JSON body */ },
  "status_code": 200,
  "source": "forward",               // forward | playback | rejected
  "network": "testnet"
}
```

- **Correlation:** `seq` plus an optional `nonce` echo lets replay match a
  request to its response.
- **Secret handling:** `/exchange` bodies contain **signatures and nonces** (not
  private keys — those never leave the client). Even so, signatures are sensitive;
  the log lives alongside session data and must be `.gitignore`d. A
  `--redact-signatures` option stores a hash placeholder instead of the raw
  signature for shareable traces.
- **Determinism:** captured timestamps and `oid`s are stored verbatim so `replay`
  reproduces the exact responses the app originally saw.

### B.6 Playback market source (v1) and future replay

**v1 — playback market source.** With `market_source: playback`, the proxy loads
a fixed recorded session and folds it into a `MarketState` via the existing
replay engine. `/info allMids` returns the current mids snapshot; `/info meta`
returns the universe. The playback cursor advances as the app polls (or on a
wall-clock pace), so the served prices evolve exactly as recorded — a
deterministic, offline market feed for cc-liquid with **no application change**
beyond the `base_url` override. Note cc-liquid constructs `Info(skip_ws=True)`,
so it consumes market data over `/info` (not `/ws`); serving `allMids`/`meta`
from playback fully covers its market-data needs.

**Future (§7 follow-up) — engine-backed replay.** Once the `VirtualAccount` and
`MatchingEngine` land, `playback` can additionally answer account reads
(`clearinghouseState`, `userFills`, …) and route `/exchange` writes to the
matcher, folding a recorded/synthetic stream underneath. That turns the proxy
into the full offline simulator over the wire — cc-liquid places *new* orders and
gets L2-matched fills. A separate **log-backed lockstep replay** (serve each
captured request's exact recorded response by `seq`) is also possible later for
regression tests. Neither is in v1 scope.

When/if `/ws` playback is needed, it reuses `wire::to_hl_message` so replayed
frames are byte-compatible with live pushes (Appendix A.4).

### B.7 Configuration

```yaml
twin_proxy:
  listen: 127.0.0.1:8088
  market_source: forward   # forward | playback  (the toggle, §B.2)
  session_dir: sessions/proxy-demo   # capture log out; replay session in (playback)
  network: testnet         # mainnet | testnet
  upstream: https://api.hyperliquid-testnet.xyz   # derived from network if unset
  allow_trading: false     # forward POST /exchange — requires network=testnet (v1)
  redact_signatures: false
```

Equivalent CLI (a new `hl-proxy` binary in the `recorder/` crate):

```bash
# Capture real testnet traffic (incl. orders) while forwarding live:
hl-proxy --listen 127.0.0.1:8088 --network testnet --allow-trading \
    --out sessions/proxy-demo

# Serve a fixed recorded session as the market feed, log everything:
hl-proxy --listen 127.0.0.1:8088 --market-source playback \
    --session sessions/demo --out sessions/proxy-replay
```

cc-liquid then points at the proxy with a one-line override, e.g.
`--set base_url=http://127.0.0.1:8088`. No other application change is required.

### B.8 Implementation notes (decisions resolved)

- **Language & placement (resolved): Rust, in the existing `recorder/` crate.**
  A new `hl-proxy` binary reuses the framework already built — `config::Network`
  (endpoints), `parser` / `wire` (codec), `events`, the `replay` engine
  (`load_session` → `MarketState`) for the playback market source, and the
  storage/session conventions. No Python proxy.
- **Suggested module layout** (all pure/testable, mirroring the crate's
  red-green TDD style): `proxy::request` (classify `/info` + `/exchange` bodies
  into the Appendix A/B.3 tags), `proxy::log` (`RpcLogEntry` + `RpcSink` trait
  with JSONL + in-memory impls, signature redaction), `proxy::upstream`
  (`Upstream` trait → reqwest impl + scripted test impl), `proxy::market`
  (`MarketDataProvider` → playback over the replay engine), `proxy::handler`
  (routing: toggle, testnet write guard, logging), and a thin `proxy::server`
  (loopback HTTP) wired by the `hl-proxy` binary.
- **Write capture (resolved): testnet-only in v1.** `/exchange` forwards only
  when `network = testnet` and `--allow-trading`; otherwise rejected + logged
  (§B.2). Mainnet writes are out of scope until the simulator matures.
- **Market source (resolved): playback ↔ forward toggle.** Market data is already
  recorded by `hl-recorder` and served by the replay engine; the proxy simply
  toggles `/info` market reads between a fixed playback session and live forward
  (§B.2, §B.6). The full virtual-account/matching path is the §7 follow-up.
- **Relationship to §8:** the proxy *complements*, not replaces, the in-process
  `TwinInfo`/`TwinExchange` shim. The shim is the fastest path for unit-style,
  fully-in-process simulation; the proxy is the highest-fidelity path for
  end-to-end capture of real sessions and SDK-agnostic playback.

### B.9 Decisions (Appendix B)

| # | Decision | Choice |
|---|---|---|
| B1 | Implementation language / placement | **Rust**, new `hl-proxy` binary in the `recorder/` crate, reusing existing codec + replay framework |
| B2 | Write (`/exchange`) capture in v1 | **Testnet-only**, gated behind `--allow-trading`; rejected + logged otherwise |
| B3 | Market-data behaviour | **Toggle** `market_source: forward | playback` — forward to/from live, or serve a fixed recorded session via the replay engine |
| B4 | Capture logging | **Always on** — JSONL `rpc_log.jsonl`, self-classifying by Appendix A method tag, optional signature redaction |
