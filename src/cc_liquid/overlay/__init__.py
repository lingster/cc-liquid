"""deepLOB execution overlay (P3) — micro-execution timing for the rebalancer.

A model decision service (hl-live --serve) supplies calibrated up/down/flat
probabilities per coin; the overlay times each child order (cross now / wait /
rest passive) to reduce implementation shortfall on flow that already pays
fees. Disabled by default and fail-closed: any error falls back to the
existing execution path.
"""

from .config import OverlayConfig
from .gate import OverlayAction, OverlayDecision, decide
from .signal import MockSignalSource, Signal, SignalSource

__all__ = [
    "OverlayConfig",
    "OverlayAction",
    "OverlayDecision",
    "decide",
    "Signal",
    "SignalSource",
    "MockSignalSource",
]
