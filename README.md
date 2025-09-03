# cc-liquid

> ⚠️ **PRE-ALPHA SOFTWARE - USE AT YOUR OWN RISK** ⚠️
> 
> This is PRE-ALPHA software provided as a reference implementation only. By using this software, you acknowledge:
> - Risk of COMPLETE LOSS of funds - CrowdCent makes NO WARRANTIES and assumes NO LIABILITY
> - Software is provided "AS IS" without any guarantees
> - You must comply with all Hyperliquid terms of service
> - We do NOT endorse any vaults, strategies, or implementations using this tool

## Overview

cc-liquid is a simple metamodel-based rebalancer for Hyperliquid that:
- Downloads metamodel predictions from CrowdCent or Numerai
- Identifies top long and bottom short opportunities
- Rebalances your Hyperliquid portfolio based on predictions
- Manages position sizing and risk automatically based on portfolio configurations

Read the docs: [documentation](https://crowdcent.github.io/cc-liquid)

![dashboard](docs/images/dashboard.svg)

## Installation

```bash
# Install globally
uv tool install cc-liquid

# With Numerai support
uv tool install cc-liquid[numerai]

# Or run without installing
uvx cc-liquid --help

# Optional: Enable shell completion
cc-liquid completion install
```

## Quick Start

```bash
cc-liquid init        # Interactive setup wizard (recommended for new users)
```

The wizard guides you through:
- Choosing testnet (recommended) or mainnet
- Selecting data source (CrowdCent/Numerai/local)
- Entering API keys securely
- Setting portfolio parameters
- Auto-configuring for safety

## Configuration

### Secrets (.env only - never commit)

```env
# API Keys
CROWDCENT_API_KEY=...

# Signer private keys (owner key or approved agent wallet keys)
HYPERLIQUID_PRIVATE_KEY=0x...
HYPER_AGENT_KEY_PERSONAL=0x...
HYPER_AGENT_KEY_VAULT=0x...
```

Get your keys from:
- CrowdCent: `https://crowdcent.com/profile`
- Hyperliquid: `https://app.hyperliquid.xyz/API`

### Settings (cc-liquid-config.yaml)

```yaml
profiles:
  default:
    owner: 0xYourMain           # Main owner address (never the API/Agent wallet)
    vault: null                 # null for personal, or vault address
    signer_env: HYPERLIQUID_PRIVATE_KEY  # ENV var name (not the key itself)

  my-vault:
    owner: 0xYourMain           # optional, informational
    vault: 0xVaultAddress
    signer_env: HYPER_AGENT_KEY_VAULT

active_profile: default

data:
  source: crowdcent             # crowdcent, numerai, or local
  path: predictions.parquet

portfolio:
  num_long: 10
  num_short: 10
  target_leverage: 1.0          # 1.0 = no leverage, 2.0 = 2x, etc.

execution:
  slippage_tolerance: 0.005
  min_trade_value: 10.0
```

**Profile Management:**
```bash
cc-liquid profile list
cc-liquid profile show
cc-liquid profile use <name>
```

**Agent Wallets (Recommended for automation):**
- Use separate agent keys per bot for nonce isolation
- Approve agent in Hyperliquid UI, then add private key to `.env`
- Never use agent address for Info queries (use owner/vault)

## Usage

### Basic Commands

```bash
# Account info
cc-liquid account

# Download predictions
cc-liquid download-crowdcent -o predictions.parquet
cc-liquid download-numerai -o predictions.parquet

# Rebalance portfolio
cc-liquid rebalance

# Continuous rebalancing (WARNING: auto-executes)
cc-liquid run --skip-confirm
```

### Configuration Overrides

Use `--set` to override any config value at runtime:

```bash
# Data source (auto-applies column mappings)
--set data.source=numerai      # → date, symbol, meta_model columns
--set data.source=crowdcent    # → release_date, id columns
--set data.path=custom.parquet

# Portfolio
--set portfolio.num_long=15
--set portfolio.num_short=8
--set portfolio.target_leverage=2.0

# Execution
--set execution.slippage_tolerance=0.01

# Environment
--set is_testnet=true
```

Example combinations:
```bash
cc-liquid rebalance --set data.source=numerai --set portfolio.num_long=20 --set portfolio.target_leverage=2.0
```

## How It Works

1. **Download Metamodel**: Fetches consolidated predictions from CrowdCent or Numerai
2. **Select Assets**: Identifies top N longs and bottom N shorts based on 10-day predictions
3. **Calculate Positions**: Determines target sizes based on account value and leverage
4. **Generate Trades**: Calculates required trades from current to target positions
5. **Execute Orders**: Places market orders with configured slippage tolerance

## Backtesting & Optimization

cc-liquid includes backtesting and optimization tools to test strategies on historical data:

```bash
cc-liquid analyze                    # Run backtest with current config
cc-liquid optimize                   # Find optimal parameters via grid search
```

> ⚠️ **BACKTESTING DISCLAIMER** ⚠️
> 
> **Past performance does not predict future results.** Backtesting has inherent limitations:
> - Results are hypothetical and may not reflect real trading conditions
> - Optimized parameters are prone to overfitting
> - Market conditions, liquidity, and execution costs change over time
> - Always validate with out-of-sample data and paper trading before live deployment
> 
> See [full documentation](https://crowdcent.github.io/cc-liquid/backtesting/) for details.

## Future Enhancements

- Neutralize to known risk factors
- Unequal-weight position sizing
- Advanced portfolio optimization

## License

MIT License - See LICENSE file for details