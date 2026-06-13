"""Red/green tests for the execution-overlay decision gate (pure logic).

The gate maps a model signal + order side to one of: cross now (taker),
wait, rest passive (maker), or fall back to the existing execution path.
P3 logic (docs/experiments.md §16, deepLOB todo P3):
  BUY:  p_up≥conf → CROSS_NOW; p_dn≥conf → WAIT; else REST_PASSIVE
  SELL: mirror (p_dn≥conf → CROSS_NOW; p_up≥conf → WAIT; else REST_PASSIVE)
No/!stale/out-of-universe signal → FALLBACK (never block a rebalance).
"""

import pytest

from cc_liquid.overlay import OverlayAction, OverlayConfig, Signal, decide


def sig(p_up: float, p_dn: float, ts_ms: int = 1_000_000) -> Signal:
    p_stat = max(0.0, 1.0 - p_up - p_dn)
    return Signal(
        coin="BTC",
        p_down=p_dn,
        p_stationary=p_stat,
        p_up=p_up,
        ts_event_ms=ts_ms,
        horizon=10,
    )


@pytest.fixture
def cfg() -> OverlayConfig:
    return OverlayConfig(enabled=True, confidence=0.6, max_staleness_ms=2000)


NOW = 1_000_500  # 500 ms after the default signal timestamp


def test_buy_bullish_crosses_now(cfg):
    d = decide(sig(p_up=0.7, p_dn=0.1), is_buy=True, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.CROSS_NOW


def test_buy_bearish_waits(cfg):
    d = decide(sig(p_up=0.1, p_dn=0.7), is_buy=True, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.WAIT


def test_buy_neutral_rests_passive(cfg):
    d = decide(sig(p_up=0.3, p_dn=0.3), is_buy=True, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.REST_PASSIVE


def test_sell_bearish_crosses_now(cfg):
    # Selling into a predicted drop: cross now before it falls.
    d = decide(sig(p_up=0.1, p_dn=0.7), is_buy=False, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.CROSS_NOW


def test_sell_bullish_waits(cfg):
    # Price predicted to rise: wait to sell higher.
    d = decide(sig(p_up=0.7, p_dn=0.1), is_buy=False, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.WAIT


def test_sell_neutral_rests_passive(cfg):
    d = decide(sig(p_up=0.3, p_dn=0.3), is_buy=False, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.REST_PASSIVE


def test_threshold_is_inclusive(cfg):
    d = decide(sig(p_up=0.6, p_dn=0.1), is_buy=True, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.CROSS_NOW


def test_missing_signal_falls_back(cfg):
    d = decide(None, is_buy=True, now_ms=NOW, cfg=cfg)
    assert d.action is OverlayAction.FALLBACK
    assert "signal" in d.reason.lower()


def test_stale_signal_falls_back(cfg):
    old = sig(p_up=0.7, p_dn=0.1, ts_ms=1_000_000)
    d = decide(old, is_buy=True, now_ms=1_000_000 + 5_000, cfg=cfg)  # 5 s old > 2 s
    assert d.action is OverlayAction.FALLBACK
    assert "stale" in d.reason.lower()


def test_fresh_signal_at_staleness_boundary_is_used(cfg):
    s = sig(p_up=0.7, p_dn=0.1, ts_ms=1_000_000)
    d = decide(s, is_buy=True, now_ms=1_000_000 + 2_000, cfg=cfg)  # exactly 2 s
    assert d.action is OverlayAction.CROSS_NOW


def test_decision_carries_signal_and_reason(cfg):
    s = sig(p_up=0.7, p_dn=0.1)
    d = decide(s, is_buy=True, now_ms=NOW, cfg=cfg)
    assert d.signal is s
    assert isinstance(d.reason, str) and d.reason
