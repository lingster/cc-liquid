"""Display utilities for rendering structured data."""

from datetime import UTC, datetime
from typing import TYPE_CHECKING

from rich import box
from rich.box import DOUBLE
from rich.columns import Columns
from rich.console import Console
from rich.layout import Layout
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

if TYPE_CHECKING:
    from .trader import AccountInfo, PortfolioInfo


def create_data_bar(
    value: float,
    max_value: float,
    width: int = 20,
    filled_char: str = "█",
    empty_char: str = "░",
) -> str:
    """Create a visual data bar for representing proportions."""
    if max_value == 0:
        filled_width = 0
    else:
        filled_width = int((value / max_value) * width)
    empty_width = width - filled_width
    return f"{filled_char * filled_width}{empty_char * empty_width}"


def format_currency(value: float, compact: bool = False) -> str:
    """Format currency values with appropriate styling."""
    if compact and abs(value) >= 1000:
        if abs(value) >= 1_000_000:
            return f"${value / 1_000_000:.1f}M"
        return f"${value / 1_000:.1f}K"
    return f"${value:,.2f}"


def create_metric_row(label: str, value: str, style: str = "") -> tuple:
    """Create a metric row tuple for tables."""
    return (f"[cyan]{label}[/cyan]", f"[{style}]{value}[/{style}]" if style else value)


def create_header_panel(base_title: str, is_rebalancing: bool = False) -> Panel:
    """Create a header panel for the dashboard view, optionally showing rebalancing status."""
    header_text = base_title
    if is_rebalancing:
        header_text += " :: [yellow blink]REBALANCING[/yellow blink]"
    return Panel(
        Text(header_text, style="bold cyan", justify="center"), box=DOUBLE, style="cyan"
    )


def create_account_metrics_table(account: "AccountInfo") -> Table:
    """Create a compact account metrics table for the metrics Panel of the dashboard view."""
    table = Table(show_header=False, box=None, padding=(0, 1))
    table.add_column("", width=10)
    table.add_column("", justify="right")

    # Leverage color based on risk
    lev_color = (
        "green"
        if account.current_leverage <= 2
        else "yellow"
        if account.current_leverage <= 3
        else "red"
    )

    rows = [
        create_metric_row(
            "VALUE", format_currency(account.account_value), "bold green"
        ),
        create_metric_row("MARGIN", format_currency(account.margin_used)),
        create_metric_row("FREE", format_currency(account.free_collateral)),
        create_metric_row("LEVERAGE", f"{account.current_leverage:.2f}x", lev_color),
    ]

    for row in rows:
        table.add_row(*row)

    return table


def create_account_exposure_table(portfolio: "PortfolioInfo") -> Table:
    """Create a compact exposure analysis table for the metrics Panel of the dashboard view."""
    table = Table(show_header=False, box=None, padding=(0, 1))
    table.add_column("", width=8)
    table.add_column("", justify="right", width=10)
    table.add_column("", width=18)

    account_val = (
        portfolio.account.account_value if portfolio.account.account_value > 0 else 1
    )

    # Calculate percentages
    long_pct = portfolio.total_long_value / account_val * 100
    short_pct = portfolio.total_short_value / account_val * 100
    net_pct = portfolio.net_exposure / account_val * 100
    gross_pct = long_pct + short_pct  # Just sum the percentages!

    # Build rows with visual bars for long/short
    net_color = "green" if portfolio.net_exposure >= 0 else "red"
    rows = [
        (
            "LONG",
            f"[green]{format_currency(portfolio.total_long_value, compact=True)}[/green]",
            f"[green]{create_data_bar(long_pct, 300, 12, '▓')} {long_pct:.0f}%[/green]",
        ),
        (
            "SHORT",
            f"[red]{format_currency(portfolio.total_short_value, compact=True)}[/red]",
            f"[red]{create_data_bar(short_pct, 300, 12, '▓')} {short_pct:.0f}%[/red]",
        ),
        (
            "NET",
            f"[{net_color}]{format_currency(portfolio.net_exposure, compact=True)}[/{net_color}]",
            f"[dim]{net_pct:+.0f}%[/dim]",
        ),
        (
            "GROSS",
            format_currency(portfolio.total_exposure, compact=True),
            f"[dim]{gross_pct:.0f}%[/dim]",
        ),
    ]

    for row in rows:
        table.add_row(*row)

    return table


