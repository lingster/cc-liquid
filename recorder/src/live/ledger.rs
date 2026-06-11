//! Prediction ledger: issue multi-horizon predictions now, score them later.
//! Port of `orderbooker.live.ledger` with identical labelling semantics.

use std::collections::HashMap;

pub const LABEL_DOWN: u32 = 0;
pub const LABEL_STATIONARY: u32 = 1;
pub const LABEL_UP: u32 = 2;

/// Same rule as the Python `dataset.make_labels` / `direction_label`.
pub fn direction_label(mv: f64, tick: f64, threshold_ticks: f64) -> u32 {
    let threshold = threshold_ticks * tick;
    if mv > threshold {
        LABEL_UP
    } else if mv < -threshold {
        LABEL_DOWN
    } else {
        LABEL_STATIONARY
    }
}

/// One prediction head: probabilities resolved at `issue_idx + horizon` and
/// labelled with `threshold_ticks`. Single-head models scored at several
/// user horizons are just several heads sharing one probability vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Head {
    pub horizon: u32,
    pub threshold_ticks: f64,
}

/// One scored prediction (a row in the results parquet).
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub horizon: u32,
    pub issue_idx: u64,
    pub target_idx: u64,
    pub ts_issue_ms: i64,
    pub ts_target_ms: i64,
    pub probs: [f32; 3], // down, stationary, up
    pub predicted: u32,
    pub mid_issue: f64,
    pub mid_target: f64,
    pub move_ticks: f64,
    pub actual: u32,
    pub correct: bool,
    /// Labelling threshold (ticks) this row was scored with.
    pub threshold_ticks: f64,
}

struct Pending {
    horizon: u32,
    threshold_ticks: f64,
    issue_idx: u64,
    ts_issue_ms: i64,
    mid_issue: f64,
    probs: [f32; 3],
}

pub struct PredictionLedger {
    heads: Vec<Head>,
    tick: f64,
    pending: HashMap<u64, Vec<Pending>>,
    pub records: Vec<Resolved>,
}

impl PredictionLedger {
    /// All horizons share one labelling threshold (the single-head layout).
    pub fn new(horizons: Vec<u32>, tick: f64, threshold_ticks: f64) -> anyhow::Result<Self> {
        Self::with_heads(
            horizons
                .into_iter()
                .map(|horizon| Head {
                    horizon,
                    threshold_ticks,
                })
                .collect(),
            tick,
        )
    }

    /// Per-head (horizon, threshold) pairs — the multi-head layout.
    pub fn with_heads(heads: Vec<Head>, tick: f64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !heads.is_empty() && heads.iter().all(|h| h.horizon >= 1),
            "head horizons must be non-empty and positive, got {:?}",
            heads.iter().map(|h| h.horizon).collect::<Vec<_>>()
        );
        anyhow::ensure!(
            heads
                .iter()
                .all(|h| h.threshold_ticks.is_finite() && h.threshold_ticks >= 0.0),
            "head thresholds must be finite and non-negative, got {:?}",
            heads.iter().map(|h| h.threshold_ticks).collect::<Vec<_>>()
        );
        let mut heads = heads;
        heads.sort_by_key(|h| h.horizon);
        Ok(Self {
            heads,
            tick,
            pending: HashMap::new(),
            records: Vec::new(),
        })
    }

    /// Register one model output against every configured head (the
    /// single-output path: every head scores the same probabilities).
    pub fn issue(&mut self, idx: u64, ts_event_ms: i64, mid: f64, probs: [f32; 3]) {
        self.issue_per_head(idx, ts_event_ms, mid, &[probs])
            .expect("a single probability vector broadcasts to any head count");
    }

    /// Register per-head model outputs: `probs[i]` is scored by head `i`
    /// (ledger heads are sorted by horizon, matching the sidecar's sorted
    /// `horizons`). A single vector broadcasts to every head.
    pub fn issue_per_head(
        &mut self,
        idx: u64,
        ts_event_ms: i64,
        mid: f64,
        probs: &[[f32; 3]],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            probs.len() == 1 || probs.len() == self.heads.len(),
            "model emitted {} probability vectors for {} ledger heads",
            probs.len(),
            self.heads.len()
        );
        for (i, head) in self.heads.iter().enumerate() {
            let head_probs = if probs.len() == 1 { probs[0] } else { probs[i] };
            self.pending
                .entry(idx + head.horizon as u64)
                .or_default()
                .push(Pending {
                    horizon: head.horizon,
                    threshold_ticks: head.threshold_ticks,
                    issue_idx: idx,
                    ts_issue_ms: ts_event_ms,
                    mid_issue: mid,
                    probs: head_probs,
                });
        }
        Ok(())
    }

    /// Score every prediction whose target snapshot is `idx`; returns how many.
    pub fn resolve(&mut self, idx: u64, ts_event_ms: i64, mid: f64) -> usize {
        let Some(pending) = self.pending.remove(&idx) else {
            return 0;
        };
        let count = pending.len();
        for p in pending {
            let mv = mid - p.mid_issue;
            let actual = direction_label(mv, self.tick, p.threshold_ticks);
            let predicted = argmax3(&p.probs);
            self.records.push(Resolved {
                horizon: p.horizon,
                issue_idx: p.issue_idx,
                target_idx: idx,
                ts_issue_ms: p.ts_issue_ms,
                ts_target_ms: ts_event_ms,
                probs: p.probs,
                predicted,
                mid_issue: p.mid_issue,
                mid_target: mid,
                move_ticks: mv / self.tick,
                actual,
                correct: predicted == actual,
                threshold_ticks: p.threshold_ticks,
            });
        }
        count
    }

    pub fn pending_count(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }
}

