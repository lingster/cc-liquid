"""Command-line interface for cc-liquid."""

import os
import yaml
import time
import traceback
from datetime import datetime, timezone
import subprocess
import shutil
import shlex

import click
from rich.console import Console
from rich.progress import (
    Progress,
    BarColumn,
    TextColumn,
    TimeRemainingColumn,
    TimeElapsedColumn,
    MofNCompleteColumn,
)
from typing import Any

from .cli_callbacks import RichCLICallbacks
from .cli_display import (
    create_dashboard_layout,
    create_config_panel,
    create_header_panel,
    create_setup_welcome_panel,
    create_setup_summary_panel,
    display_portfolio,
    display_file_summary,
    display_backtest_summary,
    show_pre_alpha_warning,
    display_optimization_results,
    display_optimization_contours,
)
from .backtester import BacktestOptimizer, BacktestConfig, Backtester
from .config import config
from .config import apply_cli_overrides
from .data_loader import DataLoader
from .trader import CCLiquid
from .completion import detect_shell_from_env, install_completion


TMUX_SESSION_NAME = "cc-liquid"
TMUX_WINDOW_NAME = "cc-liquid"


@click.group()
def cli():
    """cc-liquid - A metamodel-based rebalancer for Hyperliquid."""
    # Suppress the pre-alpha banner during Click's completion mode to avoid
    # corrupting the generated completion script output.
    in_completion_mode = any(k.endswith("_COMPLETE") for k in os.environ)
    if not in_completion_mode:
        show_pre_alpha_warning()