def create_metrics_panel(portfolio: "PortfolioInfo") -> Panel:
    """Create portfolio metrics panel (account + exposure) for the dashboard view."""
    return Panel(
        Columns(
            [
                create_account_metrics_table(portfolio.account),
                create_account_exposure_table(portfolio),
            ],
            expand=True,
        ),
        title="[bold cyan]METRICS[/bold cyan]",
        box=box.HEAVY,
    )


def create_positions_panel(portfolio: "PortfolioInfo") -> Panel:
    """Create a panel displaying all open positions with summary statistics.

    The panel includes a table of positions (sorted by value), and a title summarizing
    the number of longs/shorts and total unrealized PnL.

    Args:
        portfolio (PortfolioInfo): The portfolio containing positions and account info.

    Returns:
        Panel: A rich Panel containing the positions table and summary.
    """
    positions = portfolio.positions

    if not portfolio.positions:
        return Panel(
            "[yellow]No open positions[/yellow]",
            box=box.HEAVY,
            title="[bold cyan]POSITIONS[/bold cyan]",
        )

    account_val = (
        portfolio.account.account_value if portfolio.account.account_value > 0 else 1
    )
    long_count = sum(1 for p in positions if p.side == "LONG")
    short_count = sum(1 for p in positions if p.side == "SHORT")
    total_pnl = sum(p.unrealized_pnl for p in positions)
    pnl_pct = total_pnl / account_val * 100
    pnl_color = "green" if total_pnl >= 0 else "red"

    title = (
        f"[bold cyan]POSITIONS[/bold cyan]  [dim]│[/dim]  "
        f"[green]{long_count}L[/green] [red]{short_count}S[/red]  [dim]│[/dim]  "
        f"UNREALIZED [{pnl_color}]${total_pnl:+,.2f} ({pnl_pct:+.1f}%)[/{pnl_color}]"
    )

    table = Table(
        box=box.HEAVY_HEAD,
        show_lines=False,
        header_style="bold cyan on #001926",
        expand=True,
    )

    # Define columns
    table.add_column("COIN", style="cyan", width=8)
    table.add_column("SIDE", justify="center", width=8)
    table.add_column("SIZE", justify="right", width=8)
    table.add_column("ENTRY", justify="right", width=10)
    table.add_column("MARK", justify="right", width=10)
    table.add_column("VALUE", justify="right", width=12)
    table.add_column("PNL", justify="right", width=10)
    table.add_column("PERF", justify="center", width=8)

    # Sort positions by absolute value
    sorted_positions = sorted(positions, key=lambda p: abs(p.value), reverse=True)

    for pos in sorted_positions:
        side_style = "green" if pos.side == "LONG" else "red"
        pnl_color = "green" if pos.unrealized_pnl >= 0 else "red"

        # Format size based on magnitude
        if abs(pos.size) >= 1000:
            size_str = f"{pos.size:,.0f}"
        elif abs(pos.size) >= 1:
            size_str = f"{pos.size:.2f}"
        else:
            size_str = f"{pos.size:.4f}"

        table.add_row(
            f"[bold]{pos.coin}[/bold]",
            f"[{side_style}]{pos.side[:1]}[/{side_style}]",
            size_str,
            format_currency(pos.entry_price, compact=True),
            format_currency(pos.mark_price, compact=True),
            format_currency(abs(pos.value), compact=True),
            f"[{pnl_color}]${pos.unrealized_pnl:+,.2f}[/{pnl_color}]",
            f"[{pnl_color}]{pos.return_pct:+.1f}% [/{pnl_color}]",
        )

    return Panel(table, title=title, box=box.HEAVY)


def create_sidebar_panel(config_dict: dict | None, empty_label: str) -> Panel:
    """Create sidebar panel containing config or a standardized empty state."""
    if config_dict:
        return create_config_panel(config_dict)
    return Panel(empty_label, box=box.HEAVY)


