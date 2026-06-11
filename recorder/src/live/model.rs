//! ONNX model loading + inference (the only module that touches `ort`).
//!
//! The harness depends on the [`Predictor`] trait, not on ONNX Runtime, so
//! orchestration tests run with a stub model (Dependency Inversion).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// Anything that maps a normalized window to per-head (down, stationary, up)
/// probabilities. Single-head models return one vector; multi-head models
/// return one per head, in the sidecar's (sorted) `horizons` order.
pub trait Predictor {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<Vec<[f32; 3]>>;
}

/// Subset of the exporter's sidecar JSON (`<model>.onnx.json`) we need.
#[derive(Debug, Clone, Deserialize)]
pub struct Sidecar {
    pub coin: String,
    pub tick: f64,
    pub grid: SidecarGrid,
    /// Per-coin calibrated label thresholds (ticks), one per head, aligned
    /// with the head grid. Non-empty = per-coin calibrated labels: the live
    /// coin's entry overrides `horizon_thresholds` / `threshold_ticks`.
    #[serde(default)]
    pub coin_thresholds: HashMap<String, Vec<f64>>,
    /// Per-head softmax temperatures (`orderbooker calibrate`), aligned with
    /// the head grid. Empty = uncalibrated (T = 1 per head).
    #[serde(default)]
    pub temperatures: Vec<f64>,
    /// Primary-head temperature as a scalar (single-head convenience; the
    /// exporter writes `temperatures[0]` or 1.0 here). `temperatures` wins
    /// when non-empty. Absent = 1.0 (uncalibrated).
    #[serde(default = "default_temperature")]
    pub temperature: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarGrid {
    pub depth: usize,
    pub window: usize,
    pub horizon: u32,
    pub threshold_ticks: f64,
    /// Multi-head label horizons (sorted ascending, unique); empty = single
    /// head at `horizon`. When set, `horizon` equals `max(horizons)`.
    #[serde(default)]
    pub horizons: Vec<u32>,
    /// Per-head up/down thresholds in ticks, paired 1:1 with `horizons`.
    #[serde(default)]
    pub horizon_thresholds: Vec<f64>,
    pub norm_lookback: usize,
    /// `zscore` or `rankgauss`; older exports omit it (z-score era).
    #[serde(default = "default_norm")]
    pub norm: String,
    /// Extra input channel: signed traded volume. NOT ported to hl-live —
    /// rejected at load. Older exports omit it.
    #[serde(default)]
    pub trade_flow: bool,
    /// Extra input channel: book-delta OFI (quote flow). Older exports omit it.
    #[serde(default)]
    pub quote_flow: bool,
}

fn default_norm() -> String {
    "zscore".to_string()
}

fn default_temperature() -> f64 {
    1.0
}

impl SidecarGrid {
    /// Model input channels: bid + ask volumes plus the quote-flow plane.
    pub fn channels(&self) -> usize {
        if self.quote_flow {
            3
        } else {
            2
        }
    }
}

impl Sidecar {
    /// True when the model carries several prediction heads.
    pub fn is_multi_head(&self) -> bool {
        !self.grid.horizons.is_empty()
    }

    /// Number of prediction heads (1 for single-head models).
    pub fn n_heads(&self) -> usize {
        self.grid.horizons.len().max(1)
    }

    /// Label horizon per head, ascending (single-head: the trained horizon).
    pub fn head_horizons(&self) -> Vec<u32> {
        if self.is_multi_head() {
            self.grid.horizons.clone()
        } else {
            vec![self.grid.horizon]
        }
    }

    /// Labelling threshold (ticks) per head, aligned with `head_horizons`.
    /// The live coin's calibrated `coin_thresholds` entry wins when present;
    /// otherwise the grid's `horizon_thresholds` (multi-head) or
    /// `threshold_ticks` (single-head).
    pub fn head_thresholds(&self) -> Vec<f64> {
        if let Some(per_coin) = self.coin_thresholds.get(&self.coin) {
            return per_coin.clone();
        }
        if self.is_multi_head() {
            self.grid.horizon_thresholds.clone()
        } else {
            vec![self.grid.threshold_ticks]
        }
    }