@cli.command(name="init")
@click.option(
    "--non-interactive", is_flag=True, help="Skip interactive setup, use defaults"
)
def init_cmd(non_interactive: bool):
    """Interactive setup wizard for first-time users.

    Guides you through creating config files with validation and helpful defaults.
    """
    console = Console()
    from rich.prompt import Prompt, Confirm
    from rich.panel import Panel
    from rich.text import Text

    # Check existing files
    cfg_path = "cc-liquid-config.yaml"
    env_path = ".env"

    if os.path.exists(cfg_path) or os.path.exists(env_path):
        existing_files = []
        if os.path.exists(cfg_path):
            existing_files.append(cfg_path)
        if os.path.exists(env_path):
            existing_files.append(env_path)

        console.print(
            f"\n[yellow]⚠️  Found existing files: {', '.join(existing_files)}[/yellow]"
        )
        if not non_interactive:
            if not Confirm.ask("Overwrite existing files?", default=False):
                console.print("[red]Setup cancelled.[/red]")
                return

    # Gather all configuration based on mode
    if non_interactive:
        # All defaults in one place for non-interactive mode
        is_testnet = True
        data_source = "crowdcent"
        crowdcent_key = ""
        hyper_key_placeholder = "0x..."
        owner_address = None
        vault_address = None
        num_long = 10
        num_short = 10
        leverage = 1.0
    else:
        # Interactive flow
        console.print(create_setup_welcome_panel())

        # Step 1: Environment
        console.print("\n[bold]Step 1: Choose Environment[/bold]")
        console.print("[dim]Testnet is recommended for first-time users[/dim]")
        is_testnet = Confirm.ask("Use testnet?", default=True)

        # Step 2: Data source
        console.print("\n[bold]Step 2: Data Source[/bold]")
        console.print("Available sources:")
        console.print(
            "  • [cyan]crowdcent[/cyan] - CrowdCent metamodel (requires API key)"
        )
        console.print("  • [cyan]numerai[/cyan] - Numerai crypto signals (free)")
        console.print("  • [cyan]local[/cyan] - Your own prediction file")

        data_source = Prompt.ask(
            "Choose data source",
            choices=["crowdcent", "numerai", "local"],
            default="crowdcent",
        )

        # Step 3: API keys
        console.print("\n[bold]Step 3: API Keys[/bold]")

        crowdcent_key = ""
        if data_source == "crowdcent":
            console.print("\n[cyan]CrowdCent API Key[/cyan]")
            console.print("[dim]Get from: https://crowdcent.com/profile[/dim]")
            crowdcent_key = Prompt.ask(
                "Enter CrowdCent API key (or press Enter to add later)",
                default="",
                show_default=False,
            )

        console.print("\n[cyan]Hyperliquid Private Key[/cyan]")
        console.print("[dim]Get from: https://app.hyperliquid.xyz/API[/dim]")
        console.print(
            "[yellow]⚠️  Use an agent wallet key, not your main wallet![/yellow]"
        )
        hyper_key_input = Prompt.ask(
            "Enter Hyperliquid private key (or press Enter to add later)",
            default="",
            show_default=False,
            password=True,  # Hide input for security
        )
        hyper_key_placeholder = hyper_key_input if hyper_key_input else "0x..."

        # Step 4: Addresses
        console.print("\n[bold]Step 4: Addresses[/bold]")
        console.print("[dim]Leave blank to fill in later[/dim]")

        owner_address = Prompt.ask(
            "Owner address (your main wallet, NOT the agent wallet)",
            default="",
            show_default=False,
        )
        owner_address = owner_address if owner_address else None

        vault_address = Prompt.ask(
            "Vault address (optional, for managed vaults)",
            default="",
            show_default=False,
        )
        vault_address = vault_address if vault_address else None

        # Step 5: Portfolio settings
        console.print("\n[bold]Step 5: Portfolio Settings[/bold]")

        num_long = int(Prompt.ask("Number of long positions", default="10"))
        num_short = int(Prompt.ask("Number of short positions", default="10"))

        console.print("\n[yellow]⚠️  Leverage Warning:[/yellow]")
        console.print("[dim]1.0 = no leverage (safest)[/dim]")
        console.print("[dim]2.0 = 2x leverage (moderate risk)[/dim]")
        console.print("[dim]3.0+ = high risk of liquidation[/dim]")
        leverage = float(Prompt.ask("Target leverage", default="1.0"))

    # Compose configurations
    yaml_cfg: dict[str, Any] = {
        "active_profile": "default",
        "profiles": {
            "default": {
                "owner": owner_address,
                "vault": vault_address,
                "signer_env": "HYPERLIQUID_PRIVATE_KEY",
            }
        },
        "is_testnet": is_testnet,
        "data": {
            "source": data_source,
            "path": "predictions.parquet",
            **(
                {
                    "date_column": "date",
                    "asset_id_column": "symbol",
                    "prediction_column": "meta_model",
                }
                if data_source == "numerai"
                else {
                    "date_column": "release_date",
                    "asset_id_column": "id",
                    "prediction_column": "pred_10d",
                }
            ),
        },
        "portfolio": {
            "num_long": num_long,
            "num_short": num_short,
            "target_leverage": leverage,
            "rebalancing": {"every_n_days": 10, "at_time": "18:15"},
        },
        "execution": {"slippage_tolerance": 0.005, "min_trade_value": 10.0},
    }

    env_lines = [
        "# Secrets only - NEVER commit this file to git!",
        "# Add to .gitignore immediately",
        "",
        "# CrowdCent API (https://crowdcent.com/profile)",
        f"CROWDCENT_API_KEY={crowdcent_key}",
        "",
        "# Hyperliquid Agent Wallet Private Key (https://app.hyperliquid.xyz/API)",
        "# ⚠️  Use an agent wallet, NOT your main wallet!",
        f"HYPERLIQUID_PRIVATE_KEY={hyper_key_placeholder}",
    ]

    # Write files
    try:
        with open(cfg_path, "w") as f:
            yaml.safe_dump(yaml_cfg, f, sort_keys=False)
        console.print(f"\n[green]✓[/green] Created [cyan]{cfg_path}[/cyan]")
    except Exception as e:
        console.print(f"[red]✗ Failed to write {cfg_path}:[/red] {e}")
        raise SystemExit(1)

    try:
        with open(env_path, "w") as f:
            f.write("\n".join(env_lines) + "\n")
        console.print(f"[green]✓[/green] Created [cyan]{env_path}[/cyan]")
    except Exception as e:
        console.print(f"[red]✗ Failed to write {env_path}:[/red] {e}")
        raise SystemExit(1)

    # Add to .gitignore if it exists
    if os.path.exists(".gitignore"):
        with open(".gitignore", "r") as f:
            gitignore_content = f.read()
        if ".env" not in gitignore_content:
            with open(".gitignore", "a") as f:
                f.write("\n# cc-liquid secrets\n.env\n")
            console.print("[green]✓[/green] Added .env to .gitignore")

    # Summary and next steps
    summary = create_setup_summary_panel(
        is_testnet=is_testnet,
        data_source=data_source,
        num_long=num_long,
        num_short=num_short,
        leverage=leverage,
    )
    console.print("\n")
    console.print(summary)


@cli.command(name="config")
def show_config():
    """Show the current configuration."""
    console = Console()
    config_dict = config.to_dict()
    panel = create_config_panel(config_dict)
    console.print(panel)


@cli.group()
def completion():
    """Shell completion utilities."""