def create_footer_panel(
    next_rebalance_time: datetime | None,
    last_rebalance_time: datetime | None,
    refresh_seconds: float | None,
) -> Panel:
    """Create monitoring footer with countdown and status details."""
    now = datetime.now(UTC)

    # Next rebalance countdown
    countdown_str = ""
    if next_rebalance_time:
        time_until = next_rebalance_time - now
        if time_until.total_seconds() > 0:
            total_seconds = int(time_until.total_seconds())
            hours, remainder = divmod(total_seconds, 3600)
            minutes, seconds = divmod(remainder, 60)

            if hours > 24:
                days = hours // 24
                hours = hours % 24
                countdown = f"{days}d {hours:02d}:{minutes:02d}:{seconds:02d}"
            else:
                countdown = f"{hours:02d}:{minutes:02d}:{seconds:02d}"

            if total_seconds < 60:
                countdown_str = f"[bold yellow blink]{countdown}[/bold yellow blink]"
            elif total_seconds < 3600:
                countdown_str = f"[yellow]{countdown}[/yellow]"
            else:
                countdown_str = f"[green]{countdown}[/green]"
        else:
            countdown_str = "[bold red blink]REBALANCING[/bold red blink]"
    else:
        countdown_str = "[dim]Calculating...[/dim]"

    # Last rebalance string
    last_rebalance_str = "[dim]Never[/dim]"
    if last_rebalance_time:
        time_since = now - last_rebalance_time
        hours_ago = time_since.total_seconds() / 3600
        if hours_ago < 24:
            last_rebalance_str = f"[dim]{hours_ago:.1f}h ago[/dim]"
        else:
            days_ago = hours_ago / 24
            last_rebalance_str = f"[dim]{days_ago:.1f}d ago[/dim]"

    status_grid = Table.grid(expand=True)
    status_grid.add_column(justify="left")
    status_grid.add_column(justify="center")
    status_grid.add_column(justify="center")
    status_grid.add_column(justify="right")

    status_grid.add_row(
        f"[bold cyan]Next rebalance: {countdown_str}[/bold cyan]",
        f"[dim]Last: {last_rebalance_str}[/dim]",
        f"[dim]Monitor refresh: {refresh_seconds:.1f}s[/dim]"
        if refresh_seconds is not None
        else "",
        "[red]Press Ctrl+C to exit[/red]",
    )

    return Panel(status_grid, box=box.HEAVY)


def create_dashboard_layout(
    portfolio: "PortfolioInfo",
    config_dict: dict | None = None,
    *,
    next_rebalance_time: datetime | None = None,
    last_rebalance_time: datetime | None = None,
    is_rebalancing: bool = False,
    refresh_seconds: float | None = None,
) -> Layout:
    """Unified portfolio dashboard builder.

    - Builds the common header, body (metrics, positions, sidebar)
    - Optionally adds a monitoring footer when scheduling data is provided
    """
    has_footer = any(
        value is not None
        for value in (next_rebalance_time, last_rebalance_time, refresh_seconds)
    )

    layout = Layout()

    if has_footer:
        layout.split_column(
            Layout(name="header", size=3),
            Layout(name="body"),
            Layout(name="footer", size=3),
        )
    else:
        layout.split_column(Layout(name="header", size=3), Layout(name="body"))

    # Header
    header_title = (
        "CC-LIQUID MONITOR :: METAMODEL REBALANCER"
        if has_footer
        else "CC-LIQUID :: METAMODEL REBALANCER"
    )
    layout["header"].update(
        create_header_panel(header_title, is_rebalancing if has_footer else False)
    )

    # Body: split into main area and sidebar
    layout["body"].split_row(
        Layout(name="main", ratio=2), Layout(name="sidebar", ratio=1)
    )

    # Main area: metrics + positions
    layout["main"].split_column(
        Layout(name="metrics", size=8), Layout(name="positions")
    )

    layout["metrics"].update(create_metrics_panel(portfolio))
    layout["positions"].update(create_positions_panel(portfolio))

    # Sidebar
    empty_sidebar_text = (
        "[dim]No config loaded[/dim]" if has_footer else "[dim]No config[/dim]"
    )
    layout["sidebar"].update(create_sidebar_panel(config_dict, empty_sidebar_text))

    # Footer (optional)
    if has_footer:
        footer = create_footer_panel(
            next_rebalance_time, last_rebalance_time, refresh_seconds
        )
        layout["footer"].update(footer)

    return layout


