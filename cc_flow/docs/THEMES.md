# Theme System Documentation

## Overview

The cc-flow TUI includes a comprehensive theme system that allows you to customize the visual appearance of the application. You can choose from 12 pre-built themes or create your own custom themes.

## Available Themes

### Dark Themes

1. **Brutalist (Default)**
   - High-contrast design with cyan (#62e4fb) and deep purple (#4152A8)
   - Stark, functional aesthetics
   - Best for: Maximum readability and data density

2. **Textual Dark**
   - Default Textual theme with blue accents (#58a6ff)
   - Clean, modern GitHub-inspired design
   - Best for: General purpose use

3. **Textual ANSI**
   - Standard ANSI colors for maximum terminal compatibility
   - Classic terminal aesthetic
   - Best for: Legacy terminals or SSH sessions

4. **Nord**
   - Nordic-inspired muted blues and grays
   - Soft, easy on the eyes
   - Best for: Extended viewing sessions

5. **Monokai**
   - Classic code editor theme with vibrant colors
   - Familiar to developers
   - Best for: Developers who love Monokai

6. **Dracula**
   - Purple (#bd93f9) and pink (#ff79c6) accents
   - Popular dark theme
   - Best for: Stylish, modern look

7. **Gruvbox**
   - Retro warm palette with earthy tones
   - High contrast with warm colors
   - Best for: Warm color preference

8. **Catppuccin Mocha**
   - Warmest Catppuccin variant
   - Pastel dark theme
   - Best for: Soft, pastel aesthetics

9. **Catppuccin Macchiato**
   - Warm Catppuccin variant
   - Balanced pastel colors
   - Best for: Balanced warmth

10. **Catppuccin Frappé**
    - Cool Catppuccin variant
    - Cooler pastel tones
    - Best for: Cool color preference

### Light Themes

11. **Textual Light**
    - Clean light theme with subtle colors
    - Professional appearance
    - Best for: Daytime use, presentations

12. **Catppuccin Latte**
    - Light Catppuccin variant
    - Soft pastel colors
    - Best for: Light theme with pastel aesthetics

## How to Switch Themes

### Using the Theme Switcher (Interactive)

1. Press `Ctrl+T` while the app is running
2. Use arrow keys to navigate through available themes
3. See live preview of theme colors
4. Press `Enter` or click "Apply Theme" to switch
5. Press `Escape` or click "Cancel" to close without changing

### Using Configuration File

Edit your configuration file to set a default theme:

```yaml
ui:
  theme: nord  # or any other theme name
```

Available theme names:
- `brutalist`
- `textual-dark`
- `textual-light`
- `textual-ansi`
- `nord`
- `monokai`
- `dracula`
- `gruvbox`
- `catppuccin-mocha`
- `catppuccin-macchiato`
- `catppuccin-frappe`
- `catppuccin-latte`

### Programmatically

```python
from cc_flow.ui.app import CCLiquidApp
from cc_flow.domain.config import TradingConfig

# Load config and set theme
config = TradingConfig()
config.ui.theme = "dracula"

# App will automatically load the theme
app = CCLiquidApp(exchange, data_source, config)
```

## Theme Persistence

Theme selections are automatically saved to your configuration and will persist across application restarts. The theme preference is stored in the `ui.theme` field of your configuration.

## Color Scheme

Each theme defines the following color variables:

| Variable | Purpose | Example (Brutalist) |
|----------|---------|---------------------|
| `background` | Main background | #001926 (dark abyss) |
| `surface` | Panels, containers | #002030 (lighter dark) |
| `primary` | Primary accent | #62e4fb (cyan) |
| `secondary` | Secondary accent | #4152A8 (purple) |
| `text` | Default text | #62e4fb (cyan) |
| `text-muted` | Secondary text | #aaaaaa (gray) |
| `success` | Success states | #00ff00 (green) |
| `warning` | Warning states | #ffaa00 (orange) |
| `error` | Error states | #ff0000 (red) |
| `border` | Borders | #62e4fb (cyan) |
| `border-focus` | Focused borders | #62e4fb (cyan) |
| `header-bg` | Header background | #4152A8 (purple) |
| `header-fg` | Header text | #62e4fb (cyan) |
| `footer-bg` | Footer background | #002030 (dark) |
| `footer-fg` | Footer text | #62e4fb (cyan) |

## Creating Custom Themes

You can create custom themes by editing `cc_flow/ui/themes.py`:

```python
from cc_flow.ui.themes import Theme

MY_THEME = Theme(
    name="my-theme",
    display_name="My Custom Theme",
    description="My personal color scheme",
    background="#1a1a1a",
    surface="#2a2a2a",
    primary="#ff6b6b",
    secondary="#4ecdc4",
    text="#f0f0f0",
    text_muted="#888888",
    success="#51cf66",
    warning="#ffd43b",
    error="#ff6b6b",
    border="#4ecdc4",
    border_focus="#ff6b6b",
    header_bg="#2a2a2a",
    header_fg="#ff6b6b",
    footer_bg="#1a1a1a",
    footer_fg="#f0f0f0",
)

# Add to THEMES dictionary
THEMES["my-theme"] = MY_THEME
THEME_NAMES.append("my-theme")
```

After adding your theme, restart the application and it will appear in the theme switcher.

## Widget Compatibility

All widgets in cc-flow are fully compatible with the theme system:

- **DataTable**: Headers, rows, and cursor adapt to theme
- **Buttons**: Background, text, and hover states
- **Inputs**: Background, text, and focus indicators
- **Modals**: All modal dialogs use theme colors
- **Panels**: Borders and backgrounds
- **Charts**: Uses theme colors for visualization
- **Order Books**: Bid/ask colors adapt to theme

## Accessibility

When choosing or creating themes, consider:

1. **Contrast Ratios**: Ensure sufficient contrast between text and background
2. **Color Blindness**: Avoid relying solely on color (use text labels too)
3. **Terminal Compatibility**: Some terminals may not support all colors
4. **Readability**: Test with actual trading data to ensure numbers are readable

## Theme Development

### CSS Variables

All styles use CSS variables defined in `cc_flow/ui/styles/main.tcss`. The theme system dynamically updates these variables when you switch themes.

Example from TCSS:

```css
Button {
    background: $secondary;
    color: $primary;
    border: solid $border;
}

Button:hover {
    background: $primary;
    color: $background;
}
```

### Testing Themes

1. Open the theme switcher with `Ctrl+T`
2. Navigate through themes to see live previews
3. Apply theme to test with actual UI
4. Navigate to different screens to verify compatibility
5. Test with real data to ensure readability

## Troubleshooting

### Theme not loading
- Check that theme name is spelled correctly in config
- Verify the theme exists in `THEMES` dictionary
- Check logs for error messages

### Colors look wrong
- Verify your terminal supports true color (24-bit color)
- Try the "textual-ansi" theme for basic terminal compatibility
- Check terminal color settings

### Theme not persisting
- Ensure config file is writable
- Check that `ui.theme` field is being saved
- Verify config file path is correct

## Design Philosophy

The theme system follows these principles:

1. **Flexibility**: Easy to switch between themes
2. **Consistency**: All widgets use the same color scheme
3. **Accessibility**: High contrast and readable by default
4. **Extensibility**: Easy to add custom themes
5. **Performance**: Theme switching is instant with no lag

## References

- [Textual Documentation](https://textual.textualize.io/)
- [TCSS Styling Guide](https://textual.textualize.io/guide/CSS/)
- [Nord Theme](https://www.nordtheme.com/)
- [Catppuccin Theme](https://github.com/catppuccin/catppuccin)
- [Dracula Theme](https://draculatheme.com/)
- [Gruvbox Theme](https://github.com/morhetz/gruvbox)
