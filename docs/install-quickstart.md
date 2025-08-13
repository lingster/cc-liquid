---
title: Install & Quick Start
---

This page gets you running in minutes.

## Prerequisites

- Hyperliquid API/Agent wallet (address + private key)
- Optional: CrowdCent API key

## Install

```bash
uv pip install cc-liquid
# Optional Numerai support
uv pip install cc-liquid[numerai]
```

### Enable tab auto-completion in your shell (optional)

Tabs for commands, options, and values.

```bash
cc-liquid completion install          # auto-detects your shell
```

Manual equivalent:

=== "Bash"
    ```bash
    _CC_LIQUID_COMPLETE=bash_source cc-liquid > ~/.cc-liquid-complete.bash
    echo '. ~/.cc-liquid-complete.bash' >> ~/.bashrc
    ```

=== "Zsh"
    ```bash
    _CC_LIQUID_COMPLETE=zsh_source cc-liquid > ~/.cc-liquid-complete.zsh
    echo '. ~/.cc-liquid-complete.zsh' >> ~/.zshrc
    ```

=== "Fish"
    ```bash
    mkdir -p ~/.config/fish/completions
    _CC_LIQUID_COMPLETE=fish_source cc-liquid > ~/.config/fish/completions/cc-liquid.fish
    ```

Restart your shell to activate completion, or run `source ~/.bashrc`, `source ~/.zshrc`, etc. as needed.  
See [Click Shell Completion](https://click.palletsprojects.com/en/stable/shell-completion/) for details.

## Configure

1) Create `.env` in your working directory (never commit secrets):

```env
# Secrets only
CROWDCENT_API_KEY=...                # from your CrowdCent profile
HYPERLIQUID_PRIVATE_KEY=0x...        # default signer key name
HYPER_AGENT_KEY_VAULT=0x...          # optional: assign profile-specific signer keys
```

2) Create `cc-liquid-config.yaml` with addresses and profiles:

```yaml
active_profile: personal

profiles:
  personal:
    owner: 0xYourMain
    vault: null
    signer_env: HYPER_AGENT_KEY_PERSONAL

data:
  source: crowdcent         # crowdcent | numerai | local
  path: predictions.parquet
  date_column: release_date
  asset_id_column: id
  prediction_column: pred_10d

portfolio:
  num_long: 10
  num_short: 10
  target_leverage: 1.0
  rebalancing:
    every_n_days: 10
    at_time: "18:15"

execution:
  slippage_tolerance: 0.005
```

## First run

```bash
cc-liquid config     # verify config is loaded
cc-liquid account    # view account and positions
cc-liquid rebalance  # plan and execute trades (prompts for confirmation)
```

Use overrides without editing files:

```bash
cc-liquid rebalance --set data.source=numerai --set portfolio.num_long=20 --set portfolio.target_leverage=2.0
```

### Testnet first (recommended)

To avoid live trading while getting set up, use Hyperliquid testnet:

Use a testnet profile or override `network: testnet`:

```bash
cc-liquid rebalance --set is_testnet=true   # or set profile.network=testnet
```

## Autopilot (continuous)

Runs a live dashboard and executes on your schedule.

```bash
cc-liquid run --skip-confirm   # WARNING: executes trades automatically
```

See the [User Guide](walkthrough.md) for a deeper walkthrough.


