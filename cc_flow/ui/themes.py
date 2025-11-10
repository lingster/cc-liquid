"""Theme definitions for cc-liquid TUI.

This module provides theme definitions that map to Textual's built-in themes
and custom themes. Each theme specifies color values for consistent styling
across all widgets.

Themes are defined as dictionaries with CSS variable names and their values.
The app applies these as CSS variables that can be referenced in TCSS files.

Available Themes:
    - brutalist: Original high-contrast design (default)
    - textual-dark: Textual's default dark theme
    - textual-light: Clean light theme
    - textual-ansi: Standard ANSI colors
    - nord: Nordic-inspired muted palette
    - monokai: Vibrant dark theme from code editors
    - dracula: Purple and pink accented dark theme
    - gruvbox: Retro warm earthy tones
    - catppuccin-mocha: Pastel dark theme (warmest)
    - catppuccin-macchiato: Pastel dark theme (warm)
    - catppuccin-frappe: Pastel dark theme (cool)
    - catppuccin-latte: Pastel light theme

Example:
    >>> from cc_flow.ui.themes import THEMES, apply_theme
    >>> theme = THEMES["nord"]
    >>> print(theme["primary"])
    #88c0d0
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

# Type for available theme names
ThemeName = Literal[
    "brutalist",
    "textual-dark",
    "textual-light",
    "textual-ansi",
    "nord",
    "monokai",
    "dracula",
    "gruvbox",
    "catppuccin-mocha",
    "catppuccin-macchiato",
    "catppuccin-frappe",
    "catppuccin-latte",
]


@dataclass
class Theme:
    """Theme color scheme definition.

    Attributes:
        name: Theme name identifier
        display_name: Human-readable theme name
        description: Brief description of the theme
        background: Main background color
        surface: Secondary background (panels, containers)
        primary: Primary accent color
        secondary: Secondary accent color
        text: Default text color
        text_muted: Muted/secondary text color
        success: Success state color
        warning: Warning state color
        error: Error state color
        border: Border color
        border_focus: Focused element border color
        header_bg: Header background
        header_fg: Header text color
        footer_bg: Footer background
        footer_fg: Footer text color
    """

    name: str
    display_name: str
    description: str
    background: str
    surface: str
    primary: str
    secondary: str
    text: str
    text_muted: str
    success: str
    warning: str
    error: str
    border: str
    border_focus: str
    header_bg: str
    header_fg: str
    footer_bg: str
    footer_fg: str

    def to_css_variables(self) -> str:
        """Convert theme to CSS variable declarations.

        Returns:
            CSS variable declarations as a string
        """
        return f"""
            $background: {self.background};
            $surface: {self.surface};
            $primary: {self.primary};
            $secondary: {self.secondary};
            $text: {self.text};
            $text-muted: {self.text_muted};
            $success: {self.success};
            $warning: {self.warning};
            $error: {self.error};
            $border: {self.border};
            $border-focus: {self.border_focus};
            $header-bg: {self.header_bg};
            $header-fg: {self.header_fg};
            $footer-bg: {self.footer_bg};
            $footer-fg: {self.footer_fg};
        """


# Theme Definitions

BRUTALIST = Theme(
    name="brutalist",
    display_name="Brutalist (Original)",
    description="High-contrast, functional design with cyan and purple",
    background="#001926",
    surface="#002030",
    primary="#62e4fb",
    secondary="#4152A8",
    text="#62e4fb",
    text_muted="#aaaaaa",
    success="#00ff00",
    warning="#ffaa00",
    error="#ff0000",
    border="#62e4fb",
    border_focus="#62e4fb",
    header_bg="#4152A8",
    header_fg="#62e4fb",
    footer_bg="#002030",
    footer_fg="#62e4fb",
)

TEXTUAL_DARK = Theme(
    name="textual-dark",
    display_name="Textual Dark",
    description="Default dark theme with blue accents",
    background="#0d1117",
    surface="#161b22",
    primary="#58a6ff",
    secondary="#1f6feb",
    text="#c9d1d9",
    text_muted="#8b949e",
    success="#3fb950",
    warning="#d29922",
    error="#f85149",
    border="#30363d",
    border_focus="#58a6ff",
    header_bg="#161b22",
    header_fg="#58a6ff",
    footer_bg="#0d1117",
    footer_fg="#c9d1d9",
)

TEXTUAL_LIGHT = Theme(
    name="textual-light",
    display_name="Textual Light",
    description="Clean light theme with subtle colors",
    background="#ffffff",
    surface="#f6f8fa",
    primary="#0969da",
    secondary="#0550ae",
    text="#24292f",
    text_muted="#57606a",
    success="#1a7f37",
    warning="#9a6700",
    error="#cf222e",
    border="#d0d7de",
    border_focus="#0969da",
    header_bg="#f6f8fa",
    header_fg="#0969da",
    footer_bg="#ffffff",
    footer_fg="#24292f",
)

TEXTUAL_ANSI = Theme(
    name="textual-ansi",
    display_name="Textual ANSI",
    description="Standard ANSI colors for terminal compatibility",
    background="#000000",
    surface="#1a1a1a",
    primary="#00aaff",
    secondary="#0055aa",
    text="#ffffff",
    text_muted="#aaaaaa",
    success="#00ff00",
    warning="#ffff00",
    error="#ff0000",
    border="#ffffff",
    border_focus="#00aaff",
    header_bg="#0055aa",
    header_fg="#ffffff",
    footer_bg="#1a1a1a",
    footer_fg="#ffffff",
)

NORD = Theme(
    name="nord",
    display_name="Nord",
    description="Nordic-inspired muted blues and grays",
    background="#2e3440",
    surface="#3b4252",
    primary="#88c0d0",
    secondary="#81a1c1",
    text="#eceff4",
    text_muted="#d8dee9",
    success="#a3be8c",
    warning="#ebcb8b",
    error="#bf616a",
    border="#4c566a",
    border_focus="#88c0d0",
    header_bg="#3b4252",
    header_fg="#88c0d0",
    footer_bg="#2e3440",
    footer_fg="#eceff4",
)

MONOKAI = Theme(
    name="monokai",
    display_name="Monokai",
    description="Classic dark theme with vibrant colors",
    background="#272822",
    surface="#3e3d32",
    primary="#66d9ef",
    secondary="#a6e22e",
    text="#f8f8f2",
    text_muted="#75715e",
    success="#a6e22e",
    warning="#e6db74",
    error="#f92672",
    border="#75715e",
    border_focus="#66d9ef",
    header_bg="#3e3d32",
    header_fg="#66d9ef",
    footer_bg="#272822",
    footer_fg="#f8f8f2",
)

DRACULA = Theme(
    name="dracula",
    display_name="Dracula",
    description="Purple and pink accented dark theme",
    background="#282a36",
    surface="#44475a",
    primary="#bd93f9",
    secondary="#ff79c6",
    text="#f8f8f2",
    text_muted="#6272a4",
    success="#50fa7b",
    warning="#f1fa8c",
    error="#ff5555",
    border="#6272a4",
    border_focus="#bd93f9",
    header_bg="#44475a",
    header_fg="#bd93f9",
    footer_bg="#282a36",
    footer_fg="#f8f8f2",
)

GRUVBOX = Theme(
    name="gruvbox",
    display_name="Gruvbox",
    description="Retro warm palette with earthy tones",
    background="#282828",
    surface="#3c3836",
    primary="#83a598",
    secondary="#fe8019",
    text="#ebdbb2",
    text_muted="#928374",
    success="#b8bb26",
    warning="#fabd2f",
    error="#fb4934",
    border="#504945",
    border_focus="#83a598",
    header_bg="#3c3836",
    header_fg="#83a598",
    footer_bg="#282828",
    footer_fg="#ebdbb2",
)

CATPPUCCIN_MOCHA = Theme(
    name="catppuccin-mocha",
    display_name="Catppuccin Mocha",
    description="Pastel dark theme - warmest variant",
    background="#1e1e2e",
    surface="#313244",
    primary="#89b4fa",
    secondary="#cba6f7",
    text="#cdd6f4",
    text_muted="#a6adc8",
    success="#a6e3a1",
    warning="#f9e2af",
    error="#f38ba8",
    border="#45475a",
    border_focus="#89b4fa",
    header_bg="#313244",
    header_fg="#89b4fa",
    footer_bg="#1e1e2e",
    footer_fg="#cdd6f4",
)

CATPPUCCIN_MACCHIATO = Theme(
    name="catppuccin-macchiato",
    display_name="Catppuccin Macchiato",
    description="Pastel dark theme - warm variant",
    background="#24273a",
    surface="#363a4f",
    primary="#8aadf4",
    secondary="#c6a0f6",
    text="#cad3f5",
    text_muted="#a5adcb",
    success="#a6da95",
    warning="#eed49f",
    error="#ed8796",
    border="#494d64",
    border_focus="#8aadf4",
    header_bg="#363a4f",
    header_fg="#8aadf4",
    footer_bg="#24273a",
    footer_fg="#cad3f5",
)

CATPPUCCIN_FRAPPE = Theme(
    name="catppuccin-frappe",
    display_name="Catppuccin Frappé",
    description="Pastel dark theme - cool variant",
    background="#303446",
    surface="#414559",
    primary="#8caaee",
    secondary="#ca9ee6",
    text="#c6d0f5",
    text_muted="#a5adce",
    success="#a6d189",
    warning="#e5c890",
    error="#e78284",
    border="#51576d",
    border_focus="#8caaee",
    header_bg="#414559",
    header_fg="#8caaee",
    footer_bg="#303446",
    footer_fg="#c6d0f5",
)

CATPPUCCIN_LATTE = Theme(
    name="catppuccin-latte",
    display_name="Catppuccin Latte",
    description="Pastel light theme",
    background="#eff1f5",
    surface="#e6e9ef",
    primary="#1e66f5",
    secondary="#8839ef",
    text="#4c4f69",
    text_muted="#6c6f85",
    success="#40a02b",
    warning="#df8e1d",
    error="#d20f39",
    border="#9ca0b0",
    border_focus="#1e66f5",
    header_bg="#e6e9ef",
    header_fg="#1e66f5",
    footer_bg="#eff1f5",
    footer_fg="#4c4f69",
)

# Dictionary of all available themes
THEMES: dict[str, Theme] = {
    "brutalist": BRUTALIST,
    "textual-dark": TEXTUAL_DARK,
    "textual-light": TEXTUAL_LIGHT,
    "textual-ansi": TEXTUAL_ANSI,
    "nord": NORD,
    "monokai": MONOKAI,
    "dracula": DRACULA,
    "gruvbox": GRUVBOX,
    "catppuccin-mocha": CATPPUCCIN_MOCHA,
    "catppuccin-macchiato": CATPPUCCIN_MACCHIATO,
    "catppuccin-frappe": CATPPUCCIN_FRAPPE,
    "catppuccin-latte": CATPPUCCIN_LATTE,
}

# List of theme names for selection UI
THEME_NAMES: list[str] = list(THEMES.keys())


def get_theme(name: str) -> Theme:
    """Get theme by name.

    Args:
        name: Theme name (must be in THEMES)

    Returns:
        Theme object

    Raises:
        ValueError: If theme name not found
    """
    if name not in THEMES:
        raise ValueError(
            f"Theme '{name}' not found. Available: {', '.join(THEME_NAMES)}"
        )
    return THEMES[name]


def get_default_theme() -> Theme:
    """Get default theme (brutalist).

    Returns:
        Default Theme object
    """
    return BRUTALIST
