//! Regression ledger: issue multi-horizon return forecasts, score them
//! against realized returns once the target snapshot arrives.

use std::collections::HashMap;

/// One scored forecast (a row in the results parquet). Returns are in the
//  model's target units (bps when target_scale = 1e4).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedReg {
    pub horizon: u32,
    pub issue_idx: u64,
    pub target_idx: u64,
    pub ts_issue_ms: i64,
    pub ts_target_ms: i64,
    pub predicted: f32,
    pub mid_issue: f64,
    pub mid_target: f64,
    pub actual: f64,
}

struct Pending {
    horizon: u32,
    issue_idx: u64,
    ts_issue_ms: i64,
    mid_issue: f64,
    predicted: f32,
}

pub struct RegressionLedger {
    horizons: Vec<u32>,
    target_scale: f64,
    pending: HashMap<u64, Vec<Pending>>,
    pub records: Vec<ResolvedReg>,
}

impl RegressionLedger {
    pub fn new(horizons: Vec<u32>, target_scale: f64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !horizons.is_empty() && horizons.iter().all(|h| *h >= 1),
            "horizons must be non-empty and positive, got {horizons:?}"
        );
        Ok(Self {
            horizons,
            target_scale,
            pending: HashMap::new(),
            records: Vec::new(),
        })
    }

    /// Register one model output vector (positionally matching `horizons`).
    pub fn issue(&mut self, idx: u64, ts_event_ms: i64, mid: f64, preds: &[f32]) {
        debug_assert_eq!(preds.len(), self.horizons.len());
        for (j, &horizon) in self.horizons.iter().enumerate() {
            self.pending
                .entry(idx + horizon as u64)
                .or_default()
                .push(Pending {
                    horizon,
                    issue_idx: idx,
                    ts_issue_ms: ts_event_ms,
                    mid_issue: mid,
                    predicted: preds[j],
                });
        }
    }

    /// Score every forecast whose target row is `idx`; returns how many.
    pub fn resolve(&mut self, idx: u64, ts_event_ms: i64, mid: f64) -> usize {
        let Some(pending) = self.pending.remove(&idx) else {
            return 0;
        };
        let count = pending.len();
        for p in pending {
            let actual = (mid / p.mid_issue - 1.0) * self.target_scale;
            self.records.push(ResolvedReg {
                horizon: p.horizon,
                issue_idx: p.issue_idx,
                target_idx: idx,
                ts_issue_ms: p.ts_issue_ms,
                ts_target_ms: ts_event_ms,
                predicted: p.predicted,
                mid_issue: p.mid_issue,
                mid_target: mid,
                actual,
            });
        }
        count
    }

    pub fn pending_count(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }
}

/// Per-horizon regression summary for the end-of-run report.
#[derive(Debug, Clone)]
pub struct RegSummary {
    pub horizon: u32,
    pub n: usize,
    pub mse: f64,
    /// Out-of-sample R^2 against the zero forecast: 1 - SSE / sum(actual^2).
    pub r2_os: f64,
    /// Sign agreement on realized non-zero moves.
    pub sign_accuracy: f64,
    /// Share of the dominant realized sign (the number to beat).
    pub sign_base_rate: f64,
    pub n_moved: usize,
}

pub fn summarize(records: &[ResolvedReg]) -> Vec<RegSummary> {
    let mut horizons: Vec<u32> = records.iter().map(|r| r.horizon).collect();
    horizons.sort_unstable();
    horizons.dedup();
    horizons
        .into_iter()
        .map(|horizon| {
            let subset: Vec<&ResolvedReg> =
                records.iter().filter(|r| r.horizon == horizon).collect();
            let n = subset.len();
            let sse: f64 = subset
                .iter()
                .map(|r| (r.predicted as f64 - r.actual).powi(2))
                .sum();
            let ss_zero: f64 = subset.iter().map(|r| r.actual * r.actual).sum();
            let moved: Vec<&&ResolvedReg> = subset.iter().filter(|r| r.actual != 0.0).collect();
            let n_moved = moved.len();
            let hits = moved
                .iter()
                .filter(|r| (r.predicted as f64).signum() == r.actual.signum())
                .count();
            let ups = moved.iter().filter(|r| r.actual > 0.0).count();
            let up_rate = if n_moved > 0 {
                ups as f64 / n_moved as f64
            } else {
                0.0
            };
            RegSummary {
                horizon,
                n,
                mse: sse / n.max(1) as f64,
                r2_os: if ss_zero > 0.0 {
                    1.0 - sse / ss_zero
                } else {
                    0.0
                },
                sign_accuracy: if n_moved > 0 {
                    hits as f64 / n_moved as f64
                } else {
                    0.0
                },
                sign_base_rate: up_rate.max(1.0 - up_rate),
                n_moved,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_resolve_scores_realized_returns() {
        let mut ledger = RegressionLedger::new(vec![1, 2], 1e4).unwrap();
        ledger.issue(10, 1000, 100.0, &[0.5, -0.2]);
        assert_eq!(ledger.resolve(11, 1100, 100.01), 1); // +1bp at h=1
        assert_eq!(ledger.resolve(12, 1200, 99.99), 1); // -1bp at h=2
        assert_eq!(ledger.pending_count(), 0);
        let r1 = &ledger.records[0];
        assert_eq!(r1.horizon, 1);
        assert!((r1.actual - 1.0).abs() < 1e-6);
        let r2 = &ledger.records[1];
        assert!((r2.actual + 1.0).abs() < 1e-6);
    }

    #[test]
    fn summary_computes_r2_and_sign_accuracy() {
        let mut ledger = RegressionLedger::new(vec![1], 1e4).unwrap();
        ledger.issue(0, 0, 100.0, &[1.0]); // predicts +1bp
        ledger.issue(1, 1, 100.0, &[-1.0]); // predicts -1bp
        ledger.resolve(1, 10, 100.01); // +1bp: right sign, exact value
        ledger.resolve(2, 20, 100.01); // +1bp: wrong sign
        let s = &summarize(&ledger.records)[0];
        assert_eq!(s.n, 2);
        assert!((s.sign_accuracy - 0.5).abs() < 1e-12);
        assert!((s.sign_base_rate - 1.0).abs() < 1e-12);
        // SSE = 0 + 4, ss_zero = 2 -> r2 = -1.
        assert!((s.r2_os + 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_horizons_rejected() {
        assert!(RegressionLedger::new(vec![], 1e4).is_err());
        assert!(RegressionLedger::new(vec![0], 1e4).is_err());
    }
}
