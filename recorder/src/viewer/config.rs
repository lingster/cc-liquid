//! User-facing viewer configuration, loaded from a YAML file.
//!
//! Kept deliberately egui-free (like the rest of the `viewer` model): colours
//! are stored as plain `[r, g, b]` byte triples and converted to the UI's
//! colour type in the binary. Every field has a sensible default, so a missing
//! or partial config file degrades gracefully rather than erroring.

use std::path::Path;

use serde::Deserialize;

/// Top-level viewer config. Currently only the price chart is configurable;
/// new sections can be added as further `#[serde(default)]` fields.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewerConfig {
    pub price_chart: PriceChartConfig,
}

/// Colours for the price-movement chart, as `[r, g, b]` (0–255) triples.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PriceChartConfig {
    /// The full price history line spanning the whole loaded time range.
    pub full_color: [u8; 3],
    /// The overlay drawn up to the current playback position, so users can see
    /// where in time the replay has reached.
    pub elapsed_color: [u8; 3],
}

impl Default for PriceChartConfig {
    fn default() -> Self {
        Self {
            // Light grey for the full series.
            full_color: [180, 180, 180],
            // Light blue for the elapsed/replay overlay.
            elapsed_color: [120, 190, 255],
        }
    }
}

impl ViewerConfig {
    /// Load config from `path`. Returns the parsed config on success, or the
    /// built-in defaults if the file is absent or unreadable. Returns an `Err`
    /// only when the file exists but contains invalid YAML, so callers can warn
    /// about a genuinely broken config rather than silently ignoring it.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => serde_yaml::from_str(&text)
                .map_err(|e| format!("invalid YAML in {}: {e}", path.display())),
            // Missing file is not an error — fall back to defaults.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("could not read {}: {e}", path.display())),
        }
    }

    /// Convenience: load, but fall back to defaults (logging to stderr) on any
    /// error so the UI always has a usable config.
    pub fn load_or_default(path: impl AsRef<Path>) -> Self {
        match Self::load(&path) {
            Ok(cfg) => cfg,
            Err(msg) => {
                eprintln!("hl-viewer: {msg}; using default colours");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_light_grey_and_light_blue() {
        let cfg = ViewerConfig::default();
        assert_eq!(cfg.price_chart.full_color, [180, 180, 180]);
        assert_eq!(cfg.price_chart.elapsed_color, [120, 190, 255]);
    }

    #[test]
    fn parses_explicit_colors() {
        let yaml = "
price_chart:
  full_color: [10, 20, 30]
  elapsed_color: [200, 210, 220]
";
        let cfg: ViewerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.price_chart.full_color, [10, 20, 30]);
        assert_eq!(cfg.price_chart.elapsed_color, [200, 210, 220]);
    }

    #[test]
    fn partial_config_fills_missing_with_defaults() {
        // Only override one colour; the other must keep its default.
        let yaml = "
price_chart:
  full_color: [1, 2, 3]
";
        let cfg: ViewerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.price_chart.full_color, [1, 2, 3]);
        assert_eq!(cfg.price_chart.elapsed_color, [120, 190, 255]);
    }

    #[test]
    fn missing_file_yields_defaults_not_error() {
        let cfg = ViewerConfig::load("/no/such/hl-viewer-config.yaml").unwrap();
        assert_eq!(cfg, ViewerConfig::default());
    }

    #[test]
    fn invalid_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.yaml");
        std::fs::write(&path, "price_chart: [not, a, map]").unwrap();
        assert!(ViewerConfig::load(&path).is_err());
        // load_or_default swallows it back to defaults.
        assert_eq!(ViewerConfig::load_or_default(&path), ViewerConfig::default());
    }
}