@completion.command(name="install")
@click.option(
    "--shell",
    "shell_opt",
    type=click.Choice(["bash", "zsh", "fish"], case_sensitive=False),
    default=None,
    help="Target shell. Defaults to auto-detect from $SHELL.",
)
@click.option(
    "--prog-name",
    default="cc-liquid",
    show_default=True,
    help="Program name to install completion for (as installed on PATH).",
)
def completion_install(shell_opt: str | None, prog_name: str):
    """Install shell completion for the current user.

    Writes the generated completion script to a standard location and, for
    Bash/Zsh, appends a source line to the user's rc file idempotently.
    """
    console = Console()
    shell = shell_opt or detect_shell_from_env()
    if shell is None:
        console.print(
            "[red]Could not detect shell from $SHELL. Specify with[/red] "
            "[bold]--shell {bash|zsh|fish}[/bold]."
        )
        raise SystemExit(2)

    result = install_completion(prog_name, shell)

    console.print(
        f"[green]✓[/green] Installed completion for [bold]{shell}[/bold] at "
        f"[cyan]{result.script_path}[/cyan]"
        + (" (updated)" if result.script_written else " (no changes)")
    )

    if result.rc_path is not None:
        console.print(
            f"[blue]•[/blue] Ensured rc entry in [cyan]{result.rc_path}[/cyan] "
            + ("(added)" if result.rc_line_added else "(already present)")
        )

    console.print(
        "[dim]Restart your shell or 'source' your rc file to activate completion.[/dim]"
    )


@cli.group()
def profile():
    """Manage configuration profiles (owner/vault/signer)."""


@profile.command(name="list")
def profile_list():
    """List available profiles from YAML and highlight the active one."""
    console = Console()
    profiles = config.profiles or {}
    if not profiles:
        console.print("[yellow]No profiles found in cc-liquid-config.yaml[/yellow]")
        return
    from rich.table import Table

    table = Table(title="Profiles", show_lines=False, header_style="bold cyan")
    table.add_column("NAME", style="cyan")
    table.add_column("OWNER")
    table.add_column("VAULT")
    table.add_column("SIGNER ENV")
    for name, prof in profiles.items():
        owner = (prof or {}).get("owner") or "-"
        vault = (prof or {}).get("vault") or "-"
        signer_env = (prof or {}).get("signer_env", "HYPERLIQUID_PRIVATE_KEY")
        label = f"[bold]{name}[/bold]" + (
            " [green](active)[/green]" if name == config.active_profile else ""
        )
        table.add_row(label, owner, vault, signer_env)
    console.print(table)


@profile.command(name="show")
@click.argument("name", required=False)
def profile_show(name: str | None):
    """Show details for a profile (defaults to active)."""
    console = Console()
    target = name or config.active_profile
    if not target:
        console.print("[red]No active profile set and no name provided[/red]")
        raise SystemExit(2)
    prof = (config.profiles or {}).get(target)
    if prof is None:
        console.print(f"[red]Profile '{target}' not found[/red]")
        raise SystemExit(2)
    data = {
        "name": target,
        "owner": prof.get("owner"),
        "vault": prof.get("vault"),
        "signer_env": prof.get("signer_env", "HYPERLIQUID_PRIVATE_KEY"),
        "is_active": target == config.active_profile,
    }
    panel = create_config_panel(
        {
            "is_testnet": config.is_testnet,
            "profile": {
                "active": data["name"] if data["is_active"] else config.active_profile,
                "owner": data["owner"],
                "vault": data["vault"],
                "signer_env": data["signer_env"],
            },
            "data": config.data.__dict__,
            "portfolio": config.portfolio.__dict__
            | {"rebalancing": config.portfolio.rebalancing.__dict__},
            "execution": config.execution.__dict__,
        }
    )
    console.print(panel)


@profile.command(name="use")
@click.argument("name", required=True)
def profile_use(name: str):
    """Set active profile and persist to YAML."""
    console = Console()
    profiles = config.profiles or {}
    if name not in profiles:
        console.print(f"[red]Profile '{name}' not found in cc-liquid-config.yaml[/red]")
        raise SystemExit(2)

    # Update file
    cfg_path = "cc-liquid-config.yaml"
    try:
        y: dict = {}
        if os.path.exists(cfg_path):
            with open(cfg_path) as f:
                y = yaml.safe_load(f) or {}
        y["active_profile"] = name
        with open(cfg_path, "w") as f:
            yaml.safe_dump(y, f, sort_keys=False)
    except Exception as e:
        console.print(f"[red]Failed to update {cfg_path}: {e}[/red]")
        raise SystemExit(1)

    # Update runtime
    config.active_profile = name
    try:
        config.refresh_runtime()
    except Exception as e:
        console.print(f"[red]Failed to activate profile: {e}[/red]")
        raise SystemExit(1)
    console.print(f"[green]✓[/green] Active profile set to [bold]{name}[/bold]")


