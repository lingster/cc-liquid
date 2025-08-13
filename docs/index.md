---
title: Overview
---

!!! danger "⚠️ PRE-ALPHA SOFTWARE - USE AT YOUR OWN RISK ⚠️"
    - Using this software may result in **COMPLETE LOSS** of funds
    - CrowdCent makes **NO WARRANTIES** and assumes **NO LIABILITY**
    - Users must comply with Hyperliquid terms of service
    - We do **NOT** endorse any strategies using this tool

`cc-liquid` is a reference implementation for simple, automated portfolio rebalancing on Hyperliquid driven by metamodel predictions.

![cc-liquid dashboard](images/dashboard.png)


### What you can do

- Download [CrowdCent](https://crowdcent.com/challenge/hyperliquid-ranking/meta-model/) or [Numerai](https://crypto.numer.ai/meta-model) metamodel predictions
- Inspect account, positions, and exposure
- Rebalance to long/short target sets with equal-weight sizing
- Run continuously on a schedule (autopilot)

### TL;DR

```bash
uv pip install cc-liquid
cc-liquid config     # show current config
cc-liquid account    # view balances and positions
cc-liquid rebalance  # plan and execute trades
cc-liquid run        # run continuously on auto-pilot
```

See [Install & Quick Start](install-quickstart.md) for setup, environment variables, and first run. New users should try testnet first: `--set is_testnet=true`.

!!! warning "Legal Disclaimer"
    THIS SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED. USERS ASSUME ALL RISKS INCLUDING COMPLETE LOSS OF FUNDS, TRADING LOSSES, TECHNICAL FAILURES, AND LIQUIDATION RISKS.