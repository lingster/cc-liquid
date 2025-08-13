---
title: How it works
---

High-level flow of a rebalance:

1) Load predictions

   - From [CrowdCent](https://crowdcent.com/challenge/hyperliquid-ranking/meta-model/), [Numerai](https://crypto.numer.ai/meta-model), or a local file into a Polars DataFrame
   - Keep latest row per asset based on `date_column`

2) Select assets

   - Top `num_long` as longs and bottom `num_short` as shorts by `prediction_column`
   - Filter to perpetuals currently listed on Hyperliquid

3) Size positions

   - Equal-weight notional per position: `(account_value * target_leverage) / total_positions`
    - Warn if below `execution.min_trade_value`

4) Generate trades

   - Compare current value vs target value per asset to compute deltas
    - Round size to `sz_decimals` per asset (from exchange metadata)
    - Trades with absolute delta below `execution.min_trade_value` are marked as skipped (reported separately)

5) Execute

   - Market open via Hyperliquid SDK with `execution.slippage_tolerance`
    - Report fills, failures, and slippage; summarize execution
    - Unfilled or error statuses are surfaced with reasons; successful fills include `totalSz` and `avgPx`

Scheduling

- The autopilot loop computes the next rebalance time in UTC from [`portfolio.rebalancing`](configuration.md#portfolio)
- When due, it generates a plan, asks for confirmation (or uses `--skip-confirm`), executes, and saves the timestamp
- Last successful timestamp is stored locally in `.cc_liquid_state.json`