@cli.command()
def account():
    """Show comprehensive account and positions summary."""
    console = Console()
    trader = CCLiquid(config, callbacks=RichCLICallbacks())

    # Get structured portfolio info
    portfolio = trader.get_portfolio_info()

    # Display using reusable display function with config
    display_portfolio(portfolio, console, config.to_dict())


@cli.command()
@click.option(
    "--output",
    "-o",
    default=None,
    help="Output file path (defaults to path in config).",
)
def download_crowdcent(output):
    """Download the CrowdCent meta model."""
    console = Console()
    if output is None:
        output = config.data.path
    try:
        predictions = DataLoader.from_crowdcent_api(
            api_key=config.CROWDCENT_API_KEY, download_path=output
        )
        display_file_summary(console, predictions, output, "CrowdCent meta model")
    except Exception as e:
        console.print(f"[red]✗[/red] Failed to download CrowdCent meta model: {e}")
        raise


@cli.command()
@click.option(
    "--output",
    "-o",
    default=None,
    help="Output file path (defaults to path in config).",
)
def download_numerai(output):
    """Download the Numerai meta model."""
    console = Console()
    if output is None:
        output = config.data.path
    try:
        predictions = DataLoader.from_numerai_api(download_path=output)
        display_file_summary(console, predictions, output, "Numerai meta model")
    except Exception as e:
        console.print(f"[red]✗[/red] Failed to download Numerai meta model: {e}")
        raise


@cli.command()
@click.option(
    "--skip-confirm",
    is_flag=True,
    help="Skip confirmation prompt for closing positions.",
)
@click.option(
    "--set",
    "set_overrides",
    multiple=True,
    help="Override config values (e.g., --set is_testnet=true)",
)
@click.option(
    "--force",
    is_flag=True,
    help="Force close positions below min notional by composing a two-step workaround.",
)
def close_all(skip_confirm, set_overrides, force):
    """Close all positions and return to cash."""
    console = Console()

    # Apply CLI overrides to config
    overrides_applied = apply_cli_overrides(config, set_overrides)

    # Create callbacks and trader
    callbacks = RichCLICallbacks()
    trader = CCLiquid(config, callbacks=callbacks)

    # Show applied overrides through callbacks
    callbacks.on_config_override(overrides_applied)

    try:
        # Preview plan first (no execution)
        plan = trader.plan_close_all_positions(force=force)

        # Render plan via callbacks
        all_trades = plan["trades"] + plan["skipped_trades"]
        callbacks.show_trade_plan(
            plan["target_positions"],
            all_trades,
            plan["account_value"],
            plan["leverage"],
        )

        # Confirm/auto-confirm
        if skip_confirm or callbacks.ask_confirmation("Close all positions?"):
            # Execute
            result = trader.execute_plan(plan)
            callbacks.show_execution_summary(
                result["successful_trades"],
                result["all_trades"],
                plan["target_positions"],
                plan["account_value"],
            )
        else:
            callbacks.info("Cancelled by user")
    except Exception as e:
        console.print(f"[red]✗ Error closing positions:[/red] {e}")
        traceback.print_exc()


@cli.command()
@click.option(
    "--skip-confirm",
    is_flag=True,
    help="Skip confirmation prompt for executing trades.",
)
@click.option(
    "--set",
    "set_overrides",
    multiple=True,
    help="Override config values (e.g., --set data.source=numerai --set portfolio.num_long=10)",
)
def rebalance(skip_confirm, set_overrides):
    """Execute rebalancing based on the configured data source."""

    # Apply CLI overrides to config
    overrides_applied = apply_cli_overrides(config, set_overrides)

    # Create callbacks and trader
    callbacks = RichCLICallbacks()
    trader = CCLiquid(config, callbacks=callbacks)

    # Show applied overrides through callbacks
    callbacks.on_config_override(overrides_applied)

    # Preview plan first (no execution)
    plan = trader.plan_rebalance()

    # Render plan via callbacks
    all_trades = plan["trades"] + plan["skipped_trades"]
    callbacks.show_trade_plan(
        plan["target_positions"], all_trades, plan["account_value"], plan["leverage"]
    )

    # Confirm/auto-confirm
    if skip_confirm or callbacks.ask_confirmation("Execute these trades?"):
        result = trader.execute_plan(plan)
        callbacks.show_execution_summary(
            result["successful_trades"],
            result["all_trades"],
            plan["target_positions"],
            plan["account_value"],
        )
    else:
        callbacks.info("Trading cancelled by user")


