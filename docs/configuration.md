---
title: Configuration
---

cc-liquid loads configuration from `cc-liquid-config.yaml` and environment variables. Addresses, profile selection, and portfolio parameters live in the config YAML; secrets (private keys, API keys) should live in `.env`. You can override any setting via `--set` when running the CLI commands.

## Environment variables (.env)

Your env file, which should only store secrets/keys should look like this:

```env
CROWDCENT_API_KEY=zzxFake.CrowdCentKey1234567890   # (needed for CrowdCent metamodel source)
HYPERLIQUID_PRIVATE_KEY=0xdeadbeefdeadbeefdeadbeefdeadbeefdead  # (default signer key variable name)
```

!!! note 
    - You can change the default signer key variable name and provide additional, profile-specific signer keys you can reference via `signer_env` in YAML, (e.g.:`HYPER_AGENT_KEY_PERSONAL`, `HYPER_AGENT_KEY_VAULT`)

    - Do not put addresses in `.env`; keep owner/vault addresses in the configuration YAML file.

!!! warning "Loading .env variables"
    When `cc-liquid` is installed as a CLI tool (e.g., via `uv tool install`), it may not automatically load variables from `.env`. If you encounter errors about missing keys, you must load them manually in your shell session:
    ```bash
    export $(grep -v '^#' .env | xargs)
    ```

## YAML (`cc-liquid-config.yaml`)
Your yaml file, in the root of where you call `cc-liquid` should look like this:
```yaml
is_testnet: false

active_profile: default

profiles:
  default:
    owner: 0xYourMain
    vault: null                 # omit or null for personal portfolio
    signer_env: HYPERLIQUID_PRIVATE_KEY

  my-vault:
    owner: 0xYourMain           # optional, informational
    vault: 0xVaultAddress
    signer_env: HYPERLIQUID_AGENT_KEY_VAULT


data:
  source: crowdcent | numerai | local
  path: predictions.parquet
  date_column: release_date | date
  asset_id_column: id | symbol
  prediction_column: pred_10d | meta_model

portfolio:
  num_long: 10
  num_short: 10
  target_leverage: 1.0
  rebalancing:
    every_n_days: 10
    at_time: "18:15"   # HH:MM (UTC)

execution:
  slippage_tolerance: 0.005
  min_trade_value: 10.0
```


## Profiles, network & credentials

- `profiles` define who you trade for and which key signs.
  - `owner`: portfolio owner (used for Info queries when `vault` not set)
  - `vault`: optional; when set, becomes the portfolio owner for Info and Exchange endpoint includes `vaultAddress`
  - `signer_env`: name of env var holding the private key for signing
- `active_profile` selects the default profile, override with `set --active_profile` at runtime
- `is_testnet: true` switches from mainnet to testnet

See more on how to generate Hyperliquid API wallets and private keys for safety: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/nonces-and-api-wallets

## Data

Source types; columns can be overridden):

### [crowdcent](https://crowdcent.com/challenge/hyperliquid-ranking/meta-model/)

```bash
cc-liquid rebalance --set data.source=crowdcent
```

Defaults: `date_column=release_date`, `asset_id_column=id`, `prediction_column=pred_10d`, `path=predictions.parquet`

### [numerai](https://crypto.numer.ai/meta-model)

Ensure you have installed extras: `uv pip install cc-liquid[numerai]`

```bash
cc-liquid rebalance --set data.source=numerai
```

Defaults: `date_column=date`, `asset_id_column=symbol`, `prediction_column=meta_model`, `path=predictions.parquet`

!!! tip
    Running commands with `--set data.source=numerai` can auto-apply column defaults for the Numerai metamodel.

### local

Bring your own Parquet/CSV:

```bash
cc-liquid rebalance \
  --set data.source=local \
  --set data.path=my_preds.parquet \
  --set data.date_column=dt \
  --set data.asset_id_column=ticker \
  --set data.prediction_column=score
```

Column rules:

- `date_column`: latest row per asset is used
- `asset_id_column`: must match Hyperliquid symbols; unmatched are skipped
- `prediction_column`: ranking for longs/shorts grouped by date

## Portfolio

- `num_long` / `num_short`: counts for top/bottom selections
- `target_leverage`: scales notional per-position like `(account_value * target_leverage) / (num_long + num_short)`.
- `weighting_scheme`: position sizing method (`equal`, `rank_power`) - see [Portfolio Weighting](portfolio-weighting.md)
- `rank_power`: concentration parameter when using `rank_power` scheme (default: 1.5)
- `rebalancing.every_n_days` / `rebalancing.at_time` (UTC)

## Execution

- `slippage_tolerance`: used for market orders
- `min_trade_value`: trades below this absolute USD delta are skipped

## CLI overrides

Examples:

```bash
cc-liquid run --set active_portfolio=default
```

```bash
cc-liquid rebalance --set data.source=numerai --set portfolio.target_leverage=2.0 --set portfolio.num_long=12
```

Nested keys use dot-notation. Types are inferred (int/float/bool/str).

Smart defaults when switching `data.source` are applied unless explicitly overridden.

## CLI helpers

- `cc-liquid profile list | show | use <name>` – manage profiles
