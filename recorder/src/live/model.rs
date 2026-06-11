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
}

fn default_norm() -> String {
    "zscore".to_string()
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
        Ok(serde_json::from_str(&text)?)
    }
}

/// An exported `orderbooker` checkpoint running on ONNX Runtime.
pub struct OnnxModel {
    session: ort::session::Session,
    window: usize,
    depth: usize,
}

impl OnnxModel {
    pub fn load(model_path: &Path, meta: &Sidecar) -> anyhow::Result<Self> {
        let session = ort::session::Session::builder()?.commit_from_file(model_path)?;
        Ok(Self {
            session,
            window: meta.grid.window,
            depth: meta.grid.depth,
        })
    }
}

impl Predictor for OnnxModel {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<[f32; 3]> {
        let expected = 2 * self.window * self.depth;
        anyhow::ensure!(
            window.len() == expected,
            "window has {} values, model expects {expected}",
            window.len()
        );
        let input = ort::value::Tensor::from_array((
            [1usize, 2, self.window, self.depth],
            window.to_vec(),
        ))?;
        let outputs = self.session.run(ort::inputs!["input" => input])?;
        let (_, logits) = outputs["logits"].try_extract_tensor::<f32>()?;
        anyhow::ensure!(logits.len() == 3, "expected 3 logits, got {}", logits.len());
        Ok(softmax3([logits[0], logits[1], logits[2]]))
    }
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
}