@cli.command()
@click.option(
    "--prices",
    default="raw_data.parquet",
    help="Path to price data (parquet file with date, id, close columns)",
)
@click.option(
    "--start-date",
    type=click.DateTime(),
    help="Start date for backtest (YYYY-MM-DD)",
)
@click.option(
    "--end-date",
    type=click.DateTime(),
    help="End date for backtest (YYYY-MM-DD)",
)
@click.option(
    "--set",
    "set_overrides",
    multiple=True,
    help="Override config values (e.g., --set portfolio.num_long=15 --set data.source=numerai)",
)
@click.option(
    "--fee-bps",
    type=float,
    default=2.5,
    help="Trading fee in basis points",
)
@click.option(
    "--slippage-bps",
    type=float,
    default=5.0,
    help="Slippage cost in basis points",
)
@click.option(
    "--prediction-lag",
    type=int,
    default=1,
    help="Days between prediction date and trading date (default: 1, use higher values to avoid look-ahead bias)",
)
@click.option(
    "--save-daily",
    help="Save daily results to CSV file",
)
@click.option(
    "--show-positions",
    is_flag=True,
    help="Show detailed position analysis table",
)
@click.option(
    "--verbose",
    is_flag=True,
    help="Show detailed progress",
)
def analyze(
    prices,
    start_date,
    end_date,
    set_overrides,
    fee_bps,
    slippage_bps,
    prediction_lag,
    save_daily,
    show_positions,
    verbose,
):
    """Run backtest analysis on historical data.
    
    ⚠️ IMPORTANT DISCLAIMER:
    Past performance does not guarantee future results. Backtesting results are
    hypothetical and have inherent limitations. Actual trading results may differ
    significantly. Always consider market conditions, liquidity, and execution costs
    that may not be fully captured in simulations.
    """
    from .backtester import Backtester, BacktestConfig
    from .config import config

    console = Console()

    # Apply CLI overrides to config (includes smart defaults for data.source changes)
    overrides_applied = apply_cli_overrides(config, set_overrides)

    # Show applied overrides through console
    if overrides_applied:
        console.print("[cyan]Configuration overrides applied:[/cyan]")
        for override in overrides_applied:
            console.print(f"  • {override}")
        console.print()

    # Now use the config value (which may have been overridden)
    predictions = config.data.path

    # Create backtest config using the updated config values
    bt_config = BacktestConfig(
        prices_path=prices,
        predictions_path=predictions,
        # Use config columns for predictions
        pred_date_column=config.data.date_column,
        pred_id_column=config.data.asset_id_column,
        pred_value_column=config.data.prediction_column,
        data_provider=config.data.source,
        start_date=start_date,
        end_date=end_date,
        num_long=config.portfolio.num_long,
        num_short=config.portfolio.num_short,
        target_leverage=config.portfolio.target_leverage,
        weighting_scheme=config.portfolio.weighting_scheme,
        rank_power=config.portfolio.rank_power,
        rebalance_every_n_days=config.portfolio.rebalancing.every_n_days,
        prediction_lag_days=prediction_lag,
        fee_bps=fee_bps,
        slippage_bps=slippage_bps,
        verbose=verbose,
    )

    try:
        # Run backtest with spinner
        from rich.spinner import Spinner
        from rich.live import Live

        with Live(
            Spinner("dots", text="Running backtest..."), console=console, transient=True
        ):
            backtester = Backtester(bt_config)
            result = backtester.run()

        display_backtest_summary(console, result, bt_config, show_positions=show_positions)

        # Save daily results if requested
        if save_daily:
            result.daily.write_csv(save_daily)
            console.print(f"\n[green]✓[/green] Saved daily results to {save_daily}")

    except Exception as e:
        from rich.markup import escape

        console.print(f"[red]✗ Backtest failed: {escape(str(e))}[/red]")
        if verbose:
            import traceback

            traceback.print_exc()


