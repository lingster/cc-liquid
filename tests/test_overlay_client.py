"""Red/green tests for HttpSignalSource — the decision-service client.

Fail-closed: any transport error, non-200, or malformed payload yields None
so the gate falls back to the existing execution path. The HTTP call is
injected (DIP), so these tests need no live server.
"""

from cc_liquid.overlay import Signal, SignalSource
from cc_liquid.overlay.client import HttpSignalSource

OK_BODY = {
    "coin": "BTC",
    "p_down": 0.1,
    "p_stationary": 0.2,
    "p_up": 0.7,
    "ts_event_ms": 1_700_000_000_000,
    "horizon": 10,
}


def make(http_get):
    return HttpSignalSource(
        url="http://127.0.0.1:8080", horizon=10, model="qf-ens5", http_get=http_get
    )


def test_parses_ok_response_into_signal():
    src = make(lambda url, params, timeout: (200, OK_BODY))
    s = src.get_signal("BTC")
    assert isinstance(s, Signal)
    assert (s.coin, s.p_up, s.ts_event_ms, s.horizon) == (
        "BTC",
        0.7,
        1_700_000_000_000,
        10,
    )


def test_sends_coin_horizon_model_in_request():
    captured = {}

    def http_get(url, params, timeout):
        captured["url"] = url
        captured["params"] = params
        captured["timeout"] = timeout
        return (200, OK_BODY)

    make(http_get).get_signal("BTC")
    assert captured["params"]["coin"] == "BTC"
    assert captured["params"]["horizon"] == 10
    assert captured["params"]["model"] == "qf-ens5"


def test_non_200_returns_none():
    assert make(lambda u, p, t: (404, None)).get_signal("BTC") is None


def test_transport_error_returns_none():
    def boom(url, params, timeout):
        raise TimeoutError("connection timed out")

    assert make(boom).get_signal("BTC") is None  # fail closed


def test_malformed_payload_returns_none():
    assert make(lambda u, p, t: (200, {"coin": "BTC"})).get_signal("BTC") is None


def test_is_a_signal_source():
    assert isinstance(make(lambda u, p, t: (200, OK_BODY)), SignalSource)