    /// Softmax temperature per head: `temperatures` when calibrated, else the
    /// scalar `temperature` (1.0 in uncalibrated exports) for every head.
    pub fn head_temperatures(&self) -> Vec<f64> {
        if self.temperatures.is_empty() {
            vec![self.temperature; self.n_heads()]
        } else {
            self.temperatures.clone()
        }
    }

    /// Load `<model>.onnx.json` written by `orderbooker export`.
    pub fn load_for_model(model_path: &Path) -> anyhow::Result<Self> {
        let mut sidecar_path = model_path.as_os_str().to_owned();
        sidecar_path.push(".json");
        let sidecar_path = Path::new(&sidecar_path);
        let text = std::fs::read_to_string(sidecar_path).map_err(|e| {
            anyhow::anyhow!(
                "cannot read sidecar {} ({e}); re-export with `orderbooker export`",
                sidecar_path.display()
            )
        })?;
        let sidecar: Self = serde_json::from_str(&text)?;
        sidecar.validate()?;
        Ok(sidecar)
    }

    /// Reject configurations this harness cannot reproduce.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.grid.trade_flow,
            "this model requires the trade-flow input channel, which is not \
             ported to hl-live; re-train/export without trade flow"
        );
        anyhow::ensure!(
            self.temperature.is_finite() && self.temperature > 0.0,
            "sidecar temperature must be a positive finite number, got {}",
            self.temperature
        );
        if self.grid.horizons.is_empty() {
            anyhow::ensure!(
                self.grid.horizon_thresholds.is_empty(),
                "sidecar has horizon_thresholds without horizons"
            );
        } else {
            anyhow::ensure!(
                self.grid.horizons.windows(2).all(|w| w[0] < w[1]) && self.grid.horizons[0] >= 1,
                "sidecar horizons must be positive, sorted and unique, got {:?}",
                self.grid.horizons
            );
            anyhow::ensure!(
                self.grid.horizon_thresholds.len() == self.grid.horizons.len(),
                "sidecar horizon_thresholds ({}) must pair 1:1 with horizons ({})",
                self.grid.horizon_thresholds.len(),
                self.grid.horizons.len()
            );
            anyhow::ensure!(
                self.grid.horizon == *self.grid.horizons.last().unwrap(),
                "sidecar horizon ({}) must equal max(horizons) ({:?})",
                self.grid.horizon,
                self.grid.horizons
            );
        }
        anyhow::ensure!(
            self.grid
                .horizon_thresholds
                .iter()
                .all(|t| t.is_finite() && *t >= 0.0),
            "sidecar horizon_thresholds must be finite and non-negative, got {:?}",
            self.grid.horizon_thresholds
        );
        anyhow::ensure!(
            self.temperatures.is_empty() || self.temperatures.len() == self.n_heads(),
            "sidecar has {} temperatures for {} heads",
            self.temperatures.len(),
            self.n_heads()
        );
        anyhow::ensure!(
            self.temperatures.iter().all(|t| t.is_finite() && *t > 0.0),
            "sidecar temperatures must be positive finite numbers, got {:?}",
            self.temperatures
        );
        for (coin, thresholds) in &self.coin_thresholds {
            anyhow::ensure!(
                thresholds.len() == self.n_heads(),
                "sidecar coin_thresholds[{coin}] has {} entries for {} heads",
                thresholds.len(),
                self.n_heads()
            );
            anyhow::ensure!(
                thresholds.iter().all(|t| t.is_finite() && *t >= 0.0),
                "sidecar coin_thresholds[{coin}] must be finite and non-negative, got {thresholds:?}"
            );
        }
        Ok(())
    }
}

/// An exported `orderbooker` checkpoint running on ONNX Runtime.
pub struct OnnxModel {
    session: ort::session::Session,
    channels: usize,
    window: usize,
    depth: usize,
    /// One softmax temperature per head (length = head count).
    temperatures: Vec<f32>,
}