def create_config_tree_table(config_dict: dict) -> Table:
    """Create config display as a tree structure (reusable)."""
    table = Table(show_header=False, box=None, padding=(0, 0))
    table.add_column("Setting", style="cyan", width=20, no_wrap=True)
    table.add_column("Value", style="white")

    # Environment section
    network = "TESTNET" if config_dict.get("is_testnet", False) else "MAINNET"
    network_color = "yellow" if config_dict.get("is_testnet", False) else "green"

    # Data source section
    data_config = config_dict.get("data", {})
    source = data_config.get("source", "crowdcent")
    source_color = (
        "green"
        if source == "crowdcent"
        else "yellow"
        if source == "numerai"
        else "white"
    )

    # Portfolio section
    portfolio_config = config_dict.get("portfolio", {})
    leverage = portfolio_config.get("target_leverage", 1.0)
    leverage_color = "green" if leverage <= 2 else "yellow" if leverage <= 3 else "red"
    rebalancing = portfolio_config.get("rebalancing", {})

    # Execution section
    execution_config = config_dict.get("execution", {})
    slippage_pct = execution_config.get("slippage_tolerance", 0.005) * 100
    slippage_color = (
        "green" if slippage_pct <= 0.5 else "yellow" if slippage_pct <= 1.0 else "red"
    )
    min_trade_value = execution_config.get("min_trade_value", 10.0)

    # Profile section (owner/vault and signer env name)
    profile_cfg = config_dict.get("profile", {})
    owner = profile_cfg.get("owner") or "[dim]-[/dim]"
    vault = profile_cfg.get("vault") or "[dim]-[/dim]"
    active_profile = profile_cfg.get("active") or "[dim]-[/dim]"
    signer_env = profile_cfg.get("signer_env") or "HYPERLIQUID_PRIVATE_KEY"

    # Build all rows
    rows = [
        ("[bold]ENVIRONMENT[/bold]", ""),
        ("├─ Network", f"[{network_color}]{network}[/{network_color}]"),
        ("├─ Active Profile", f"[white]{active_profile}[/white]"),
        ("├─ Owner", f"[white]{owner}[/white]"),
        ("├─ Vault", f"[white]{vault}[/white]"),
        ("└─ Signer Env", f"[white]{signer_env}[/white]"),
        ("", ""),
        ("[bold]DATA SOURCE[/bold]", ""),
        ("├─ Provider", f"[{source_color}]{source}[/{source_color}]"),
        ("├─ Path", data_config.get("path", "predictions.parquet")),
        ("└─ Prediction", data_config.get("prediction_column", "pred_10d")),
        ("", ""),
        ("[bold]PORTFOLIO[/bold]", ""),
        ("├─ Long Positions", f"[green]{portfolio_config.get('num_long', 10)}[/green]"),
        ("├─ Short Positions", f"[red]{portfolio_config.get('num_short', 10)}[/red]"),
        ("├─ Target Leverage", f"[{leverage_color}]{leverage:.1f}x[/{leverage_color}]"),
        ("└─ Rebalancing", ""),
        ("   ├─ Frequency", f"Every {rebalancing.get('every_n_days', 10)} days"),
        ("   └─ Time (UTC)", rebalancing.get("at_time", "18:15")),
        ("", ""),
        ("[bold]EXECUTION[/bold]", ""),
        ("├─ Slippage", f"[{slippage_color}]{slippage_pct:.1f}%[/{slippage_color}]"),
        ("└─ Min Trade Value", format_currency(min_trade_value, compact=False)),
    ]

    for row in rows:
        table.add_row(*row)

    return table


def display_portfolio(
    portfolio: "PortfolioInfo",
    console: Console | None = None,
    config_dict: dict | None = None,
) -> None:
    """Display portfolio information in a compact dashboard."""
    if console is None:
        console = Console()

    # Use the new dashboard layout
    layout = create_dashboard_layout(portfolio, config_dict)
    console.print(layout)


def create_config_panel(config_dict: dict) -> Panel:
    """Create display Panel for the config view. Can be used standalone or as part of the dashboard view."""
    return Panel(
        create_config_tree_table(config_dict),
        title="[bold cyan]CONFIG[/bold cyan]",
        box=box.HEAVY,
    )


def display_file_summary(
    console: Console, predictions, output_path: str, model_name: str
) -> None:
    """Display a summary of downloaded predictions file."""
    console.print(f"[green]✓[/green] Downloaded {model_name} to {output_path}")
    console.print(f"[cyan]Shape:[/cyan] {predictions.shape}")
    console.print(f"[cyan]Columns:[/cyan] {list(predictions.columns)}")


