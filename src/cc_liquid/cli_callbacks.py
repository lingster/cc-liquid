"""Rich-based CLI callbacks implementation.

This module depends on Rich and rendering helpers and is intentionally
separated from the core `callbacks` definitions to avoid UI deps in core.
"""

from typing import Any

from rich.console import Console
from rich.prompt import Confirm

from .callbacks import CCLiquidCallbacks
from .cli_display import display_execution_summary, show_rebalancing_plan


class RichCLICallbacks(CCLiquidCallbacks):
    """Rich CLI/Terminal UI implementation of `CCLiquidCallbacks`."""

    def __init__(self):
        self.console = Console()

    def ask_confirmation(self, message: str) -> bool:
        return Confirm.ask(f"\n[bold yellow]{message}[/bold yellow]")

    def info(self, message: str) -> None:
        self.console.print(f"[cyan]{message}[/cyan]")

    def warn(self, message: str) -> None:
        self.console.print(f"[yellow]{message}[/yellow]")

    def error(self, message: str) -> None:
        self.console.print(f"[red]{message}[/red]")

    def on_config_override(self, overrides: list[str]) -> None:
        if overrides:
            self.console.print(
                f"[yellow]Applied CLI overrides: {', '.join(overrides)}[/yellow]"
            )

    def on_trade_start(self, idx: int, total: int, trade: dict[str, Any]) -> None:
        side = "BUY" if trade["is_buy"] else "SELL"
        side_style = "green" if side == "BUY" else "red"
        self.console.print(
            f"  [{idx}/{total}] {trade['coin']} [{side_style}]{side}[/{side_style}] "
            f"{trade['sz']:.4f} @ ${trade['price']:,.4f}...",
            end="",
        )

    def on_trade_fill(
        self, trade: dict[str, Any], fill_data: dict[str, Any], slippage_pct: float
    ) -> None:
        avg_px = float(fill_data.get("avgPx", trade["price"]))
        slippage_style = "green" if slippage_pct <= 0 else "red"
        self.console.print(
            f" [green]✓[/green] Filled @ ${avg_px:,.4f} "
            f"([{slippage_style}]{slippage_pct:+.3f}%[/{slippage_style}])"
        )

    def on_trade_fail(self, trade: dict[str, Any], reason: str) -> None:
        self.console.print(f" [red]✗ {reason}[/red]")

    def on_batch_complete(self, success: list[dict], failed: list[dict]) -> None:
        if not success and not failed:
            return
        total = len(success) + len(failed)
        self.console.print(
            f"\n[bold]Batch Complete:[/bold] "
            f"[green]{len(success)}/{total} succeeded[/green]"
            f"{f' [red]{len(failed)} failed[/red]' if failed else ''}"
        )

    def show_trade_plan(
        self,
        target_positions: dict,
        trades: list,
        account_value: float,
        leverage: float,
    ) -> None:
        show_rebalancing_plan(
            self.console, target_positions, trades, account_value, leverage
        )

    def show_execution_summary(
        self,
        successful_trades: list[dict],
        all_trades: list[dict],
        target_positions: dict,
        account_value: float,
    ) -> None:
        display_execution_summary(
            self.console, successful_trades, all_trades, target_positions, account_value
        )
