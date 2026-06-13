"""The execution-overlay decision gate — pure, side-effect-free strategy.

Maps (signal, side) to an action. Kept pure so it is exhaustively unit-tested
without a feed, exchange, or clock. The order-working loop interprets the
action; this module only decides.

P3 logic (deepLOB docs/experiments.md §16):
  BUY:  p_up ≥ conf → CROSS_NOW; p_down ≥ conf → WAIT; else REST_PASSIVE
  SELL: mirror — p_down ≥ conf → CROSS_NOW; p_up ≥ conf → WAIT; else REST_PASSIVE
A missing / stale signal → FALLBACK, so the overlay can never block a trade.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum

from .config import OverlayConfig
from .signal import Signal


class OverlayAction(Enum):
    """What to do with the child order right now."""

    CROSS_NOW = "cross_now"  # take liquidity immediately (predicted move with us)
    WAIT = "wait"  # hold off — predicted move against us; re-evaluate next snapshot
    REST_PASSIVE = "rest_passive"  # post-only at best; capture spread, no fee
    FALLBACK = "fallback"  # no usable signal — defer to the existing execution path


@dataclass(frozen=True)
class OverlayDecision:
    action: OverlayAction
    reason: str
    signal: Signal | None = None


def decide(
    signal: Signal | None,
    *,
    is_buy: bool,
    now_ms: int,
    cfg: OverlayConfig,
) -> OverlayDecision:
    """Decide the action for one child order from the current signal."""
    if signal is None:
        return OverlayDecision(OverlayAction.FALLBACK, "no signal for coin", None)
    if signal.age_ms(now_ms) > cfg.max_staleness_ms:
        return OverlayDecision(
            OverlayAction.FALLBACK,
            f"stale signal ({signal.age_ms(now_ms)} ms > {cfg.max_staleness_ms} ms)",
            signal,
        )

    # "with us" = the move that helps this side: up for a buy, down for a sell.
    p_with = signal.p_up if is_buy else signal.p_down
    p_against = signal.p_down if is_buy else signal.p_up
    if p_with >= cfg.confidence:
        return OverlayDecision(
            OverlayAction.CROSS_NOW, "directional move with order", signal
        )
    if p_against >= cfg.confidence:
        return OverlayDecision(
            OverlayAction.WAIT, "directional move against order", signal
        )
    return OverlayDecision(
        OverlayAction.REST_PASSIVE, "neutral — capture spread", signal
    )
