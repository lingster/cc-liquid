"""Red/green tests for HyperliquidOrderGateway — order pricing/routing.

Implements the OrderGateway port the executor needs, behind two small ports
(MarketData, OrderSubmitter) so pricing is tested without the SDK:
  cross        -> marketable IOC across the spread (buy up to ask, sell to bid)
  rest_passive -> post-only (Alo) joining the best quote (no spread paid)
  fallback     -> delegates to the trader's existing execution path (DRY)
"""

from cc_liquid.overlay import FillResult, OrderGateway
from cc_liquid.overlay.gateway import HyperliquidOrderGateway, OrderRequest


class FakeMarket:
    def best_bid_ask(self, coin):
        return (100.0, 100.10)  # bid, ask

    def round_price(self, coin, px):
        return round(px, 2)


class FakeSubmitter:
    def __init__(self):
        self.submitted: list[OrderRequest] = []

    def submit(self, req: OrderRequest) -> FillResult:
        self.submitted.append(req)
        return FillResult(filled=True, avg_px=req.limit_px, detail=req.tif)


def make(slippage=0.0005):
    market, sub = FakeMarket(), FakeSubmitter()
    fb_calls = []

    def fallback_fn(coin, is_buy, size):
        fb_calls.append((coin, is_buy, size))
        return FillResult(filled=True, avg_px=100.05, detail="fallback")

    gw = HyperliquidOrderGateway(market, sub, fallback_fn, slippage=slippage)
    return gw, sub, fb_calls


def test_is_an_order_gateway():
    gw, _, _ = make()
    assert isinstance(gw, OrderGateway)


def test_cross_buy_is_marketable_ioc_above_ask():
    gw, sub, _ = make(slippage=0.001)
    gw.cross("BTC", is_buy=True, size=1.0)
    req = sub.submitted[-1]
    assert req.tif == "Ioc" and req.post_only is False
    assert req.is_buy is True
    assert req.limit_px >= 100.10  # crosses up to/through the ask


def test_cross_sell_is_marketable_ioc_below_bid():
    gw, sub, _ = make(slippage=0.001)
    gw.cross("BTC", is_buy=False, size=1.0)
    req = sub.submitted[-1]
    assert (
        req.tif == "Ioc" and req.limit_px <= 100.00
    )  # crosses down to/through the bid


def test_rest_passive_buy_posts_alo_at_best_bid():
    gw, sub, _ = make()
    gw.rest_passive("BTC", is_buy=True, size=1.0)
    req = sub.submitted[-1]
    assert req.tif == "Alo" and req.post_only is True
    assert req.limit_px == 100.00  # joins the best bid, pays no spread


def test_rest_passive_sell_posts_alo_at_best_ask():
    gw, sub, _ = make()
    gw.rest_passive("BTC", is_buy=False, size=1.0)
    req = sub.submitted[-1]
    assert req.tif == "Alo" and req.limit_px == 100.10


def test_fallback_delegates_to_existing_path():
    gw, sub, fb_calls = make()
    r = gw.fallback("BTC", is_buy=True, size=2.0)
    assert r.detail == "fallback"
    assert fb_calls == [("BTC", True, 2.0)]
    assert sub.submitted == []  # did NOT use the overlay submitter
