"""Signal value object and the SignalSource port (Dependency Inversion).

`Signal` is the calibrated model output for one coin at one horizon. The gate
and the order-working loop depend on the `SignalSource` Protocol, never on a
concrete transport — so the decision-service HTTP client and the test mock are
interchangeable.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol, runtime_checkable


@dataclass(frozen=True)
class Signal:
    """Calibrated (p_down, p_stationary, p_up) for `coin` at `horizon`.

    `ts_event_ms` is the exchange event time of the book snapshot the
    prediction was made from — used for the staleness guard (NOT receipt or
    request time).
    """

    coin: str
    p_down: float
    p_stationary: float
    p_up: float
    ts_event_ms: int
    horizon: int

    def age_ms(self, now_ms: int) -> int:
        """Milliseconds between the source snapshot and `now_ms`."""
        return now_ms - self.ts_event_ms


@runtime_checkable
class SignalSource(Protocol):
    """A source of live model signals, keyed by coin.

    Returns None when the coin is not served (out of the model's universe) or
    no fresh prediction is available — the gate treats None as "fall back".
    """

    def get_signal(self, coin: str) -> Signal | None: ...


class MockSignalSource:
    """In-memory SignalSource for tests and dry runs."""

    def __init__(self, signals: dict[str, Signal]):
        self._signals = dict(signals)

    def get_signal(self, coin: str) -> Signal | None:
        return self._signals.get(coin)
