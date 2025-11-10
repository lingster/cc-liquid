"""Theme switcher modal widget.

This module provides a modal dialog for selecting and switching between
different visual themes for the TUI application.

The theme switcher displays all available themes with their descriptions
and allows users to preview and apply themes in real-time.
"""

from __future__ import annotations

from textual import on
from textual.app import ComposeResult
from textual.containers import Container, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, Label, OptionList, Static
from textual.widgets.option_list import Option

from cc_flow.ui.themes import THEME_NAMES, THEMES, get_theme
from cc_flow.utils.logger_config import log


class ThemeSwitcherModal(ModalScreen[str | None]):
    """Modal screen for selecting application theme.

    This modal presents a list of available themes with descriptions.
    Users can select a theme and apply it to the entire application.

    The modal returns the selected theme name when confirmed, or None
    if cancelled.

    Attributes:
        current_theme: Name of currently active theme
        selected_theme: Name of theme selected in the list

    CSS Classes:
        #theme-switcher-modal: Main modal container
        #theme-title: Modal title
        #theme-list: OptionList for theme selection
        #theme-description: Description of selected theme
        #theme-preview: Preview area showing theme colors
        #theme-buttons: Button container
        #btn-apply-theme: Apply button
        #btn-cancel-theme: Cancel button
    """

    DEFAULT_CSS = """
    ThemeSwitcherModal {
        align: center middle;
    }

    #theme-switcher-modal {
        width: 80;
        height: auto;
        max-height: 40;
        background: $surface;
        border: thick $border-focus;
        padding: 2;
    }

    #theme-title {
        text-style: bold;
        color: $primary;
        text-align: center;
        padding: 1;
        background: $header-bg;
        margin-bottom: 1;
    }

    #theme-list {
        height: 15;
        border: solid $border;
        margin-bottom: 1;
    }

    #theme-description {
        color: $text-muted;
        text-align: center;
        padding: 1;
        margin-bottom: 1;
        min-height: 3;
    }

    #theme-preview {
        height: 5;
        border: solid $border;
        padding: 1;
        margin-bottom: 1;
    }

    .preview-row {
        height: 1;
        margin-bottom: 0;
    }

    #theme-buttons {
        align: center middle;
        height: auto;
        padding: 1;
    }

    #theme-buttons > Button {
        margin: 0 1;
    }

    #btn-apply-theme {
        background: $success;
        color: $background;
    }

    #btn-apply-theme:hover {
        background: $primary;
        color: $background;
    }

    #btn-cancel-theme {
        background: $error;
        color: $text;
    }

    #btn-cancel-theme:hover {
        background: $warning;
        color: $background;
    }
    """

    def __init__(self, current_theme: str = "brutalist") -> None:
        """Initialize theme switcher modal.

        Args:
            current_theme: Name of currently active theme
        """
        super().__init__()
        self.current_theme = current_theme
        self.selected_theme = current_theme

    def compose(self) -> ComposeResult:
        """Create child widgets for theme switcher.

        Yields:
            Modal container with theme list and controls
        """
        with Container(id="theme-switcher-modal"):
            yield Label("Select Theme", id="theme-title")

            # Build option list with all themes
            options = []
            for theme_name in THEME_NAMES:
                theme = THEMES[theme_name]
                # Mark current theme
                prompt = (
                    f"● {theme.display_name}"
                    if theme_name == self.current_theme
                    else f"  {theme.display_name}"
                )
                options.append(Option(prompt, id=theme_name))

            yield OptionList(*options, id="theme-list")
            yield Static("", id="theme-description")

            with Vertical(id="theme-preview"):
                yield Static("", id="preview-colors", classes="preview-row")

            with Container(id="theme-buttons"):
                yield Button("Apply Theme", id="btn-apply-theme", variant="success")
                yield Button("Cancel", id="btn-cancel-theme", variant="error")

    def on_mount(self) -> None:
        """Called when modal is mounted.

        Sets up initial selection and preview.
        """
        # Highlight current theme in list
        option_list = self.query_one("#theme-list", OptionList)

        # Find index of current theme
        for idx, theme_name in enumerate(THEME_NAMES):
            if theme_name == self.current_theme:
                option_list.highlighted = idx
                break

        # Show initial description
        self._update_description(self.current_theme)

    @on(OptionList.OptionHighlighted)
    def on_option_highlighted(self, event: OptionList.OptionHighlighted) -> None:
        """Handle theme selection in list.

        Args:
            event: Option highlighted event
        """
        if event.option_id is not None:
            self.selected_theme = event.option_id
            self._update_description(self.selected_theme)
            log.debug(f"Theme highlighted: {self.selected_theme}")

    def _update_description(self, theme_name: str) -> None:
        """Update description and preview for selected theme.

        Args:
            theme_name: Name of theme to preview
        """
        theme = get_theme(theme_name)

        # Update description
        desc_widget = self.query_one("#theme-description", Static)
        desc_widget.update(theme.description)

        # Update preview with color samples
        preview_widget = self.query_one("#preview-colors", Static)
        preview_text = (
            f"[{theme.primary}]●[/] Primary   "
            f"[{theme.secondary}]●[/] Secondary   "
            f"[{theme.success}]●[/] Success   "
            f"[{theme.warning}]●[/] Warning   "
            f"[{theme.error}]●[/] Error"
        )
        preview_widget.update(preview_text)

    @on(Button.Pressed, "#btn-apply-theme")
    def on_apply_pressed(self) -> None:
        """Handle apply button press.

        Dismisses modal with selected theme name.
        """
        log.info(f"Applying theme: {self.selected_theme}")
        self.dismiss(self.selected_theme)

    @on(Button.Pressed, "#btn-cancel-theme")
    def on_cancel_pressed(self) -> None:
        """Handle cancel button press.

        Dismisses modal without applying theme.
        """
        log.debug("Theme selection cancelled")
        self.dismiss(None)

    def on_key(self, event) -> None:
        """Handle keyboard shortcuts.

        Args:
            event: Key event
        """
        if event.key == "escape":
            self.dismiss(None)
        elif event.key == "enter":
            self.dismiss(self.selected_theme)