fn argmax3(probs: &[f32; 3]) -> u32 {
    let mut best = 0usize;
    for i in 1..3 {
        if probs[i] > probs[best] {
            best = i;
        }
    }
    best as u32
}

/// Per-horizon accuracy summary for the end-of-run report.
#[derive(Debug, Clone)]
pub struct HorizonSummary {
    pub horizon: u32,
    pub n: usize,
    pub accuracy: f64,
    /// Share of the most common actual class — the number to beat.
    pub baseline: f64,
    pub up_precision: f64,
    pub up_recall: f64,
    pub down_precision: f64,
    pub down_recall: f64,
}

pub fn summarize(records: &[Resolved]) -> Vec<HorizonSummary> {
    let mut horizons: Vec<u32> = records.iter().map(|r| r.horizon).collect();
    horizons.sort_unstable();
    horizons.dedup();
    horizons
        .into_iter()
        .map(|horizon| {
            let subset: Vec<&Resolved> = records.iter().filter(|r| r.horizon == horizon).collect();
            let n = subset.len();
            let mut confusion = [[0usize; 3]; 3]; // [actual][predicted]
            for r in &subset {
                confusion[r.actual as usize][r.predicted as usize] += 1;
            }
            let class_rate = |class: usize, by_pred: bool| -> f64 {
                let tp = confusion[class][class] as f64;
                let total: usize = (0..3)
                    .map(|i| {
                        if by_pred {
                            confusion[i][class]
                        } else {
                            confusion[class][i]
                        }
                    })
                    .sum();
                if total == 0 {
                    0.0
                } else {
                    tp / total as f64
                }
            };
            let max_actual: usize = (0..3).map(|c| confusion[c].iter().sum()).max().unwrap_or(0);
            HorizonSummary {
                horizon,
                n,
                accuracy: subset.iter().filter(|r| r.correct).count() as f64 / n.max(1) as f64,
                baseline: max_actual as f64 / n.max(1) as f64,
                up_precision: class_rate(LABEL_UP as usize, true),
                up_recall: class_rate(LABEL_UP as usize, false),
                down_precision: class_rate(LABEL_DOWN as usize, true),
                down_recall: class_rate(LABEL_DOWN as usize, false),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_labels_use_strict_threshold() {
        assert_eq!(direction_label(0.6, 1.0, 0.5), LABEL_UP);
        assert_eq!(direction_label(-0.6, 1.0, 0.5), LABEL_DOWN);
        assert_eq!(direction_label(0.5, 1.0, 0.5), LABEL_STATIONARY);
    }

    #[test]
    fn issue_and_resolve_per_horizon() {
        let mut ledger = PredictionLedger::new(vec![2, 4], 1.0, 0.5).unwrap();
        ledger.issue(10, 1000, 100.0, [0.1, 0.2, 0.7]); // calls "up"
        assert_eq!(ledger.resolve(11, 1100, 100.0), 0);
        assert_eq!(ledger.resolve(12, 1200, 102.0), 1); // h=2: +2 -> up, correct
        assert_eq!(ledger.resolve(14, 1400, 99.0), 1); // h=4: -1 -> down, wrong
        assert_eq!(ledger.pending_count(), 0);
        let r2 = &ledger.records[0];
        assert!(r2.correct && r2.actual == LABEL_UP && r2.horizon == 2);
        let r4 = &ledger.records[1];
        assert!(!r4.correct && r4.actual == LABEL_DOWN && r4.move_ticks == -1.0);
    }

    #[test]
    fn summary_reports_accuracy_and_baseline() {
        let mut ledger = PredictionLedger::new(vec![1], 1.0, 0.5).unwrap();
        ledger.issue(0, 0, 100.0, [0.1, 0.2, 0.7]);
        ledger.issue(1, 1, 100.0, [0.7, 0.2, 0.1]);
        ledger.resolve(1, 10, 102.0); // up, predicted up -> correct
        ledger.resolve(2, 20, 102.0); // up (vs mid 100), predicted down -> wrong
        let summary = summarize(&ledger.records);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].n, 2);
        assert!((summary[0].accuracy - 0.5).abs() < 1e-12);
        assert!((summary[0].baseline - 1.0).abs() < 1e-12); // all actuals up
        assert!((summary[0].up_precision - 1.0).abs() < 1e-12);
        assert!((summary[0].up_recall - 0.5).abs() < 1e-12);
    }

    #[test]
    fn empty_horizons_rejected() {
        assert!(PredictionLedger::new(vec![], 1.0, 0.5).is_err());
        assert!(PredictionLedger::new(vec![0], 1.0, 0.5).is_err());
    }

    #[test]
    fn single_threshold_constructor_stamps_every_record() {
        let mut ledger = PredictionLedger::new(vec![1], 1.0, 0.75).unwrap();
        ledger.issue(0, 0, 100.0, [0.1, 0.2, 0.7]);
        ledger.resolve(1, 10, 101.0);
        assert_eq!(ledger.records[0].threshold_ticks, 0.75);
    }

    #[test]
    fn heads_resolve_at_own_targets_with_own_thresholds() {
        let heads = vec![
            Head {
                horizon: 2,
                threshold_ticks: 0.5,
            },
            Head {
                horizon: 4,
                threshold_ticks: 2.5,
            },
        ];
        let mut ledger = PredictionLedger::with_heads(heads, 1.0).unwrap();
        ledger
            .issue_per_head(10, 1000, 100.0, &[[0.1, 0.2, 0.7], [0.7, 0.2, 0.1]])
            .unwrap();
        assert_eq!(ledger.pending_count(), 2);
        assert_eq!(ledger.resolve(12, 1200, 102.0), 1); // head 0 only
        assert_eq!(ledger.resolve(14, 1400, 102.0), 1); // head 1 only
        let r0 = &ledger.records[0];
        // +2 ticks > 0.5 -> up; head 0 carried its own "up" probabilities.
        assert_eq!(r0.horizon, 2);
        assert_eq!(r0.threshold_ticks, 0.5);
        assert_eq!(r0.probs, [0.1, 0.2, 0.7]);
        assert_eq!(r0.actual, LABEL_UP);
        assert!(r0.correct);
        let r1 = &ledger.records[1];
        // Same +2 tick move is STATIONARY under head 1's 2.5-tick threshold.
        assert_eq!(r1.horizon, 4);
        assert_eq!(r1.threshold_ticks, 2.5);
        assert_eq!(r1.probs, [0.7, 0.2, 0.1]);
        assert_eq!(r1.actual, LABEL_STATIONARY);
        assert!(!r1.correct); // it predicted down
    }

    #[test]
    fn issue_per_head_broadcasts_one_vector_and_rejects_mismatch() {
        let heads = vec![
            Head {
                horizon: 1,
                threshold_ticks: 0.5,
            },
            Head {
                horizon: 2,
                threshold_ticks: 0.5,
            },
        ];
        let mut ledger = PredictionLedger::with_heads(heads, 1.0).unwrap();
        ledger
            .issue_per_head(0, 0, 100.0, &[[0.1, 0.2, 0.7]])
            .unwrap();
        ledger.resolve(1, 10, 102.0);
        ledger.resolve(2, 20, 102.0);
        assert!(ledger.records.iter().all(|r| r.probs == [0.1, 0.2, 0.7]));
        assert!(ledger
            .issue_per_head(3, 30, 100.0, &[[0.1, 0.2, 0.7]; 3])
            .is_err());
    }

    #[test]
    fn with_heads_validates_horizons_and_thresholds() {
        let head = |horizon, threshold_ticks| Head {
            horizon,
            threshold_ticks,
        };
        assert!(PredictionLedger::with_heads(vec![], 1.0).is_err());
        assert!(PredictionLedger::with_heads(vec![head(0, 0.5)], 1.0).is_err());
        assert!(PredictionLedger::with_heads(vec![head(1, f64::NAN)], 1.0).is_err());
        assert!(PredictionLedger::with_heads(vec![head(1, -0.5)], 1.0).is_err());
        // Zero threshold is legal (calibrated BTC head 0 uses it).
        assert!(PredictionLedger::with_heads(vec![head(1, 0.0)], 1.0).is_ok());
    }
}