impl OnnxModel {
    pub fn load(model_path: &Path, meta: &Sidecar) -> anyhow::Result<Self> {
        meta.validate()?;
        let session = ort::session::Session::builder()?.commit_from_file(model_path)?;
        Ok(Self {
            session,
            channels: meta.grid.channels(),
            window: meta.grid.window,
            depth: meta.grid.depth,
            temperatures: meta.head_temperatures().iter().map(|t| *t as f32).collect(),
        })
    }
}

impl Predictor for OnnxModel {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<Vec<[f32; 3]>> {
        let expected = self.channels * self.window * self.depth;
        anyhow::ensure!(
            window.len() == expected,
            "window has {} values, model expects {expected}",
            window.len()
        );
        let input = ort::value::Tensor::from_array((
            [1usize, self.channels, self.window, self.depth],
            window.to_vec(),
        ))?;
        let outputs = self.session.run(ort::inputs!["input" => input])?;
        let (_, logits) = outputs["logits"].try_extract_tensor::<f32>()?;
        per_head_probs(logits, &self.temperatures)
    }
}

/// Slice a batch-1 logits buffer — `(1, 3)` single-head or `(1, H, 3)`
/// multi-head, row-major — into per-head probabilities, applying each head's
/// calibration temperature before its softmax.
pub fn per_head_probs(logits: &[f32], temperatures: &[f32]) -> anyhow::Result<Vec<[f32; 3]>> {
    anyhow::ensure!(
        logits.len() == 3 * temperatures.len(),
        "expected {} logits for {} heads, got {}",
        3 * temperatures.len(),
        temperatures.len(),
        logits.len()
    );
    Ok(logits
        .chunks_exact(3)
        .zip(temperatures)
        .map(|(head, &t)| calibrated_softmax([head[0], head[1], head[2]], t))
        .collect())
}

/// Temperature-scaled softmax: logits / T, then softmax. T = 1 is the plain,
/// uncalibrated path.
pub fn calibrated_softmax(logits: [f32; 3], temperature: f32) -> [f32; 3] {
    softmax3([
        logits[0] / temperature,
        logits[1] / temperature,
        logits[2] / temperature,
    ])
}

