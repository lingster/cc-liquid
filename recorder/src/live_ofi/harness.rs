//! Orchestration: snapshots in -> forecasts issued -> scored on arrival.
//!
//! Two harness flavours share the same OFI feature state: the regression
//! harness scores predicted vs realized returns, the classification harness
//! scores deepLOB-style 3-class direction calls (reusing `live::ledger`).

use std::time::Duration;

use tokio::time::Instant;
use tracing::info;

use crate::events::{L2Book, MarketEvent, RecordedEvent};
use crate::live::ledger::PredictionLedger;
use crate::live::model::softmax3;
use crate::live_ofi::ledger::RegressionLedger;
use crate::live_ofi::model::{OfiSidecar, Regressor};
use crate::live_ofi::state::OfiState;
use crate::parser::parse_message;
use crate::source::EventSource;

/// Anything the live/replay drivers can feed snapshots into.
pub trait Steps {
    fn step(&mut self, book: &L2Book) -> anyhow::Result<()>;
    fn snapshots_seen(&self) -> u64;
}

fn build_state(meta: &OfiSidecar) -> anyhow::Result<OfiState> {
    let f = &meta.features;
    Ok(OfiState::with_kind(
        f.levels,
        f.window,
        f.norm_lookback,
        f.target_scale,
        f.include_lag_return,
        crate::live_ofi::state::FeatureKind::parse(&f.feature_set)?,
        meta.tick,
    ))
}

pub struct OfiHarnessStats {
    pub snapshots_seen: u64,
    pub feature_rows: u64,
    pub predictions_issued: u64,
    pub resolved: usize,
    pub unresolved: usize,
}

/// Mutable harness state shared by the live and replay drivers.
pub struct OfiHarness<'a, R: Regressor> {
    state: OfiState,
    pub ledger: RegressionLedger,
    model: &'a mut R,
    coin: String,
    snapshots: u64,
    idx: u64, // feature-row index (one behind snapshots: OFI consumes a transition)
    issued: u64,
}

impl<'a, R: Regressor> OfiHarness<'a, R> {
    pub fn new(meta: &OfiSidecar, model: &'a mut R) -> anyhow::Result<Self> {
        let f = &meta.features;
        Ok(Self {
            state: build_state(meta)?,
            ledger: RegressionLedger::new(f.horizons.clone(), f.target_scale)?,
            model,
            coin: meta.coin.clone(),
            snapshots: 0,
            idx: 0,
            issued: 0,
        })
    }

    pub fn stats(&self) -> OfiHarnessStats {
        OfiHarnessStats {
            snapshots_seen: self.snapshots,
            feature_rows: self.idx,
            predictions_issued: self.issued,
            resolved: self.ledger.records.len(),
            unresolved: self.ledger.pending_count(),
        }
    }
}

impl<R: Regressor> Steps for OfiHarness<'_, R> {
    /// Fold one book snapshot: resolve due forecasts, then issue new ones.
    fn step(&mut self, book: &L2Book) -> anyhow::Result<()> {
        if book.coin != self.coin {
            return Ok(());
        }
        self.snapshots += 1;
        let Some(mid) = self.state.push(book) else {
            return Ok(()); // first snapshot or one-sided book: no feature row
        };
        self.ledger.resolve(self.idx, book.time_ms, mid);
        if self.state.ready() {
            let preds = self.model.predict(&self.state.latest_window()?)?;
            self.ledger.issue(self.idx, book.time_ms, mid, &preds);
            self.issued += 1;
        }
        self.idx += 1;
        Ok(())
    }

    fn snapshots_seen(&self) -> u64 {
        self.snapshots
    }
}

/// deepLOB-objective harness: OFI windows in, 3-class direction calls out,
/// scored with the classification ledger (accuracy vs majority baseline).
pub struct OfiClsHarness<'a, R: Regressor> {
    state: OfiState,
    pub ledger: PredictionLedger,
    model: &'a mut R,
    coin: String,
    snapshots: u64,
    idx: u64,
    issued: u64,
}

impl<'a, R: Regressor> OfiClsHarness<'a, R> {
    pub fn new(meta: &OfiSidecar, model: &'a mut R) -> anyhow::Result<Self> {
        anyhow::ensure!(
            meta.objective.kind == "classification",
            "OfiClsHarness needs a classification sidecar"
        );
        Ok(Self {
            state: build_state(meta)?,
            ledger: PredictionLedger::new(
                vec![meta.objective.cls_horizon],
                meta.tick,
                meta.objective.threshold_ticks,
            )?,
            model,
            coin: meta.coin.clone(),
            snapshots: 0,
            idx: 0,
            issued: 0,
        })
    }

    pub fn stats(&self) -> OfiHarnessStats {
        OfiHarnessStats {
            snapshots_seen: self.snapshots,
            feature_rows: self.idx,
            predictions_issued: self.issued,
            resolved: self.ledger.records.len(),
            unresolved: self.ledger.pending_count(),
        }
    }
}

