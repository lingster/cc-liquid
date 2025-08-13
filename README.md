# cc-liquid

> ⚠️ **PRE-ALPHA SOFTWARE - USE AT YOUR OWN RISK** ⚠️
> 
> **CRITICAL WARNINGS:**
> - This is PRE-ALPHA software provided as a reference implementation only
> - Using this software may result in COMPLETE LOSS of funds
> - CrowdCent makes NO WARRANTIES and assumes NO LIABILITY for any losses
> - This software is provided "AS IS" without any guarantees
> - Users must comply with all Hyperliquid terms of service
> - We do NOT endorse any vaults, strategies, or implementations using this tool
> 
> **By using this software, you acknowledge that you understand and accept ALL risks**


## Overview

cc-liquid is a simple metamodel-based rebalancer for Hyperliquid that:
- Downloads metamodel predictions from CrowdCent or Numerai
- Identifies top long and bottom short opportunities
- Rebalances your Hyperliquid portfolio based on predictions
- Manages position sizing and risk automatically based on portfolio configurations

Read the docs: [documentation](https://crowdcent.github.io/cc-liquid)

## Installation

```bash
uv pip install cc-liquid
# Optional Numerai support
uv pip install cc-liquid[numerai]
```

### Shell completion (optional)

- Quick install (auto-detects your shell):

```bash
cc-liquid completion install
```

## Configuration

### Secrets in .env (only)

Create a `.env` for secrets (never commit private keys):

```env
# CrowdCent API
CROWDCENT_API_KEY=...

# Signer private keys (owner key or approved agent wallet(s) keys)
# You can define multiple for different profiles
HYPERLIQUID_PRIVATE_KEY=0x...
HYPER_AGENT_KEY_PERSONAL=0x...
HYPER_AGENT_KEY_VAULT=0x...
```

Get your keys from `https://crowdcent.com/profile` and `https://app.hyperliquid.xyz/API`.

### Addresses and Profiles in YAML

Keep addresses and selection in `cc-liquid-config.yaml` (not in `.env`):

```yaml
profiles:
  default:
    owner: 0xYourMain # Set to the main owner (never the API/Agent wallet address)
    vault: null                 # omit or set to null for personal portfolio rebalancing
    signer_env: HYPERLIQUID_PRIVATE_KEY # DO NOT WRITE YOUR ACTUAL KEY HERE, JUST THE ENVIRONMENT VARIABLE NAME

  my-vault:
    owner: 0xYourMain           # optional, informational
    vault: 0xVaultAddress
    signer_env: HYPER_AGENT_KEY_VAULT

active_profile: default

data:
  source: crowdcent
  path: predictions.parquet
portfolio:
  num_long: 10
  num_short: 10
  target_leverage: 1.0
execution:
  slippage_tolerance: 0.005
  min_trade_value: 10.0
```

Resolution order per run:
- `--profile` flag > `--set` overrides > `active_profile` in YAML > env fallback for signer key

What each field means:
- `owner`: portfolio owner (used for Info queries when `vault` not set)
- `vault`: optional; when set, becomes the portfolio owner for Info and orders include `vaultAddress`
- `signer_env`: name of env var holding the private key for signing
- `network`: `mainnet` or `testnet`

Run-time helpers:
- `cc-liquid profile list | show | use <name>`
- Quick override: `--profile`, `--owner`, `--vault`, `--signer-env`, `--network`

**Agent Wallets (Recommended for automation):**
- Use a separate agent key per bot/process for nonce isolation and safer ops
- Approve the agent wallet in the Hyperliquid UI/API, then set its private key in `.env`
- Never use the agent wallet address for Info queries (balances live on owner/vault)

### Optional Configuration

Parameters can be customized via `cc-liquid-config.yaml`. See `docs/configuration.md` for all options.

### Leverage Configuration

cc-liquid supports leveraged trading through Hyperliquid's perpetual futures. Configure leverage in `cc-liquid-config.yaml`:

```yaml
portfolio:
  target_leverage: 2.0  # Position sizing multiplier (1.0 = no leverage, 2.0 = 2x, etc.)
```

When leverage is used (multiplier > 1.0):
- Position sizes are calculated as `(account_value * target_leverage) / num_positions`
- This creates leveraged positions by sizing them larger than your account equity
- Leverage settings are displayed in the rebalancing plan

**Important**: Higher leverage increases risk of liquidation and bankruptcy. Always monitor your positions when using leverage.

## Usage

### Account Information

```bash
cc-liquid account   # show account summary and positions
```

### Download predictions

```bash
cc-liquid download-crowdcent -o predictions.parquet
cc-liquid download-numerai -o predictions.parquet
```

### Portfolio Management and Rebalancing

```bash
cc-liquid rebalance
# with overrides
cc-liquid rebalance --set data.source=numerai --set portfolio.num_long=10 --set portfolio.num_short=5
cc-liquid rebalance --set portfolio.target_leverage=2.0 --set execution.slippage_tolerance=0.01

# Autopilot (continuous)
cc-liquid run --skip-confirm   # WARNING: executes trades automatically
```

### Configuration

```bash
cc-liquid config
```

#### Configuration Overrides

You can override any configuration value at runtime for `rebalance` or `run` using the `--set` flag, like:

```bash
cc-liquid rebalance --set data.source=numerai --set portfolio.num_long=20 --set portfolio.target_leverage=2.0
```


**Smart Defaults**: When switching data sources, column mappings are auto-updated:

- `--set data.source=numerai` → Auto-applies `date_column=date`, `asset_id_column=symbol`, `prediction_column=meta_model`
- `--set data.source=crowdcent` → Auto-applies `date_column=release_date`, `asset_id_column=id`

Other possible options:
```bash
# Data source settings
--set data.source=numerai                    # Switch to Numerai (auto-applies column defaults)
--set data.source=crowdcent                  # Switch to CrowdCent  
--set data.source=local                      # Use local file
--set data.path=my_predictions.parquet       # Custom prediction file

# Portfolio settings
--set portfolio.num_long=15                  # Number of long positions
--set portfolio.num_short=8                  # Number of short positions  
--set portfolio.target_leverage=3.0          # Leverage multiplier

# Execution settings
--set execution.slippage_tolerance=0.005     # Slippage tolerance (0.5%)

# Environment settings
--set is_testnet=true                        # Use testnet instead of mainnet
```


## How It Works

1. **Download Metamodel**: Fetches the latest consolidated predictions from CrowdCent or Numerai
2. **Select Assets**: Identifies top N assets for long positions and bottom N for short positions based on 10-day predictions
3. **Calculate Positions**: Determines target position sizes based on account value and configuration
4. **Generate Trades**: Calculates required trades to move from current to target positions
5. **Execute Orders**: Places market orders with configured slippage tolerance

## Future Enhancements

- Neutralize to known risk factors
- Unequal-weight position sizing
- Backtest/analyze various configurations


## Disclaimer

THIS SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

This is PRE-ALPHA software intended as a reference implementation only. Users assume ALL risks associated with using this software, including but not limited to:
- Complete loss of funds
- Trading losses
- Technical failures
- Liquidation risks
- Any other financial losses

CrowdCent does not endorse, recommend, or provide support for any trading strategies, vaults, or implementations using this software. Users must independently verify all functionality and assume full responsibility for their trading decisions.

By using this software, you agree to comply with all applicable laws and regulations, including Hyperliquid's terms of service.