def show_pre_alpha_warning() -> None:
    """Display pre-alpha warning to users."""
    console = Console()
    warning_copy = """
This is pre-alpha software provided as a reference implementation only.
• Using this software may result in COMPLETE LOSS of funds.
• CrowdCent makes NO WARRANTIES and assumes NO LIABILITY for any losses.
• Users must comply with all Hyperliquid and CrowdCent terms of service.
• We do NOT endorse any vaults or strategies using this tool.

[bold yellow]By continuing, you acknowledge that you understand and accept ALL risks.[/bold yellow]
    """
    warning_text = Text.from_markup(warning_copy, justify="left")
    panel = Panel(
        warning_text,
        title="[bold cyan]CC-LIQUID ::[/bold cyan] [bold red] PRE-ALPHA SOFTWARE - USE AT YOUR OWN RISK [/bold red]",
        border_style="red",
        box=box.HEAVY,
    )
    console.print(panel)


def show_rebalancing_plan(
    console: Console,
    target_positions: dict,
    trades: list,
    account_value: float,
    leverage: float,
) -> None:
    """Create a comprehensive rebalancing dashboard layout."""
    # Header
    header = Panel(
        Text("REBALANCING PLAN", style="bold cyan", justify="center"),
        box=DOUBLE,
        style="cyan",
    )
    console.print(header)

    # Metrics row: account + rebalancing summary
    metrics_content = create_rebalancing_metrics_panel(
        account_value, leverage, trades, target_positions
    )
    console.print(metrics_content)

    # Trades panel
    trades_panel = create_trades_panel(trades)
    console.print(trades_panel)

    # Check if we have skipped trades
    skipped_count = sum(1 for t in trades if t.get("skipped", False))
    if skipped_count > 0:
        console.print(
            f"\n[bold yellow]⚠️ WARNING: {skipped_count} trade(s) marked as SKIPPED[/bold yellow]\n"
            f"[yellow]These positions cannot be resized due to minimum trade size constraints.[/yellow]\n"
            f"[yellow]They will remain at their current sizes, causing portfolio imbalance.[/yellow]\n"
            f"[dim]Consider: increasing account value, using higher leverage, or reducing position count.[/dim]"
        )


def create_rebalancing_metrics_panel(
    account_value: float, leverage: float, trades: list, target_positions: dict
) -> Panel:
    """Create rebalancing metrics panel."""
    # Position counts using the type field
    executable_trades = [t for t in trades if not t.get("skipped", False)]
    opens = sum(1 for t in executable_trades if t.get("type") == "open")
    closes = sum(1 for t in executable_trades if t.get("type") == "close")
    flips = sum(1 for t in executable_trades if t.get("type") == "flip")
    reduces = sum(1 for t in executable_trades if t.get("type") == "reduce")
    increases = sum(1 for t in executable_trades if t.get("type") == "increase")

    # Target portfolio metrics
    total_long_value = sum(v for v in target_positions.values() if v > 0)
    total_short_value = abs(sum(v for v in target_positions.values() if v < 0))

    # Create two columns
    left_table = Table(show_header=False, box=None, padding=(0, 1))
    left_table.add_column("", width=12)
    left_table.add_column("", justify="right")

    left_table.add_row("ACCOUNT", format_currency(account_value, compact=False))
    left_table.add_row(
        "LEVERAGE",
        f"[{'green' if leverage <= 2 else 'yellow' if leverage <= 3 else 'red'}]{leverage:.1f}x[/{'green' if leverage <= 2 else 'yellow' if leverage <= 3 else 'red'}]",
    )
    left_table.add_row("MAX EXPOSURE", format_currency(account_value * leverage))
    left_table.add_row("", "")  # spacer
    left_table.add_row(
        "TARGET LONG",
        f"[green]{format_currency(total_long_value, compact=True)}[/green]",
    )
    left_table.add_row(
        "TARGET SHORT", f"[red]{format_currency(total_short_value, compact=True)}[/red]"
    )

    right_table = Table(show_header=False, box=None, padding=(0, 1))
    right_table.add_column("", width=12)
    right_table.add_column("", justify="right")

    right_table.add_row("TRADES", f"[bold]{len(executable_trades)}[/bold]")
    if opens > 0:
        right_table.add_row("OPEN", f"[green]{opens}[/green]")
    if closes > 0:
        right_table.add_row("CLOSE", f"[red]{closes}[/red]")
    if flips > 0:
        right_table.add_row("FLIP", f"[yellow]{flips}[/yellow]")
    if reduces > 0:
        right_table.add_row("REDUCE", f"[blue]{reduces}[/blue]")
    if increases > 0:
        right_table.add_row("ADD", f"[cyan]{increases}[/cyan]")

    return Panel(
        Columns([left_table, right_table], expand=True),
        title="[bold cyan]METRICS[/bold cyan]",
        box=box.HEAVY,
    )