@cli.command()
@click.option(
    "--prices",
    default="raw_data.parquet",
    help="Path to price data",
)
@click.option(
    "--start-date",
    type=click.DateTime(),
    help="Start date for backtest (YYYY-MM-DD)",
)
@click.option(
    "--end-date",
    type=click.DateTime(),
    help="End date for backtest (YYYY-MM-DD)",
)
@click.option(
    "--set",
    "set_overrides",
    multiple=True,
    help="Override config values (e.g., --set data.source=numerai)",
)
@click.option(
    "--num-longs",
    default="10,20,30,40,50",
    help="Comma-separated list of long positions to test",
)
@click.option(
    "--num-shorts",
    default="10,20,30,40,50",
    help="Comma-separated list of short positions to test",
)
@click.option(
    "--leverages",
    default="1.0,2.0,3.0,4.0,5.0",
    help="Comma-separated list of leverage values to test",
)
@click.option(
    "--rebalance-days",
    default="8,10,12",
    help="Comma-separated list of rebalance frequencies to test",
)
@click.option(
    "--metric",
    type=click.Choice(["sharpe", "cagr", "calmar"]),
    default="sharpe",
    help="Optimization metric",
)
@click.option(
    "--max-drawdown",
    type=float,
    help="Maximum drawdown constraint (e.g., 0.2 for 20%)",
)
@click.option(
    "--fee-bps",
    type=float,
    default=2.5,
    help="Trading fee in basis points",
)
@click.option(
    "--slippage-bps",
    type=float,
    default=5.0,
    help="Slippage cost in basis points",
)
@click.option(
    "--prediction-lag",
    type=int,
    default=1,
    help="Days between prediction date and trading date (default: 1, use higher values to avoid look-ahead bias)",
)
@click.option(
    "--top-n",
    type=int,
    default=20,
    help="Show top N results",
)
@click.option(
    "--apply-best",
    is_flag=True,
    help="Run full analysis with best parameters",
)
@click.option(
    "--save-results",
    help="Save optimization results to CSV",
)
@click.option(
    "--plot",
    is_flag=True,
    help="Show contour plots of results",
)
@click.option(
    "--max-workers",
    type=int,
    help="Number of parallel workers (default: auto)",
)
@click.option(
    "--clear-cache",
    is_flag=True,
    help="Clear cached optimization results",
)
@click.option(
    "--verbose",
    is_flag=True,
    help="Show detailed progress",
)
def optimize(
    prices,
    start_date,
    end_date,
    set_overrides,
    num_longs,
    num_shorts,
    leverages,
    rebalance_days,
    metric,
    max_drawdown,
    fee_bps,
    slippage_bps,
    prediction_lag,
    top_n,
    apply_best,
    save_results,
    plot,
    max_workers,
    clear_cache,
    verbose,
):
    """Optimize backtest parameters using parallel grid search.
    
    ⚠️ IMPORTANT DISCLAIMER:
    Optimization results are based on historical data and are subject to overfitting.
    Parameters that performed well in the past may not perform well in the future.
    Always use out-of-sample testing and forward walk analysis. Consider that
    optimized parameters may be curve-fit to historical noise rather than true patterns.
    """
    console = Console()

    # Apply CLI overrides to config (includes smart defaults for data.source changes)
    overrides_applied = apply_cli_overrides(config, set_overrides)

    # Show applied overrides through console
    if overrides_applied:
        console.print("[cyan]Configuration overrides applied:[/cyan]")
        for override in overrides_applied:
            console.print(f"  • {override}")
        console.print()

    # Parse parameter lists
    num_longs_list = [int(x.strip()) for x in num_longs.split(",")]
    num_shorts_list = [int(x.strip()) for x in num_shorts.split(",")]
    leverages_list = [float(x.strip()) for x in leverages.split(",")]
    rebalance_days_list = [int(x.strip()) for x in rebalance_days.split(",")]

    # Now use the config value (which may have been overridden)
    predictions = config.data.path

    # Create base config with all parameters
    base_config = BacktestConfig(
        prices_path=prices,
        predictions_path=predictions,
        pred_date_column=config.data.date_column,
        pred_id_column=config.data.asset_id_column,
        pred_value_column=config.data.prediction_column,
        data_provider=config.data.source,
        start_date=start_date,
        end_date=end_date,
        weighting_scheme=config.portfolio.weighting_scheme,
        rank_power=config.portfolio.rank_power,
        prediction_lag_days=prediction_lag,
        fee_bps=fee_bps,
        slippage_bps=slippage_bps,
        verbose=verbose,
    )

    try:
        # Calculate total combinations
        total_combos = (
            len(num_longs_list)
            * len(num_shorts_list)
            * len(leverages_list)
            * len(rebalance_days_list)
        )

        # Create optimizer
        optimizer = BacktestOptimizer(base_config)

        # Clear cache if requested
        if clear_cache:
            optimizer.clear_cache()
            console.print("[yellow]Cache cleared[/yellow]\n")

        # Show optimization header
        header = create_header_panel(
            f"OPTIMIZATION :: {total_combos} COMBINATIONS"
        )
        console.print(header)
        console.print(f"\nOptimizing for: [bold yellow]{metric.upper()}[/bold yellow]")
        if max_drawdown:
            console.print(
                f"Max drawdown constraint: [yellow]{max_drawdown:.1%}[/yellow]"
            )
        console.print(
            f"Parameters: L={num_longs_list} S={num_shorts_list} Lev={leverages_list} Days={rebalance_days_list}"
        )

        if max_workers:
            console.print(f"Parallel workers: [cyan]{max_workers}[/cyan]")
        else:
            import multiprocessing as mp

            auto_workers = min(mp.cpu_count(), 24)
            console.print(f"Parallel workers: [cyan]{auto_workers}[/cyan] (auto)")
        console.print()

        # Run optimization with Rich Progress
        with Progress(
            TextColumn("[progress.description]{task.description}"),
            BarColumn(),
            MofNCompleteColumn(),
            TextColumn("•"),
            TimeElapsedColumn(),
            TextColumn("•"),
            TimeRemainingColumn(),
            console=console,
            expand=False,
        ) as progress:
            # Run parallel optimization
            results_df = optimizer.grid_search_parallel(
                num_longs=num_longs_list,
                num_shorts=num_shorts_list,
                leverages=leverages_list,
                rebalance_days=rebalance_days_list,
                metric=metric,
                max_drawdown_limit=max_drawdown,
                max_workers=max_workers,
                progress_callback=progress,
            )

        if len(results_df) == 0:
            console.print("[red]No valid results found[/red]")
            return

        # Display results
        console.print()  # Space after progress
        display_optimization_results(console, results_df, metric, top_n, base_config)

        # Show contour plots if requested
        if plot:
            display_optimization_contours(console, results_df, metric)

        # Save results if requested
        if save_results:
            results_df.write_csv(save_results)
            console.print(f"\n[green]✓[/green] Saved results to {save_results}")

        # Apply best parameters if requested
        if apply_best:
            best_params = optimizer.get_best_params(results_df, metric)
            if best_params:
                console.print(
                    "\n[bold cyan]Running full analysis with best parameters...[/bold cyan]"
                )
                console.print(
                    f"Best params: L={best_params['num_long']}, S={best_params['num_short']}, "
                    f"Lev={best_params['target_leverage']:.1f}x, Days={best_params['rebalance_every_n_days']}"
                )

                # Create config with best params and all other settings
                best_config = BacktestConfig(
                    prices_path=prices,
                    predictions_path=predictions,
                    pred_date_column=config.data.date_column,
                    pred_id_column=config.data.asset_id_column,
                    pred_value_column=config.data.prediction_column,
                    data_provider=config.data.source,
                    start_date=start_date,
                    end_date=end_date,
                    num_long=best_params["num_long"],
                    num_short=best_params["num_short"],
                    target_leverage=best_params["target_leverage"],
                    rebalance_every_n_days=best_params["rebalance_every_n_days"],
                    weighting_scheme=config.portfolio.weighting_scheme,
                    rank_power=config.portfolio.rank_power,
                    prediction_lag_days=prediction_lag,
                    fee_bps=fee_bps,
                    slippage_bps=slippage_bps,
                    verbose=False,
                )

                # Run backtest
                backtester = Backtester(best_config)
                result = backtester.run()

                display_backtest_summary(console, result, best_config, show_positions=False)

    except Exception as e:
        from rich.markup import escape

        console.print(f"[red]✗ Optimization failed: {escape(str(e))}[/red]")
        if verbose:
            import traceback

            traceback.print_exc()


