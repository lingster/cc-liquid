"""Red/green tests for the Signal value object and the SignalSource port."""

from cc_liquid.overlay import MockSignalSource, Signal, SignalSource


def make(coin="BTC", ts_ms=1_000_000) -> Signal:
    return Signal(
        coin=coin, p_down=0.2, p_stationary=0.3, p_up=0.5, ts_event_ms=ts_ms, horizon=10
    )


def test_signal_age_ms():
    s = make(ts_ms=1_000_000)
    assert s.age_ms(1_000_750) == 750


def test_signal_is_immutable():
    s = make()
    try:
        s.p_up = 0.9  # type: ignore[misc]
    except Exception:
        return
    raise AssertionError("Signal should be frozen/immutable")


def test_mock_signal_source_returns_configured():
    src = MockSignalSource({"BTC": make("BTC")})
    assert src.get_signal("BTC").coin == "BTC"


def test_mock_signal_source_unknown_coin_is_none():
    src = MockSignalSource({"BTC": make("BTC")})
    assert src.get_signal("DOGE") is None


def test_mock_satisfies_signal_source_protocol():
    src = MockSignalSource({})
    assert isinstance(src, SignalSource)