def create_trades_panel(trades: list) -> Panel:
    """Create a panel for the trades table matching the positions table style."""
    # Handle no trades
    if not trades:
        return Panel(
            "[yellow]No trades required - portfolio is already balanced[/yellow]",
            box=box.HEAVY,
            title="[bold cyan]TRADES[/bold cyan]",
        )

    # Separate executable and skipped trades
    executable_trades = [t for t in trades if not t.get("skipped", False)]
    skipped_trades = [t for t in trades if t.get("skipped", False)]

    # Calculate summary for title (only executable trades)
    total_volume = sum(abs(t.get("delta_value", 0)) for t in executable_trades)
    buy_count = sum(1 for t in executable_trades if t.get("is_buy"))
    sell_count = len(executable_trades) - buy_count
    buy_volume = sum(
        abs(t.get("delta_value", 0)) for t in executable_trades if t.get("is_buy")
    )
    sell_volume = sum(
        abs(t.get("delta_value", 0)) for t in executable_trades if not t.get("is_buy")
    )

    # Add skipped count to title if any
    skipped_info = ""
    if skipped_trades:
        skipped_info = f"  [dim]│[/dim]  [yellow]{len(skipped_trades)} SKIPPED[/yellow]"

    title = (
        f"[bold cyan]TRADES[/bold cyan]  [dim]│[/dim]  "
        f"[green]{buy_count} BUY (${buy_volume:,.2f})[/green] "
        f"[red]{sell_count} SELL (${sell_volume:,.2f})[/red]  [dim]│[/dim]  "
        f"VOLUME [bold]${total_volume:,.2f}[/bold]"
        f"{skipped_info}"
    )

    table = Table(
        box=box.HEAVY_HEAD,
        show_lines=False,
        header_style="bold cyan on #001926",
        expand=True,
    )

    # Define columns
    table.add_column("COIN", style="cyan", width=8)
    table.add_column("ACTION", justify="center", width=7)
    table.add_column("CURRENT", justify="right", width=10)
    table.add_column("→", justify="center", width=1, style="dim")
    table.add_column("TARGET", justify="right", width=10)
    table.add_column("DELTA", justify="right", width=10)
    table.add_column("TRADE", justify="center", width=6)
    table.add_column("SIZE", justify="right", width=10)
    table.add_column("PRICE", justify="right", width=10)

    # Sort trades by absolute delta value, with executable trades first
    sorted_trades = sorted(
        trades, key=lambda t: (t.get("skipped", False), -abs(t.get("delta_value", 0)))
    )

    for trade in sorted_trades:
        coin = trade["coin"]
        is_buy = trade.get("is_buy", False)  # Skipped trades may not have this
        current_value = trade.get("current_value", 0)
        target_value = trade.get("target_value", 0)
        delta_value = trade.get("delta_value", 0)

        # Use the type field from trade calculation
        trade_type = trade.get("type", "increase")  # fallback for old data
        action_styles = {
            "open": "[green]OPEN[/green]",
            "close": "[red]CLOSE[/red]",
            "flip": "[yellow]FLIP[/yellow]",
            "reduce": "[blue]REDUCE[/blue]",
            "increase": "[cyan]ADD[/cyan]",
        }
        action = action_styles.get(trade_type, "[dim]ADJUST[/dim]")

        # Format current and target with side indicators
        if current_value == 0:
            current_str = "[dim]-[/dim]"
        else:
            side = "L" if current_value > 0 else "S"
            side_color = "green" if current_value > 0 else "red"
            current_str = f"{format_currency(abs(current_value), compact=True)} [{side_color}]{side}[/{side_color}]"

        if target_value == 0:
            target_str = "[dim]-[/dim]"
        else:
            side = "L" if target_value > 0 else "S"
            side_color = "green" if target_value > 0 else "red"
            target_str = f"{format_currency(abs(target_value), compact=True)} [{side_color}]{side}[/{side_color}]"

        # Trade direction
        trade_action = "[green]BUY[/green]" if is_buy else "[red]SELL[/red]"

        # Delta with color
        delta_color = "green" if delta_value > 0 else "red"
        delta_str = f"[{delta_color}]{delta_value:+,.0f}[/{delta_color}]"

        # Style differently if trade is skipped
        if trade.get("skipped", False):
            # Show the skip reason in the trade column
            trade_action = "[yellow]SKIP[/yellow]"
            # Dim the entire row for skipped trades
            coin_str = f"[dim]{coin}[/dim]"
            action = f"[dim]{action}[/dim]"
            current_str = f"[dim]{current_str}[/dim]"
            target_str = f"[dim]{target_str}[/dim]"
            delta_str = f"[dim yellow]{delta_value:+,.0f}[/dim yellow]"
            size_str = "[dim]-[/dim]"  # No size since it won't execute
            price_str = "[dim]-[/dim]"  # No price since it won't execute
        else:
            coin_str = f"[bold]{coin}[/bold]"
            size_str = f"{trade.get('sz', 0):.4f}" if "sz" in trade else "[dim]-[/dim]"
            price_str = (
                format_currency(trade["price"], compact=True)
                if "price" in trade
                else "[dim]-[/dim]"
            )

        table.add_row(
            coin_str,
            action,
            current_str,
            "→",
            target_str,
            delta_str,
            trade_action,
            size_str,
            price_str,
        )

    return Panel(table, title=title, box=box.HEAVY, expand=True)