@cli.command()
@click.option(
    "--skip-confirm",
    is_flag=True,
    help="Skip confirmation prompt for executing trades.",
)
@click.option(
    "--set",
    "set_overrides",
    multiple=True,
    help="Override config values (e.g., --set is_testnet=true)",
)
@click.option(
    "--refresh",
    type=float,
    default=1.0,
    show_default=True,
    help="Dashboard update cadence in seconds.",
)
@click.option(
    "--tmux",
    is_flag=True,
    help="Run inside a fixed tmux session (attach if exists, else create and run).",
)
def run(skip_confirm, set_overrides, refresh, tmux):
    """Start continuous monitoring and automatic rebalancing with live dashboard."""
    # Minimal tmux wrapper: attach-or-create a fixed session, with recursion guard
    if tmux and os.environ.get("CCLIQUID_TMUX_CHILD") != "1":
        if shutil.which("tmux") is None:
            raise click.ClickException(
                "tmux not found in PATH. Please install tmux to use --tmux."
            )

        inside_tmux = bool(os.environ.get("TMUX"))

        # Check if fixed session exists
        session_exists = (
            subprocess.call(
                ["tmux", "has-session", "-t", TMUX_SESSION_NAME],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            == 0
        )

        if session_exists:
            # Attach or switch to existing session
            if inside_tmux:
                subprocess.check_call(
                    ["tmux", "switch-client", "-t", TMUX_SESSION_NAME]
                )
                return
            else:
                os.execvp("tmux", ["tmux", "attach", "-t", TMUX_SESSION_NAME])

        # Create the session and run inner command with guard set
        inner_cmd = [
            "uv",
            "run",
            "-m",
            "cc_liquid.cli",
            "run",
        ]
        if skip_confirm:
            inner_cmd.append("--skip-confirm")
        for override in set_overrides:
            inner_cmd.extend(["--set", override])
        inner_cmd.extend(["--refresh", str(refresh)])

        # Build a single shell-quoted command string with guard env var
        command_string = f"CCLIQUID_TMUX_CHILD=1 {shlex.join(inner_cmd)}"

        subprocess.check_call(
            [
                "tmux",
                "new-session",
                "-d",
                "-s",
                TMUX_SESSION_NAME,
                "-n",
                TMUX_WINDOW_NAME,
                command_string,
            ]
        )

        if inside_tmux:
            subprocess.check_call(["tmux", "switch-client", "-t", TMUX_SESSION_NAME])
            return
        else:
            os.execvp("tmux", ["tmux", "attach", "-t", TMUX_SESSION_NAME])

    # Normal, non-tmux path
    overrides_applied = apply_cli_overrides(config, set_overrides)
    run_live_cli(config, skip_confirm, overrides_applied, refresh)


def run_live_cli(
    config_obj,
    skip_confirm: bool,
    overrides_applied: list[str],
    refresh_seconds: float = 1.0,
):
    """Run continuous monitoring with live dashboard.

    Args:
        config_obj: The configuration object
        skip_confirm: Whether to skip confirmations during rebalancing
        overrides_applied: List of CLI overrides applied (for display)
        refresh_seconds: UI update cadence in seconds
    """
    console = Console()

    # Create trader with initial callbacks and load state
    callbacks = RichCLICallbacks()
    trader = CCLiquid(config_obj, callbacks=callbacks)

    # Show applied overrides if any (route via callbacks)
    callbacks.on_config_override(overrides_applied)
    if overrides_applied:
        time.sleep(2)  # Brief pause to show overrides

    last_rebalance_date = trader.load_state()

    # converts seconds per refresh to Live's refresh-per-second value
    live_rps = 1.0 / refresh_seconds if refresh_seconds > 0 else 1.0
    from rich.spinner import Spinner
    from rich.live import Live

    spinner = Spinner("dots", text="Loading...")
    with Live(
        spinner,
        console=console,
        screen=True,  # Use alternate screen
        refresh_per_second=live_rps,
        transient=False,
    ) as live:
        # quick loading screen
        try:
            while True:
                # Get current portfolio state
                portfolio = trader.get_portfolio_info()

                # Calculate next rebalance time and determine if due
                next_action_time = trader.compute_next_rebalance_time(
                    last_rebalance_date
                )
                now = datetime.now(timezone.utc)
                should_rebalance = now >= next_action_time

                if should_rebalance:
                    # Stop the live display to run the standard rebalancing flow
                    live.stop()

                    try:
                        console.print(
                            "\n[bold yellow]-- Scheduled rebalance started --[/bold yellow]"
                        )
                        # Preview plan
                        plan = trader.plan_rebalance()
                        all_trades = plan["trades"] + plan["skipped_trades"]
                        callbacks.show_trade_plan(
                            plan["target_positions"],
                            all_trades,
                            plan["account_value"],
                            plan["leverage"],
                        )

                        proceed = skip_confirm or callbacks.ask_confirmation(
                            "Execute these trades?"
                        )
                        if proceed:
                            result = trader.execute_plan(plan)
                            callbacks.show_execution_summary(
                                result["successful_trades"],
                                result["all_trades"],
                                plan["target_positions"],
                                plan["account_value"],
                            )
                        else:
                            callbacks.info("Trading cancelled by user")

                        # Update state on successful completion
                        last_rebalance_date = datetime.now(timezone.utc)
                        trader.save_state(last_rebalance_date)

                        console.input(
                            "\n[bold green]✓ Rebalance cycle finished. Press [bold]Enter[/bold] to resume dashboard...[/bold green]"
                        )

                    except Exception as e:
                        console.print(
                            f"\n[bold red]✗ Rebalancing failed:[/bold red] {e}"
                        )
                        traceback.print_exc()
                        console.input(
                            "\n[yellow]Press [bold]Enter[/bold] to resume dashboard...[/yellow]"
                        )
                    finally:
                        # Resume the live dashboard
                        live.start()
                        # Continue to the next loop iteration to immediately refresh the dashboard
                        continue

                else:
                    # Normal monitoring dashboard
                    dashboard = create_dashboard_layout(
                        portfolio=portfolio,
                        next_rebalance_time=next_action_time,
                        last_rebalance_time=last_rebalance_date,
                        is_rebalancing=False,
                        config_dict=config_obj.to_dict(),
                        refresh_seconds=refresh_seconds,
                    )
                    live.update(dashboard)

                # Sleep to control dashboard update cadence and API usage
                time.sleep(refresh_seconds if refresh_seconds > 0 else 1)

        except KeyboardInterrupt:
            pass
        except Exception as e:
            console.print(f"[red]✗ Error:[/red] {e}")
            traceback.print_exc()


if __name__ == "__main__":
    cli()
