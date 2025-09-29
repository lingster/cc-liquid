# Portfolio Weighting Schemes

cc-liquid supports rank power position sizing methods to go beyond equal weighting, allowing you to concentrate capital in your highest-conviction positions.

## Visual Overview

### Example schemes impact on position sizing

![Portfolio Weighting Grid](assets/weighting_schemes_grid.png)

The grid above shows how three different weighting schemes distribute capital across long and short positions in various portfolio configurations. Notice how rank power creates concentration in top-ranked positions while maintaining the target leverage.

### Concentration Effects

![Concentration Curves](assets/concentration_curves.png)

These curves demonstrate how the rank power parameter controls concentration:

- **Power = 0** (equal weight): All positions get the same allocation
- **Power = 0.5-1.0**: Mild concentration favoring top positions  
- **Power = 1.5-2.0**: Moderate concentration
- **Power = 3.0+**: Heavy concentration in top few positions

## Configuration

### YAML Configuration

```yaml
portfolio:
  num_long: 60
  num_short: 50
  target_leverage: 4.0
  weighting_scheme: rank_power  # Options: equal, rank_power
  rank_power: 1.5               # Used when scheme is rank_power (default: 1.5)
```

### CLI Override

```bash
# Backtest with rank-weighted positions
uv run -m cc_liquid.cli analyze \
  --set portfolio.weighting_scheme=rank_power \
  --set portfolio.rank_power=1.5

# Live trading with concentration
uv run -m cc_liquid.cli rebalance \
  --set portfolio.weighting_scheme=rank_power \
  --set portfolio.rank_power=2.0
```

## Weighting Schemes

### Equal Weight (Default)
Every position gets an equal share of the allocated capital. Simple and diversified.

```yaml
weighting_scheme: equal
```

### Rank Power
Positions are weighted by `(rank/n)^power`. Higher-ranked positions (stronger signals) get more capital.

```yaml
weighting_scheme: rank_power
rank_power: 0.5
```
