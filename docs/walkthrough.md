---
title: User Guide
---

This walkthrough explains the core flow and safe first steps.

## 1) Install and configure

Follow [Install & Quick Start](install-quickstart.md) to install and create `.env`. Optionally copy `cc-liquid-config.yaml` and tune values.


!!! tip "Tip: enable tab autocompletion for a smoother CLI experience"

    ```bash
    cc-liquid completion install
    ```

## 2) Choose a data source

Set in `cc-liquid-config.yaml` or via `--set`:

- `crowdcent`: latest consolidated metamodel from CrowdCent
- `numerai`: Numerai Crypto metamodel
- `local`: your own parquet/csv file

Examples:

```bash
cc-liquid download-crowdcent -o predictions.parquet
cc-liquid download-numerai -o predictions.parquet
```

See configuration for column defaults and overrides: [Configuration → Data](configuration.md#data)

## 3) Inspect account and positions

```bash
cc-liquid account
```

## 4) Dry-run a plan (prompted)

```bash
cc-liquid rebalance
```

The CLI will display a plan with target positions and trades; confirm to execute. You can adjust on the fly:

```bash
cc-liquid rebalance --set portfolio.num_long=12 --set portfolio.num_short=8 --set portfolio.target_leverage=2.0
```

### Flatten (close all positions)

```bash
cc-liquid close-all --skip-confirm
```

This plans and executes trades to return to cash. Omit `--skip-confirm` to review first.

## 5) Continuous mode (autopilot)

Schedules execution at `portfolio.rebalancing.at_time` every `every_n_days`.

```bash
cc-liquid run --skip-confirm   # executes automatically on schedule
```

## Safety notes

- Leverage increases liquidation risk; start with 1.0x
- Ensure `execution.min_trade_value` and slippage are appropriate
- Use Hyperliquid testnet first (`--set is_testnet=true`)


