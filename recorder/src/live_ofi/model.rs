//! Sidecar metadata + ONNX inference for deepofi regression exports.

use std::path::Path;

use serde::Deserialize;

/// Anything that maps a normalized window to per-horizon return forecasts.
pub trait Regressor {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<Vec<f32>>;
}

/// `<model>.onnx.json` written by `deepofi export`.
#[derive(Debug, Clone, Deserialize)]
pub struct OfiSidecar {
    pub coin: String,
    pub tick: f64,
    #[serde(default)]
    pub model_name: String,
    pub features: SidecarFeatures,
    #[serde(default)]
    pub objective: SidecarObjective,
}

/// Head type: the paper's multi-horizon regression (default for older
/// exports) or deepLOB's 3-class direction at one horizon.
#[derive(Debug, Clone, Deserialize)]
pub struct SidecarObjective {
    pub kind: String,
    #[serde(default = "default_cls_horizon")]
    pub cls_horizon: u32,
    #[serde(default = "default_threshold_ticks")]
    pub threshold_ticks: f64,
}

fn default_cls_horizon() -> u32 {
    5
}

fn default_threshold_ticks() -> f64 {
    0.5
}

impl Default for SidecarObjective {
    fn default() -> Self {
        Self {
            kind: "regression".to_string(),
            cls_horizon: default_cls_horizon(),
            threshold_ticks: default_threshold_ticks(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarFeatures {
    pub levels: usize,
    pub window: usize,
    pub horizons: Vec<u32>,
    pub norm_lookback: usize,
    pub include_lag_return: bool,
    pub feature_set: String,
    pub target_scale: f64,
}

impl OfiSidecar {
    pub fn load_for_model(model_path: &Path) -> anyhow::Result<Self> {
        let mut sidecar_path = model_path.as_os_str().to_owned();
        sidecar_path.push(".json");
        let sidecar_path = Path::new(&sidecar_path);
        let text = std::fs::read_to_string(sidecar_path).map_err(|e| {
            anyhow::anyhow!(
                "cannot read sidecar {} ({e}); re-export with `deepofi export`",
                sidecar_path.display()
            )
        })?;
        let sidecar: Self = serde_json::from_str(&text)?;
        anyhow::ensure!(
            matches!(sidecar.features.feature_set.as_str(), "ofi" | "grid"),
            "live harness supports feature_set ofi|grid, sidecar says {:?}",
            sidecar.features.feature_set
        );
        Ok(sidecar)
    }

    pub fn n_features(&self) -> usize {
        let base = match self.features.feature_set.as_str() {
            "grid" => 2 * self.features.levels,
            _ => self.features.levels,
        };
        base + usize::from(self.features.include_lag_return)
    }
}

/// An exported deepofi checkpoint running on ONNX Runtime. Regression heads
/// emit a "returns" vector (one per horizon); classification heads emit
/// three "logits".
pub struct OnnxRegressor {
    session: ort::session::Session,
    window: usize,
    n_features: usize,
    output_name: &'static str,
    n_outputs: usize,
}

impl OnnxRegressor {
    pub fn load(model_path: &Path, meta: &OfiSidecar) -> anyhow::Result<Self> {
        let session = ort::session::Session::builder()?.commit_from_file(model_path)?;
        let classification = meta.objective.kind == "classification";
        Ok(Self {
            session,
            window: meta.features.window,
            n_features: meta.n_features(),
            output_name: if classification { "logits" } else { "returns" },
            n_outputs: if classification {
                3
            } else {
                meta.features.horizons.len()
            },
        })
    }
}

impl Regressor for OnnxRegressor {
    fn predict(&mut self, window: &[f32]) -> anyhow::Result<Vec<f32>> {
        let expected = self.window * self.n_features;
        anyhow::ensure!(
            window.len() == expected,
            "window has {} values, model expects {expected}",
            window.len()
        );
        let input = ort::value::Tensor::from_array((
            [1usize, self.window, self.n_features],
            window.to_vec(),
        ))?;
        let outputs = self.session.run(ort::inputs!["input" => input])?;
        let (_, values) = outputs[self.output_name].try_extract_tensor::<f32>()?;
        anyhow::ensure!(
            values.len() == self.n_outputs,
            "expected {} outputs, got {}",
            self.n_outputs,
            values.len()
        );
        Ok(values.to_vec())
    }
}
