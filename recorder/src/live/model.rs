//! ONNX model loading + inference (the only module that touches `ort`).
//!
//! The harness depends on the [`Predictor`] trait, not on ONNX Runtime, so
//! orchestration tests run with a stub model (Dependency Inversion).

use std::path::Path;

use serde::Deserialize;

/// Anything that maps a normalized window to (down, stationary, up) probs.
pub trait Predictor {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<[f32; 3]>;
}

/// Subset of the exporter's sidecar JSON (`<model>.onnx.json`) we need.
#[derive(Debug, Clone, Deserialize)]
pub struct Sidecar {
    pub coin: String,
    pub tick: f64,
    pub grid: SidecarGrid,
    /// Probability-calibration temperature: logits are divided by it before
    /// softmax. Absent = 1.0 (uncalibrated).
    #[serde(default = "default_temperature")]
    pub temperature: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarGrid {
    pub depth: usize,
    pub window: usize,
    pub horizon: u32,
    pub threshold_ticks: f64,
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
        Ok(())
    }
}

/// An exported `orderbooker` checkpoint running on ONNX Runtime.
pub struct OnnxModel {
    session: ort::session::Session,
    channels: usize,
    window: usize,
    depth: usize,
    temperature: f32,
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
            temperature: meta.temperature as f32,
        })
    }
}

impl Predictor for OnnxModel {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<[f32; 3]> {
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
        anyhow::ensure!(logits.len() == 3, "expected 3 logits, got {}", logits.len());
        Ok(calibrated_softmax(
            [logits[0], logits[1], logits[2]],
            self.temperature,
        ))
    }
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
}
