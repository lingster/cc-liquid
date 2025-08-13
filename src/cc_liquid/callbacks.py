"""Callbacks for cc-liquid trader.py UI/UX abstraction.

This module defines the protocol and non-Rich implementations only, to keep the
core package free of UI dependencies. Rich-based callbacks live in
`cli_callbacks.py`.
"""

from typing import Any, Protocol


class CCLiquidCallbacks(Protocol):
    """Protocol for trader callbacks to abstract UI/UX concerns."""

    # High-level lifecycle methods
    def ask_confirmation(self, message: str) -> bool:
        """Ask user for confirmation."""
        ...

    def info(self, message: str) -> None:
        """Display info message."""
        ...

    def warn(self, message: str) -> None:
        """Display warning message."""
        ...

    def error(self, message: str) -> None:
        """Display error message."""
        ...

    def on_config_override(self, overrides: list[str]) -> None:
        """Display applied configuration overrides."""
        ...

    # Trade execution progress hooks
    def on_trade_start(self, idx: int, total: int, trade: dict[str, Any]) -> None:
        """Called when a trade execution starts."""
        ...

    def on_trade_fill(
        self, trade: dict[str, Any], fill_data: dict[str, Any], slippage_pct: float
    ) -> None:
        """Called when a trade is filled."""
        ...

    def on_trade_fail(self, trade: dict[str, Any], reason: str) -> None:
        """Called when a trade fails."""
        ...

    def on_batch_complete(self, success: list[dict], failed: list[dict]) -> None:
        """Called when a batch of trades completes."""
        ...

    def show_trade_plan(
        self,
        target_positions: dict,
        trades: list,
        account_value: float,
        leverage: float,
    ) -> None:
        """Display the trade plan before execution."""
        ...

    def show_execution_summary(
        self,
        successful_trades: list[dict],
        all_trades: list[dict],
        target_positions: dict,
        account_value: float,
    ) -> None:
        """Display execution summary after trades complete."""
        ...


class NoOpCallbacks:
    """No-op implementation for silent operation (e.g., notebooks, tests)."""

    def ask_confirmation(self, message: str) -> bool:  # noqa: D401
        return True  # auto-confirm in silent mode

    def info(self, message: str) -> None:  # noqa: D401
        pass

    def warn(self, message: str) -> None:  # noqa: D401
        pass

    def error(self, message: str) -> None:  # noqa: D401
        pass

    def on_config_override(self, overrides: list[str]) -> None:  # noqa: D401
        pass

    def on_trade_start(self, idx: int, total: int, trade: dict[str, Any]) -> None:
        pass

    def on_trade_fill(
        self, trade: dict[str, Any], fill_data: dict[str, Any], slippage_pct: float
    ) -> None:
        pass

    def on_trade_fail(self, trade: dict[str, Any], reason: str) -> None:
        pass

    def on_batch_complete(self, success: list[dict], failed: list[dict]) -> None:
        pass

    def show_trade_plan(
        self,
        target_positions: dict,
        trades: list,
        account_value: float,
        leverage: float,
    ) -> None:
        pass

    def show_execution_summary(
        self,
        successful_trades: list[dict],
        all_trades: list[dict],
        target_positions: dict,
        account_value: float,
    ) -> None:
        pass


class PrintCallbacks:
    """Lightweight `print`-based callbacks for scripts & notebooks.

    Provides human-readable stdout messages without any Rich dependency.
    Useful when running in Jupyter or small automation scripts where you still
    want basic visibility but not full colour UI.
    """

    def __init__(self, auto_confirm: bool = False) -> None:
        self.auto_confirm = auto_confirm

    def ask_confirmation(self, message: str) -> bool:  # noqa: D401
        if self.auto_confirm:
            print(f"AUTO-CONFIRM: {message}")
            return True
        return input(f"{message} (y/n): ").strip().lower() in {"y", "yes"}

    def info(self, message: str) -> None:  # noqa: D401
        print(f"INFO: {message}")

    def warn(self, message: str) -> None:  # noqa: D401
        print(f"WARNING: {message}")

    def error(self, message: str) -> None:  # noqa: D401
        print(f"ERROR: {message}")

    def on_config_override(self, overrides: list[str]) -> None:  # noqa: D401
        if overrides:
            print(f"Applied CLI overrides: {', '.join(overrides)}")

    def on_trade_start(self, idx: int, total: int, trade: dict[str, Any]) -> None:
        side = "BUY" if trade["is_buy"] else "SELL"
        print(
            f"[{idx}/{total}] {trade['coin']} {side} {trade['sz']:.4f} @ ${trade['price']:,.4f} …",
            end=" ",
        )

    def on_trade_fill(
        self, trade: dict[str, Any], fill_data: dict[str, Any], slippage_pct: float
    ) -> None:
        avg_px = float(fill_data.get("avgPx", trade["price"]))
        print(f"✓ filled @ ${avg_px:,.4f} (slip {slippage_pct:+.3f}%)")

    def on_trade_fail(self, trade: dict[str, Any], reason: str) -> None:
        print(f"✗ failed: {reason}")

    def on_batch_complete(self, success: list[dict], failed: list[dict]) -> None:
        print(f"\nExecution complete › success {len(success)} | failed {len(failed)}")

    def show_trade_plan(
        self,
        target_positions: dict,
        trades: list,
        account_value: float,
        leverage: float,
    ) -> None:
        print("\n=== Portfolio Rebalancing Plan ===")
        print(f"Account value: ${account_value:,.2f}  |  leverage {leverage}x")
        if trades:
            tot = sum(abs(t.get("delta_value", 0)) for t in trades)
            print(f"Planned trades: {len(trades)}  |  volume ${tot:,.2f}")

    def show_execution_summary(
        self,
        successful_trades: list[dict],
        all_trades: list[dict],
        target_positions: dict,
        account_value: float,
    ) -> None:
        succ = len(successful_trades)
        print(f"\nSummary: {succ}/{len(all_trades)} trades succeeded")