def create_execution_metrics_panel(
    successful_trades: list[dict],
    all_trades: list[dict],
    target_positions: dict,
    account_value: float,
) -> Panel:
    """Create execution summary metrics panel."""
    total_success = len(successful_trades)
    total_failed = len(all_trades) - total_success

    # Calculate portfolio metrics
    total_long_value = sum(v for v in target_positions.values() if v > 0)
    total_short_value = abs(sum(v for v in target_positions.values() if v < 0))
    total_exposure = total_long_value + total_short_value
    net_exposure = total_long_value - total_short_value
    leverage = total_exposure / account_value if account_value > 0 else 0

    # Calculate slippage stats from successful trades
    if successful_trades:
        slippages = [t.get("slippage_pct", 0) for t in successful_trades]
        avg_slippage = sum(slippages) / len(slippages)
        max_slippage = max(slippages)
        min_slippage = min(slippages)
    else:
        avg_slippage = max_slippage = min_slippage = 0

    # Create two columns
    left_table = Table(show_header=False, box=None, padding=(0, 1))
    left_table.add_column("", width=15)
    left_table.add_column("", justify="right")

    # Execution results
    left_table.add_row(
        "EXECUTED", f"[green]{total_success}[/green]" if total_success > 0 else "0"
    )
    if total_failed > 0:
        left_table.add_row("FAILED", f"[red]{total_failed}[/red]")
    left_table.add_row(
        "SUCCESS RATE",
        f"[bold]{total_success / len(all_trades) * 100:.1f}%[/bold]"
        if all_trades
        else "N/A",
    )
    left_table.add_row("", "")  # spacer

    # Slippage stats
    if successful_trades:
        left_table.add_row(
            "AVG SLIPPAGE",
            f"[{'green' if avg_slippage <= 0 else 'red'}]{avg_slippage:+.3f}%[/{'green' if avg_slippage <= 0 else 'red'}]",
        )
        left_table.add_row("MAX SLIPPAGE", f"{max_slippage:+.3f}%")
        left_table.add_row("MIN SLIPPAGE", f"{min_slippage:+.3f}%")

    right_table = Table(show_header=False, box=None, padding=(0, 1))
    right_table.add_column("", width=15)
    right_table.add_column("", justify="right")

    # Portfolio metrics
    right_table.add_row("TOTAL EXPOSURE", format_currency(total_exposure))
    right_table.add_row("NET EXPOSURE", format_currency(net_exposure))
    right_table.add_row(
        "LEVERAGE",
        f"[{'green' if leverage <= 2 else 'yellow' if leverage <= 3 else 'red'}]{leverage:.2f}x[/{'green' if leverage <= 2 else 'yellow' if leverage <= 3 else 'red'}]",
    )
    right_table.add_row("", "")  # spacer
    right_table.add_row(
        "LONG VALUE",
        f"[green]{format_currency(total_long_value, compact=True)}[/green]",
    )
    right_table.add_row(
        "SHORT VALUE", f"[red]{format_currency(total_short_value, compact=True)}[/red]"
    )
    right_table.add_row(
        "NET % OF NAV",
        f"{net_exposure / account_value * 100:+.1f}%" if account_value > 0 else "N/A",
    )

    return Panel(
        Columns([left_table, right_table], expand=True),
        title="[bold cyan]METRICS[/bold cyan]",
        box=box.HEAVY,
    )


