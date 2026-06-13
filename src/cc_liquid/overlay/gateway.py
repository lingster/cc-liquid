"""HyperliquidOrderGateway — concrete OrderGateway for the working loop.

Pricing/routing lives here, behind two small ports so it is unit-tested
without the SDK:

- `MarketData`: best bid/ask + price rounding for the coin.
- `OrderSubmitter`: send one order, return a FillResult (wraps the SDK
  bulk_orders + response parsing in the trader adapter).

`cross` crosses the spread as a marketable IOC; `rest_passive` joins the best
quote post-only (Alo) to capture spread; `fallback` delegates to the trader's
existing execution path so that behaviour is never duplicated (DRY).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Protocol

from .executor import FillResult


@dataclass(frozen=True)
class OrderRequest:
    coin: str
    is_buy: bool
    size: float
    limit_px: float
    tif: str  # "Ioc" | "Gtc" | "Alo"
    post_only: bool = False
    reduce_only: bool = False


class MarketData(Protocol):
    def best_bid_ask(self, coin: str) -> tuple[float, float]: ...
    def round_price(self, coin: str, px: float) -> float: ...


class OrderSubmitter(Protocol):
    def submit(self, req: OrderRequest) -> FillResult: ...


FallbackFn = Callable[[str, bool, float], FillResult]


class HyperliquidOrderGateway:
    def __init__(
        self,
        market: MarketData,
        submitter: OrderSubmitter,
        fallback_fn: FallbackFn,
        slippage: float = 0.0005,
    ):
        self._market = market
        self._submitter = submitter
        self._fallback = fallback_fn
        self._slippage = slippage

    def cross(self, coin: str, is_buy: bool, size: float) -> FillResult:
        bid, ask = self._market.best_bid_ask(coin)
        # Marketable: cross to the far touch with a slippage cushion so the IOC
        # actually clears (buy up through the ask, sell down through the bid).
        raw = ask * (1 + self._slippage) if is_buy else bid * (1 - self._slippage)
        px = self._market.round_price(coin, raw)
        return self._submitter.submit(OrderRequest(coin, is_buy, size, px, tif="Ioc"))

    def rest_passive(self, coin: str, is_buy: bool, size: float) -> FillResult:
        bid, ask = self._market.best_bid_ask(coin)
        # Join the near touch post-only: pay no spread, add liquidity.
        px = self._market.round_price(coin, bid if is_buy else ask)
        return self._submitter.submit(
            OrderRequest(coin, is_buy, size, px, tif="Alo", post_only=True)
        )

    def fallback(self, coin: str, is_buy: bool, size: float) -> FillResult:
        return self._fallback(coin, is_buy, size)