impl<R: Regressor> Steps for OfiClsHarness<'_, R> {
    fn step(&mut self, book: &L2Book) -> anyhow::Result<()> {
        if book.coin != self.coin {
            return Ok(());
        }
        self.snapshots += 1;
        let Some(mid) = self.state.push(book) else {
            return Ok(());
        };
        self.ledger.resolve(self.idx, book.time_ms, mid);
        if self.state.ready() {
            let logits = self.model.predict(&self.state.latest_window()?)?;
            anyhow::ensure!(logits.len() == 3, "classifier must emit 3 logits");
            let probs = softmax3([logits[0], logits[1], logits[2]]);
            self.ledger.issue(self.idx, book.time_ms, mid, probs);
            self.issued += 1;
        }
        self.idx += 1;
        Ok(())
    }

    fn snapshots_seen(&self) -> u64 {
        self.snapshots
    }
}

/// Drive a harness from a live source until `duration` elapses.
pub async fn run_live<H: Steps>(
    harness: &mut H,
    source: &mut dyn EventSource,
    duration: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            info!(
                "duration reached after {} snapshots",
                harness.snapshots_seen()
            );
            return Ok(());
        }
        let msg = match tokio::time::timeout(remaining, source.next_message()).await {
            Err(_) => {
                info!(
                    "duration reached after {} snapshots",
                    harness.snapshots_seen()
                );
                return Ok(());
            }
            Ok(None) => {
                info!("source ended after {} snapshots", harness.snapshots_seen());
                return Ok(());
            }
            Ok(Some(msg)) => msg?,
        };
        if let Ok(Some(MarketEvent::L2Book(book))) = parse_message(&msg) {
            harness.step(&book)?;
        }
    }
}

/// Drive a harness from recorded events (deterministic replay).
pub fn run_replay<H: Steps>(harness: &mut H, events: &[RecordedEvent]) -> anyhow::Result<()> {
    for event in events {
        if let MarketEvent::L2Book(book) = &event.payload {
            harness.step(book)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Level;
    use crate::live_ofi::model::SidecarFeatures;

    /// Always predicts +1bp at every horizon.
    struct StubModel(usize);
    impl Regressor for StubModel {
        fn predict(&mut self, _window: &[f32]) -> anyhow::Result<Vec<f32>> {
            Ok(vec![1.0; self.0])
        }
    }

    fn meta() -> OfiSidecar {
        OfiSidecar {
            coin: "BTC".into(),
            tick: 1.0,
            model_name: "stub".into(),
            features: SidecarFeatures {
                levels: 2,
                window: 4,
                horizons: vec![2, 5],
                norm_lookback: 10,
                include_lag_return: true,
                feature_set: "ofi".into(),
                target_scale: 1e4,
            },
            objective: Default::default(),
        }
    }

    fn ramp_events(n: u64) -> Vec<RecordedEvent> {
        (0..n)
            .map(|i| {
                let mid = 1000.0 + i as f64;
                RecordedEvent {
                    seq: i,
                    ts_event_ms: i as i64 * 100,
                    ts_recv_ms: i as i64 * 100,
                    payload: MarketEvent::L2Book(L2Book {
                        coin: "BTC".into(),
                        time_ms: i as i64 * 100,
                        bids: vec![Level {
                            px: mid - 1.0,
                            sz: 2.0,
                            n: 1,
                        }],
                        asks: vec![Level {
                            px: mid + 1.0,
                            sz: 3.0,
                            n: 1,
                        }],
                    }),
                }
            })
            .collect()
    }

    #[test]
    fn replay_issues_and_resolves_expected_counts() {
        let mut model = StubModel(2);
        let sidecar = meta();
        let mut harness = OfiHarness::new(&sidecar, &mut model).unwrap();
        run_replay(&mut harness, &ramp_events(21)).unwrap();
        let stats = harness.stats();
        // 21 snapshots -> 20 feature rows (indices 0..19). Window (4) fills at
        // row 3 -> issued rows 3..19 = 17 per horizon.
        assert_eq!(stats.snapshots_seen, 21);
        assert_eq!(stats.feature_rows, 20);
        assert_eq!(stats.predictions_issued, 17);
        // h=2 resolves for issue row <= 17 (15), h=5 for row <= 14 (12).
        assert_eq!(stats.resolved, 15 + 12);
        assert_eq!(stats.unresolved, 2 + 5);
        // Mid rises 1.0 per snapshot: realized returns are positive, and the
        // stub always predicts +1bp -> every sign call is correct.
        assert!(harness.ledger.records.iter().all(|r| r.actual > 0.0));
    }

    #[test]
    fn other_coins_are_ignored() {
        let mut model = StubModel(2);
        let sidecar = meta();
        let mut harness = OfiHarness::new(&sidecar, &mut model).unwrap();
        let mut ev = ramp_events(1);
        if let MarketEvent::L2Book(b) = &mut ev[0].payload {
            b.coin = "ETH".into();
        }
        run_replay(&mut harness, &ev).unwrap();
        assert_eq!(harness.stats().snapshots_seen, 0);
    }
}
