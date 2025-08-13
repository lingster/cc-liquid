"""Command-line interface for cc-liquid."""

import os
import time
import traceback
from datetime import UTC, datetime
import subprocess
import shutil
import shlex

import click
from rich.console import Console
from rich.live import Live

from .cli_callbacks import RichCLICallbacks
from .cli_display import (
    create_dashboard_layout,
    create_config_panel,
    display_portfolio,
    display_file_summary,
    show_pre_alpha_warning,
)
from .config import apply_cli_overrides, config
from .data_loader import DataLoader
from .trader import CCLiquid
from .completion import detect_shell_from_env, install_completion
import yaml


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
    console = Console()

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
                now = datetime.now(UTC)
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
                        last_rebalance_date = datetime.now(UTC)
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
