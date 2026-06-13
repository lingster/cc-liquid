"""HttpSignalSource — talks to the deepLOB decision service (hl-live --serve).

Implements the `SignalSource` port over HTTP. **Fail-closed**: any transport
error, non-200 status, or malformed body yields None, so the gate falls back
to the existing execution path and the overlay can never block a rebalance.

The HTTP call is injected (`http_get`) so the parsing/error-handling logic is
unit-tested without a live server; the default uses `requests`.

Wire contract (GET {url}/decide?coin=&horizon=&model=):
    200 -> {"coin","p_down","p_stationary","p_up","ts_event_ms","horizon"}
    204/404/5xx or unreachable -> treated as "no signal".
"""

from __future__ import annotations

import logging
from typing import Any, Callable

from .signal import Signal

logger = logging.getLogger(__name__)

# (url, params, timeout_sec) -> (status_code, json_body_or_None)
HttpGet = Callable[[str, dict[str, Any], float], "tuple[int, dict[str, Any] | None]"]

_REQUIRED = ("p_down", "p_stationary", "p_up", "ts_event_ms")


def _requests_get(
    url: str, params: dict[str, Any], timeout: float
) -> tuple[int, dict | None]:
    import requests

    resp = requests.get(url, params=params, timeout=timeout)
    try:
        body = resp.json()
    except ValueError:
        body = None
    return resp.status_code, body


class HttpSignalSource:
    """Fetch live signals from the decision service, one coin per call."""

    def __init__(
        self,
        url: str,
        horizon: int,
        model: str = "qf-ens5",
        timeout_sec: float = 2.0,
        http_get: HttpGet = _requests_get,
    ):
        self._endpoint = url.rstrip("/") + "/decide"
        self._horizon = horizon
        self._model = model
        self._timeout = timeout_sec
        self._http_get = http_get

    def get_signal(self, coin: str) -> Signal | None:
        params = {"coin": coin, "horizon": self._horizon, "model": self._model}
        try:
            status, body = self._http_get(self._endpoint, params, self._timeout)
        except Exception as exc:  # fail closed on any transport error
            logger.warning("overlay signal fetch failed for %s: %s", coin, exc)
            return None
        if status != 200 or not isinstance(body, dict):
            return None
        if any(k not in body for k in _REQUIRED):
            logger.warning("overlay signal for %s missing fields: %s", coin, body)
            return None
        return Signal(
            coin=body.get("coin", coin),
            p_down=float(body["p_down"]),
            p_stationary=float(body["p_stationary"]),
            p_up=float(body["p_up"]),
            ts_event_ms=int(body["ts_event_ms"]),
            horizon=int(body.get("horizon", self._horizon)),
        )
