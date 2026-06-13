"""OverlayExecutor — works a single child order under the decision gate.

Depends only on ports (SignalSource, OrderGateway, Clock), so the timing
policy is unit-tested with fakes and zero real orders. The trader supplies a
concrete OrderGateway backed by the Hyperliquid SDK.

Policy per order:
  CROSS_NOW    -> cross() and finish
  REST_PASSIVE -> rest_passive() for one snapshot; if it fills, finish; else
                  count a snapshot and re-poll
  WAIT         -> count a snapshot, sleep, re-poll
  FALLBACK     -> fallback() to the existing path and finish
  After `max_wait_snaps` snapshots without a fill -> cross(), so a rebalance is
  never left unexecuted. The loop is therefore bounded by max_wait_snaps.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol, runtime_checkable

from .config import OverlayConfig
from .gate import OverlayAction, decide
from .signal import SignalSource


@dataclass(frozen=True)
class FillResult:
    """Outcome of working one child order."""

    filled: bool
    avg_px: float | None
    detail: str


@runtime_checkable
class OrderGateway(Protocol):
    """The order actions the working loop needs, abstracted from the SDK."""

    def cross(self, coin: str, is_buy: bool, size: float) -> FillResult:
        """Take liquidity now (marketable / aggressive limit)."""
        ...

    def rest_passive(self, coin: str, is_buy: bool, size: float) -> FillResult:
        """Post-only at best for ~one snapshot; FillResult.filled says if it hit."""
        ...

    def fallback(self, coin: str, is_buy: bool, size: float) -> FillResult:
        """Execute via the existing (non-overlay) path."""
        ...


class Clock(Protocol):
    def now_ms(self) -> int: ...
    def sleep_ms(self, ms: int) -> None: ...


class OverlayExecutor:
    def __init__(
        self,
        source: SignalSource,
        gateway: OrderGateway,
        cfg: OverlayConfig,
        clock: Clock,
    ):
        self._source = source
        self._gateway = gateway
        self._cfg = cfg
        self._clock = clock

    def work(self, coin: str, is_buy: bool, size: float) -> FillResult:
        cfg, gw = self._cfg, self._gateway
        waited = 0
        while True:
            signal = self._source.get_signal(coin)
            decision = decide(
                signal, is_buy=is_buy, now_ms=self._clock.now_ms(), cfg=cfg
            )

            if decision.action is OverlayAction.FALLBACK:
                return gw.fallback(coin, is_buy, size)
            if decision.action is OverlayAction.CROSS_NOW:
                return gw.cross(coin, is_buy, size)

            if decision.action is OverlayAction.REST_PASSIVE:
                rested = gw.rest_passive(coin, is_buy, size)
                if rested.filled:
                    return rested
            # REST_PASSIVE (unfilled) and WAIT both consume one snapshot.

            waited += 1
            if waited >= cfg.max_wait_snaps:
                return gw.cross(coin, is_buy, size)  # timeout: guarantee execution
            self._clock.sleep_ms(cfg.snapshot_ms)
