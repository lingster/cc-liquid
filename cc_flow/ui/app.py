"""Main Textualize TUI application for cc-liquid.

This module provides the main application class that orchestrates the terminal
user interface, managing screen navigation, key bindings, and integration with
core trading services.

The app uses a brutalist design philosophy with high-contrast colors and
functional, information-dense interfaces.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.widgets import Footer, Header

from cc_flow.utils.logger_config import log

if TYPE_CHECKING:
    from cc_flow.domain.config import TradingConfig
    from cc_flow.domain.data_source import DataSource
    from cc_flow.domain.exchange import Exchange


class CCLiquidApp(App):
    """Main application for cc-liquid TUI.

    The app provides navigation between different screens:
    - Dashboard: Live portfolio monitoring
    - Trading: Manual rebalancing
    - Account: Detailed account view
    - Backtest: Strategy backtesting
    - Optimize: Parameter optimization
    - History: Trade history
    - Config: Configuration management

    Attributes:
        CSS_PATH: Path to the stylesheet (relative to ui/ directory)
        TITLE: Application title shown in header
        SUB_TITLE: Application subtitle shown in header
        BINDINGS: List of key bindings for navigation
        exchange: Exchange implementation for trading operations
        data_source: Data source for predictions
        config: Trading configuration
        orchestrator: Trading orchestrator for coordinating operations
        current_screen: Name of currently displayed screen
    """

    CSS_PATH = "styles/main.tcss"
    TITLE = "cc-liquid :: Portfolio Rebalancer"
    SUB_TITLE = "Powered by Hyperliquid"

    BINDINGS = [
        Binding("d", "show_dashboard", "Dashboard", priority=True),
        Binding("t", "show_trading", "Trading", priority=True),
        Binding("a", "show_account", "Account"),
        Binding("b", "show_backtest", "Backtest"),
        Binding("o", "show_optimize", "Optimize"),
        Binding("h", "show_history", "History"),
        Binding("c", "show_config", "Config"),
        Binding("ctrl+t", "show_theme_switcher", "Theme", priority=True),
        Binding("q", "quit", "Quit", priority=True),
    ]

    def __init__(
        self,
        exchange: Exchange,
        data_source: DataSource,
        config: TradingConfig,
        **kwargs,
    ):
        """Initialize app with core services.

        Args:
            exchange: Exchange implementation
            data_source: Data source for predictions
            config: Trading configuration
            **kwargs: Additional Textual app arguments
        """
        super().__init__(**kwargs)
        self.exchange = exchange
        self.data_source = data_source
        self.config = config

        # Initialize orchestrator
        from cc_flow.core.trader import TradingOrchestrator

        self.orchestrator = TradingOrchestrator(
            exchange=exchange, data_source=data_source, config=config
        )

        # Track current screen
        self.current_screen = "dashboard"

        # Track current theme (load from config)
        self.current_theme = config.ui.theme

    def compose(self) -> ComposeResult:
        """Create child widgets.

        Yields:
            Header and Footer widgets
        """
        yield Header(show_clock=True)
        yield Footer()

    def action_show_dashboard(self) -> None:
        """Show dashboard screen."""
        from cc_flow.ui.screens.dashboard import DashboardScreen

        log.info("Switching to dashboard")
        self.current_screen = "dashboard"
        self.push_screen(DashboardScreen(self.orchestrator))

    def action_show_trading(self) -> None:
        """Show trading screen."""
        from cc_flow.ui.screens.trading import TradingScreen

        log.info("Switching to trading")
        self.current_screen = "trading"
        self.push_screen(TradingScreen(self.orchestrator))

    def action_show_account(self) -> None:
        """Show account screen."""
        from cc_flow.ui.screens.account import AccountScreen

        log.info("Switching to account")
        self.current_screen = "account"
        self.push_screen(AccountScreen(self.orchestrator))

    def action_show_backtest(self) -> None:
        """Show backtest screen."""
        from cc_flow.ui.screens.backtest import BacktestScreen

        log.info("Switching to backtest")
        self.current_screen = "backtest"
        self.push_screen(BacktestScreen(self.orchestrator))

    def action_show_optimize(self) -> None:
        """Show optimize screen."""
        from cc_flow.ui.screens.optimize import OptimizeScreen

        log.info("Switching to optimize")
        self.current_screen = "optimize"
        self.push_screen(OptimizeScreen(self.orchestrator))

    def action_show_history(self) -> None:
        """Show history screen."""
        from cc_flow.ui.screens.history import HistoryScreen

        log.info("Switching to history")
        self.current_screen = "history"
        self.push_screen(HistoryScreen(self.orchestrator))

    def action_show_config(self) -> None:
        """Show config screen."""
        from cc_flow.ui.screens.config import ConfigScreen

        log.info("Switching to config")
        self.current_screen = "config"
        self.push_screen(ConfigScreen(self.config))

    def action_show_theme_switcher(self) -> None:
        """Show theme switcher modal."""
        from cc_flow.ui.widgets.theme_switcher import ThemeSwitcherModal

        log.info("Opening theme switcher")

        def handle_theme_selection(theme_name: str | None) -> None:
            """Handle theme selection from modal.

            Args:
                theme_name: Selected theme name, or None if cancelled
            """
            if theme_name is not None:
                log.info(f"Switching to theme: {theme_name}")
                self._apply_theme(theme_name)
            else:
                log.debug("Theme selection cancelled")

        self.push_screen(
            ThemeSwitcherModal(self.current_theme), handle_theme_selection
        )

    def _apply_theme(self, theme_name: str) -> None:
        """Apply a theme to the application.

        Args:
            theme_name: Name of theme to apply
        """
        from cc_flow.ui.themes import get_theme

        try:
            theme = get_theme(theme_name)
            self.current_theme = theme_name

            # Save theme preference to config
            self.config.ui.theme = theme_name

            # Update CSS variables with theme colors
            # Textual will automatically re-render with new colors
            self.stylesheet.set_variables({
                "background": theme.background,
                "surface": theme.surface,
                "primary": theme.primary,
                "secondary": theme.secondary,
                "text": theme.text,
                "text-muted": theme.text_muted,
                "success": theme.success,
                "warning": theme.warning,
                "error": theme.error,
                "border": theme.border,
                "border-focus": theme.border_focus,
                "header-bg": theme.header_bg,
                "header-fg": theme.header_fg,
                "footer-bg": theme.footer_bg,
                "footer-fg": theme.footer_fg,
            })

            log.info(f"Theme '{theme.display_name}' applied successfully")

        except ValueError as e:
            log.error(f"Failed to apply theme: {e}")

    def on_mount(self) -> None:
        """Called when app is mounted.

        Set up initial state and show default screen.
        """
        log.info("cc-liquid TUI started")
        # Apply default theme
        self._apply_theme(self.current_theme)
        self.action_show_dashboard()