pub fn softmax3(logits: [f32; 3]) -> [f32; 3] {
    let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exp = [
        (logits[0] - max).exp(),
        (logits[1] - max).exp(),
        (logits[2] - max).exp(),
    ];
    let sum: f32 = exp.iter().sum();
    [exp[0] / sum, exp[1] / sum, exp[2] / sum]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_sums_to_one_and_orders() {
        let p = softmax3([1.0, 2.0, 3.0]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(p[2] > p[1] && p[1] > p[0]);
    }

    #[test]
    fn temperature_one_is_plain_softmax_and_higher_flattens() {
        let logits = [1.0, 2.0, 3.0];
        assert_eq!(calibrated_softmax(logits, 1.0), softmax3(logits));
        let hot = calibrated_softmax(logits, 4.0);
        let cold = softmax3(logits);
        assert!(hot[2] < cold[2] && hot[0] > cold[0]); // flatter distribution
        assert!((hot.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }

    const BASE_GRID: &str = r#""grid": {"depth": 20, "window": 20, "horizon": 5,
        "threshold_ticks": 0.5, "norm_lookback": 500"#;

    #[test]
    fn sidecar_defaults_are_backward_compatible() {
        // Pre-flow, pre-temperature sidecar: flags false, temperature 1.0.
        let json = format!(r#"{{"coin": "BTC", "tick": 1.0, {BASE_GRID}}}}}"#);
        let s: Sidecar = serde_json::from_str(&json).unwrap();
        assert!(!s.grid.trade_flow && !s.grid.quote_flow);
        assert_eq!(s.grid.norm, "zscore");
        assert_eq!(s.temperature, 1.0);
        assert_eq!(s.grid.channels(), 2);
        s.validate().unwrap();
    }

    #[test]
    fn sidecar_parses_flow_flags_and_temperature() {
        let json = format!(
            r#"{{"coin": "BTC", "tick": 1.0, "temperature": 1.7,
                {BASE_GRID}, "trade_flow": false, "quote_flow": true}}}}"#
        );
        let s: Sidecar = serde_json::from_str(&json).unwrap();
        assert!(s.grid.quote_flow && !s.grid.trade_flow);
        assert_eq!(s.temperature, 1.7);
        assert_eq!(s.grid.channels(), 3);
        s.validate().unwrap();
    }

    #[test]
    fn sidecar_rejects_trade_flow_and_bad_temperature() {
        let json = format!(r#"{{"coin": "BTC", "tick": 1.0, {BASE_GRID}, "trade_flow": true}}}}"#);
        let s: Sidecar = serde_json::from_str(&json).unwrap();
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("trade-flow"), "unhelpful error: {err}");

        let json = format!(r#"{{"coin": "BTC", "tick": 1.0, "temperature": 0.0, {BASE_GRID}}}}}"#);
        let s: Sidecar = serde_json::from_str(&json).unwrap();
        assert!(s.validate().is_err());
    }

    #[test]
    fn load_for_model_rejects_trade_flow_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("m.onnx");
        let json = format!(r#"{{"coin": "BTC", "tick": 1.0, {BASE_GRID}, "trade_flow": true}}}}"#);
        std::fs::write(dir.path().join("m.onnx.json"), json).unwrap();
        let err = Sidecar::load_for_model(&model).unwrap_err().to_string();
        assert!(err.contains("trade-flow"), "unhelpful error: {err}");
    }

    /// Grid with three heads at horizons 5/10/20; `horizon` = max.
    const MH_GRID: &str = r#""grid": {"depth": 20, "window": 20, "horizon": 20,
        "threshold_ticks": 0.5, "norm_lookback": 500,
        "horizons": [5, 10, 20], "horizon_thresholds": [0.5, 2.5, 4.75]"#;

    fn parse(json: &str) -> Sidecar {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn single_head_defaults_for_multi_head_fields() {
        let s = parse(&format!(r#"{{"coin": "BTC", "tick": 1.0, {BASE_GRID}}}}}"#));
        assert!(!s.is_multi_head());
        assert_eq!(s.n_heads(), 1);
        assert_eq!(s.head_horizons(), vec![5]);
        assert_eq!(s.head_thresholds(), vec![0.5]);
        assert_eq!(s.head_temperatures(), vec![1.0]);
        assert!(s.coin_thresholds.is_empty() && s.temperatures.is_empty());
        s.validate().unwrap();
    }

    #[test]
    fn multi_head_sidecar_parses_and_validates() {
        let s = parse(&format!(
            r#"{{"coin": "BTC", "tick": 1.0, {MH_GRID}}},
                "temperatures": [1.1, 0.9, 1.3],
                "coin_thresholds": {{"BTC": [0.0, 1.0, 5.0], "ETH": [0.0, 1.0, 2.0]}}}}"#
        ));
        s.validate().unwrap();
        assert!(s.is_multi_head());
        assert_eq!(s.n_heads(), 3);
        assert_eq!(s.head_horizons(), vec![5, 10, 20]);
        // The live coin (BTC) is calibrated: per-coin thresholds win.
        assert_eq!(s.head_thresholds(), vec![0.0, 1.0, 5.0]);
        assert_eq!(s.head_temperatures(), vec![1.1, 0.9, 1.3]);
    }

    #[test]
    fn multi_head_without_coin_entry_uses_horizon_thresholds() {
        let s = parse(&format!(
            r#"{{"coin": "SOL", "tick": 0.01, {MH_GRID}}},
                "coin_thresholds": {{"BTC": [0.0, 1.0, 5.0]}}}}"#
        ));
        s.validate().unwrap();
        assert_eq!(s.head_thresholds(), vec![0.5, 2.5, 4.75]);
        // Uncalibrated temperatures: 1.0 per head.
        assert_eq!(s.head_temperatures(), vec![1.0; 3]);
    }

    #[test]
    fn single_head_prefers_temperatures_over_scalar() {
        let s = parse(&format!(
            r#"{{"coin": "BTC", "tick": 1.0, "temperature": 1.7,
                "temperatures": [2.5], {BASE_GRID}}}}}"#
        ));
        s.validate().unwrap();
        assert_eq!(s.head_temperatures(), vec![2.5]);
        // Empty `temperatures` falls back to the scalar (legacy single-head).
        let s = parse(&format!(
            r#"{{"coin": "BTC", "tick": 1.0, "temperature": 1.7, {BASE_GRID}}}}}"#
        ));
        assert_eq!(s.head_temperatures(), vec![1.7]);
    }

    #[test]
    fn multi_head_pairing_errors_are_rejected() {
        let expect_err = |json: &str, needle: &str| {
            let err = parse(json).validate().unwrap_err().to_string();
            assert!(err.contains(needle), "expected '{needle}' in: {err}");
        };
        // Thresholds not paired 1:1 with horizons.
        expect_err(
            r#"{"coin": "BTC", "tick": 1.0, "grid": {"depth": 20, "window": 20,
                "horizon": 20, "threshold_ticks": 0.5, "norm_lookback": 500,
                "horizons": [5, 10, 20], "horizon_thresholds": [0.5, 2.5]}}"#,
            "pair 1:1",
        );
        // horizon != max(horizons).
        expect_err(
            r#"{"coin": "BTC", "tick": 1.0, "grid": {"depth": 20, "window": 20,
                "horizon": 10, "threshold_ticks": 0.5, "norm_lookback": 500,
                "horizons": [5, 10, 20], "horizon_thresholds": [0.5, 2.5, 4.75]}}"#,
            "max(horizons)",
        );
        // Unsorted horizons.
        expect_err(
            r#"{"coin": "BTC", "tick": 1.0, "grid": {"depth": 20, "window": 20,
                "horizon": 20, "threshold_ticks": 0.5, "norm_lookback": 500,
                "horizons": [10, 5, 20], "horizon_thresholds": [0.5, 2.5, 4.75]}}"#,
            "sorted",
        );
        // Thresholds without horizons.
        expect_err(
            r#"{"coin": "BTC", "tick": 1.0, "grid": {"depth": 20, "window": 20,
                "horizon": 20, "threshold_ticks": 0.5, "norm_lookback": 500,
                "horizon_thresholds": [0.5]}}"#,
            "without horizons",
        );
        // Temperatures not aligned with the heads.
        expect_err(
            &format!(r#"{{"coin": "BTC", "tick": 1.0, {MH_GRID}}}, "temperatures": [1.1]}}"#),
            "temperatures",
        );
        // Non-positive per-head temperature.
        expect_err(
            &format!(
                r#"{{"coin": "BTC", "tick": 1.0, {MH_GRID}}},
                    "temperatures": [1.1, 0.0, 1.3]}}"#
            ),
            "positive",
        );
        // Per-coin threshold list with the wrong head count.
        expect_err(
            &format!(
                r#"{{"coin": "BTC", "tick": 1.0, {MH_GRID}}},
                    "coin_thresholds": {{"BTC": [0.0, 1.0]}}}}"#
            ),
            "coin_thresholds",
        );
        // Negative per-head threshold.
        expect_err(
            r#"{"coin": "BTC", "tick": 1.0, "grid": {"depth": 20, "window": 20,
                "horizon": 20, "threshold_ticks": 0.5, "norm_lookback": 500,
                "horizons": [5, 10, 20], "horizon_thresholds": [0.5, -2.5, 4.75]}}"#,
            "non-negative",
        );
    }

    #[test]
    fn per_head_probs_slices_and_applies_each_temperature() {
        // (1, 3, 3) logits row-major: head h occupies logits[3h .. 3h+3].
        let logits = [1.0f32, 2.0, 3.0, 3.0, 2.0, 1.0, 0.0, 0.0, 0.0];
        let temps = [1.0f32, 2.0, 4.0];
        let probs = per_head_probs(&logits, &temps).unwrap();
        assert_eq!(probs.len(), 3);
        assert_eq!(probs[0], calibrated_softmax([1.0, 2.0, 3.0], 1.0));
        assert_eq!(probs[1], calibrated_softmax([3.0, 2.0, 1.0], 2.0));
        assert_eq!(probs[2], [1.0 / 3.0; 3]);
        for p in &probs {
            assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
        // Head-count mismatch is a hard error, not a truncation.
        assert!(per_head_probs(&logits[..6], &temps).is_err());
    }
}