def create_execution_details_panel(
    successful_trades: list[dict], all_trades: list[dict]
) -> Panel:
    """Create execution details table."""
    # Create a set of successful trade identifiers (coin + is_buy combination)
    successful_ids = {(t["coin"], t["is_buy"]) for t in successful_trades}
    failed_trades = [
        t for t in all_trades if (t["coin"], t["is_buy"]) not in successful_ids
    ]

    # Calculate volumes for successful trades
    success_volume = sum(
        float(t.get("fill_data", {}).get("totalSz", 0))
        * float(t.get("fill_data", {}).get("avgPx", 0))
        for t in successful_trades
        if "fill_data" in t
    )

    title = (
        f"[bold cyan]EXECUTION DETAILS[/bold cyan]  [dim]│[/dim]  "
        f"[green]{len(successful_trades)} SUCCESS (${success_volume:,.2f})[/green] "
        f"[red]{len(failed_trades)} FAILED[/red]"
    )

    table = Table(
        box=box.HEAVY_HEAD,
        show_lines=False,
        header_style="bold cyan on #001926",
        title_style="bold cyan",
    )

    # Define columns
    table.add_column("COIN", style="cyan", width=8)
    table.add_column("SIDE", justify="center", width=6)
    table.add_column("SIZE", justify="right", width=10)
    table.add_column("EXPECTED", justify="right", width=10)
    table.add_column("FILLED", justify="right", width=10)
    table.add_column("SLIPPAGE", justify="right", width=10)
    table.add_column("VALUE", justify="right", width=12)
    table.add_column("STATUS", justify="center", width=8)

    # Add successful trades first
    for trade in successful_trades:
        if "fill_data" in trade:
            fill = trade["fill_data"]
            side = "BUY" if trade["is_buy"] else "SELL"
            side_style = "green" if side == "BUY" else "red"
            slippage_style = "green" if trade.get("slippage_pct", 0) <= 0 else "red"

            table.add_row(
                f"[bold]{trade['coin']}[/bold]",
                f"[{side_style}]{side}[/{side_style}]",
                f"{float(fill['totalSz']):.4f}",
                format_currency(trade["price"], compact=True),
                format_currency(float(fill["avgPx"]), compact=True),
                f"[{slippage_style}]{trade.get('slippage_pct', 0):+.3f}%[/{slippage_style}]",
                format_currency(float(fill["totalSz"]) * float(fill["avgPx"])),
                "[green]✓[/green]",
            )

    # Add failed trades
    for trade in failed_trades:
        side = "BUY" if trade["is_buy"] else "SELL"
        side_style = "green" if side == "BUY" else "red"

        table.add_row(
            f"[bold]{trade['coin']}[/bold]",
            f"[{side_style}]{side}[/{side_style}]",
            f"{trade['sz']:.4f}",
            format_currency(trade["price"], compact=True),
            "[red]-[/red]",
            "[red]-[/red]",
            "[red]-[/red]",
            "[red]✗[/red]",
        )

    panel = Panel(table, title=title, box=box.HEAVY)
    return panel


def display_execution_summary(
    console: Console,
    successful_trades: list[dict],
    all_trades: list[dict],
    target_positions: dict,
    account_value: float,
) -> None:
    """Display execution summary after trades complete.

    Prints panels sequentially (header → summary metrics → details),
    matching the style of the trade plan output.
    """
    # Header
    header = Panel(
        Text("EXECUTION SUMMARY", style="bold cyan", justify="center"),
        box=DOUBLE,
        style="cyan",
    )
    console.print("\n")
    console.print(header)

    # Summary metrics
    summary_panel = create_execution_metrics_panel(
        successful_trades, all_trades, target_positions, account_value
    )
    console.print(summary_panel)

    # Details (only when there were any trades or failures)
    if successful_trades or (len(all_trades) > len(successful_trades)):
        details_panel = create_execution_details_panel(successful_trades, all_trades)
        console.print(details_panel)
    else:
        console.print(Panel("[dim]No trades executed[/dim]", box=box.HEAVY))
