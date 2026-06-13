"""Red/green tests for OverlayExecutor — the per-order working loop.

Drives the gate over time against injected ports (OrderGateway, Clock,
SignalSource), so the timing policy is tested with zero real orders and zero
wall-clock waits. Policy:
  CROSS_NOW    -> take liquidity, done
  REST_PASSIVE -> post-only for one snapshot; if it fills, done; else re-poll
  WAIT         -> hold one snapshot, re-poll
  FALLBACK     -> hand to the existing execution path, done
  timeout (max_wait_snaps elapsed and still not done) -> cross, so a rebalance
  is never left unexecuted.
"""

from cc_liquid.overlay import OverlayConfig, Signal
from cc_liquid.overlay.executor import FillResult, OverlayExecutor


class FakeClock:
    def __init__(self):
        self.t = 1_000_000
        self.sleeps = 0

    def now_ms(self) -> int:
        return self.t

    def sleep_ms(self, ms: int) -> None:
        self.sleeps += 1
        self.t += ms


class ScriptedSource:
    """Yields a programmed sequence of signals (last value repeats)."""

    def __init__(self, seq):
        self._seq = list(seq)
        self.calls = 0

    def get_signal(self, coin):
        s = self._seq[min(self.calls, len(self._seq) - 1)]
        self.calls += 1
        return s


class RecordingGateway:
    def __init__(self, passive_fills=()):
        self.calls = []
        self._passive_fills = list(passive_fills)

    def cross(self, coin, is_buy, size):
        self.calls.append(("cross", coin, is_buy, size))
        return FillResult(filled=True, avg_px=100.0, detail="taker")

    def rest_passive(self, coin, is_buy, size):
        fills = self._passive_fills.pop(0) if self._passive_fills else False
        self.calls.append(("rest_passive", coin, is_buy, size))
        return FillResult(filled=fills, avg_px=99.0 if fills else None, detail="maker")

    def fallback(self, coin, is_buy, size):
        self.calls.append(("fallback", coin, is_buy, size))
        return FillResult(filled=True, avg_px=100.5, detail="fallback")


def sig(p_up, p_dn, ts=1_000_000):
    return Signal("BTC", p_dn, max(0.0, 1 - p_up - p_dn), p_up, ts, 10)


def make(source, gateway, max_wait=10):
    cfg = OverlayConfig(
        enabled=True,
        confidence=0.6,
        max_wait_snaps=max_wait,
        snapshot_ms=540,
        max_staleness_ms=10_000,
    )
    clock = FakeClock()
    return OverlayExecutor(source, gateway, cfg, clock), clock


def kinds(gw):
    return [c[0] for c in gw.calls]


def test_cross_now_takes_immediately():
    gw = RecordingGateway()
    ex, _ = make(ScriptedSource([sig(0.7, 0.1)]), gw)
    r = ex.work("BTC", is_buy=True, size=1.0)
    assert r.filled and kinds(gw) == ["cross"]


def test_no_signal_falls_back():
    gw = RecordingGateway()
    ex, _ = make(ScriptedSource([None]), gw)
    ex.work("BTC", is_buy=True, size=1.0)
    assert kinds(gw) == ["fallback"]


def test_passive_fill_completes_without_crossing():
    gw = RecordingGateway(passive_fills=[True])
    ex, _ = make(ScriptedSource([sig(0.3, 0.3)]), gw)
    r = ex.work("BTC", is_buy=True, size=1.0)
    assert r.detail == "maker" and kinds(gw) == ["rest_passive"]


def test_wait_then_flip_to_cross():
    # Bearish (wait) for two snapshots, then turns bullish -> cross.
    gw = RecordingGateway()
    src = ScriptedSource([sig(0.1, 0.7), sig(0.1, 0.7), sig(0.8, 0.05)])
    ex, clock = make(src, gw)
    ex.work("BTC", is_buy=True, size=1.0)
    assert kinds(gw) == ["cross"] and clock.sleeps == 2


def test_wait_times_out_into_cross():
    gw = RecordingGateway()
    ex, _ = make(ScriptedSource([sig(0.1, 0.7)]), gw, max_wait=3)
    ex.work("BTC", is_buy=True, size=1.0)
    # waited 3 snapshots, then crossed to guarantee execution
    assert kinds(gw) == ["cross"]


def test_passive_never_fills_times_out_into_cross():
    gw = RecordingGateway(passive_fills=[False, False, False, False])
    ex, _ = make(ScriptedSource([sig(0.3, 0.3)]), gw, max_wait=3)
    ex.work("BTC", is_buy=True, size=1.0)
    assert kinds(gw)[-1] == "cross"
    assert kinds(gw).count("rest_passive") <= 3


def test_loop_is_bounded():
    # Pathological: always WAIT, must still terminate within max_wait+1 actions.
    gw = RecordingGateway()
    ex, _ = make(ScriptedSource([sig(0.0, 0.9)]), gw, max_wait=5)
    ex.work("BTC", is_buy=True, size=1.0)
    assert len(gw.calls) == 1 and kinds(gw) == ["cross"]
