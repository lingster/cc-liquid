//! Orchestration: snapshots in -> predictions issued -> resolutions out.
//!
//! Works over any [`EventSource`] (live WebSocket, scripted test source) and
//! any [`Predictor`] (ONNX model, test stub), so the whole loop is testable
//! offline. Replay over a recorded session uses the same `step` path.

use std::time::Duration;

use tokio::time::Instant;
use tracing::info;

use crate::events::{L2Book, MarketEvent, RecordedEvent};
use crate::live::grid::RollingBook;
use crate::live::ledger::PredictionLedger;
use crate::live::model::{Predictor, Sidecar};
use crate::parser::parse_message;
use crate::source::EventSource;

pub struct HarnessStats {
    pub snapshots_seen: u64,
    pub predictions_issued: u64,
    pub resolved: usize,
    pub unresolved: usize,
}

/// Mutable harness state shared by the live and replay drivers.
pub struct Harness<'a, P: Predictor> {
    book: RollingBook,
    pub ledger: PredictionLedger,
    model: &'a mut P,
    coin: String,
    idx: u64,
    issued: u64,
}

impl<'a, P: Predictor> Harness<'a, P> {
    pub fn new(meta: &Sidecar, horizons: Vec<u32>, model: &'a mut P) -> anyhow::Result<Self> {
        meta.validate()?;
        Ok(Self {
            book: RollingBook::with_norm(
                meta.grid.depth,
                meta.grid.window,
                meta.grid.norm_lookback,
                meta.tick,
                crate::live::grid::NormMode::parse(&meta.grid.norm)?,
            )
            .with_quote_flow(meta.grid.quote_flow)?,
            ledger: PredictionLedger::new(horizons, meta.tick, meta.grid.threshold_ticks)?,
            model,
            coin: meta.coin.clone(),
            idx: 0,
            issued: 0,
        })
    }

    /// Fold one book snapshot: resolve due predictions, then issue new ones.
    pub fn step(&mut self, book: &L2Book) -> anyhow::Result<()> {
        if book.coin != self.coin {
            return Ok(());
        }
        let Some(mid) = self.book.push(book) else {
            return Ok(()); // one-sided book: skip without consuming an index
        };
        self.ledger.resolve(self.idx, book.time_ms, mid);
        if self.book.ready() {
            let probs = self.model.predict(&self.book.latest_window()?)?;
            self.ledger.issue(self.idx, book.time_ms, mid, probs);
            self.issued += 1;
        }
        self.idx += 1;
        Ok(())
    }

    pub fn stats(&self) -> HarnessStats {
        HarnessStats {
            snapshots_seen: self.idx,
            predictions_issued: self.issued,
            resolved: self.ledger.records.len(),
            unresolved: self.ledger.pending_count(),
        }
    }
}

/// Drive the harness from a live source until `duration` elapses.
pub async fn run_live<P: Predictor>(
    harness: &mut Harness<'_, P>,
    source: &mut dyn EventSource,
    duration: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            info!("duration reached after {} snapshots", harness.idx);
            return Ok(());
        }
        let msg = match tokio::time::timeout(remaining, source.next_message()).await {
            Err(_) => {
                info!("duration reached after {} snapshots", harness.idx);
                return Ok(());
            }
            Ok(None) => {
                info!("source ended after {} snapshots", harness.idx);
                return Ok(());
            }
            Ok(Some(msg)) => msg?,
        };
        if let Ok(Some(MarketEvent::L2Book(book))) = parse_message(&msg) {
            harness.step(&book)?;
        }
    }
}

/// Drive the harness from recorded events (deterministic replay).
pub fn run_replay<P: Predictor>(
    harness: &mut Harness<'_, P>,
    events: &[RecordedEvent],
) -> anyhow::Result<()> {
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
    use crate::live::model::SidecarGrid;

    /// Always predicts "up" with fixed probabilities.
    struct StubModel;
    impl Predictor for StubModel {
        fn predict(&mut self, _window: &[f32]) -> anyhow::Result<[f32; 3]> {
            Ok([0.1, 0.2, 0.7])
        }
    }

    fn meta() -> Sidecar {
        Sidecar {
            coin: "BTC".into(),
            tick: 1.0,
            grid: SidecarGrid {
                depth: 3,
                window: 4,
                horizon: 2,
                threshold_ticks: 0.5,
                norm_lookback: 10,
                norm: "zscore".to_string(),
                trade_flow: false,
                quote_flow: false,
            },
            temperature: 1.0,
        }
    }

    fn ramp_events(n: u64) -> Vec<RecordedEvent> {
        (0..n)
            .map(|i| {
                let mid = 1000.0 + i as f64; // rises 1 tick per snapshot -> always "up"
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
        let mut model = StubModel;
        let sidecar = meta();
        let mut harness = Harness::new(&sidecar, vec![2, 5], &mut model).unwrap();
        run_replay(&mut harness, &ramp_events(20)).unwrap();
        let stats = harness.stats();
        // Window (4) fills at idx 3 -> issued at 3..19 = 17 per horizon.
        assert_eq!(stats.snapshots_seen, 20);
        assert_eq!(stats.predictions_issued, 17);
        // h=2 resolves for issue idx <= 17 (15), h=5 for idx <= 14 (12).
        assert_eq!(stats.resolved, 15 + 12);
        assert_eq!(stats.unresolved, 2 + 5);
        // Stub always says "up", the ramp always goes up: everything correct.
        assert!(harness.ledger.records.iter().all(|r| r.correct));
    }

    /// Records the size of every window it is asked to score.
    struct ShapeProbe(Vec<usize>);
    impl Predictor for ShapeProbe {
        fn predict(&mut self, window: &[f32]) -> anyhow::Result<[f32; 3]> {
            self.0.push(window.len());
            Ok([0.1, 0.2, 0.7])
        }
    }

    #[test]
    fn quote_flow_sidecar_feeds_three_channel_windows() {
        let mut sidecar = meta();
        sidecar.grid.quote_flow = true;
        let mut model = ShapeProbe(Vec::new());
        let mut harness = Harness::new(&sidecar, vec![2], &mut model).unwrap();
        run_replay(&mut harness, &ramp_events(8)).unwrap();
        assert!(!model.0.is_empty());
        assert!(model.0.iter().all(|&len| len == 3 * 4 * 3)); // (3, window, depth)
    }

    #[test]
    fn plain_sidecar_feeds_two_channel_windows() {
        let mut model = ShapeProbe(Vec::new());
        let sidecar = meta();
        let mut harness = Harness::new(&sidecar, vec![2], &mut model).unwrap();
        run_replay(&mut harness, &ramp_events(8)).unwrap();
        assert!(model.0.iter().all(|&len| len == 2 * 4 * 3));
    }

    #[test]
    fn trade_flow_sidecar_rejected_with_clear_error() {
        let mut sidecar = meta();
        sidecar.grid.trade_flow = true;
        let mut model = StubModel;
        let err = Harness::new(&sidecar, vec![2], &mut model)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("trade-flow"), "unhelpful error: {err}");
    }

    #[test]
    fn quote_flow_with_rankgauss_rejected() {
        let mut sidecar = meta();
        sidecar.grid.quote_flow = true;
        sidecar.grid.norm = "rankgauss".to_string();
        let mut model = StubModel;
        assert!(Harness::new(&sidecar, vec![2], &mut model).is_err());
    }

    #[test]
    fn other_coins_are_ignored() {
        let mut model = StubModel;
        let sidecar = meta();
        let mut harness = Harness::new(&sidecar, vec![1], &mut model).unwrap();
        let mut ev = ramp_events(1);
        if let MarketEvent::L2Book(b) = &mut ev[0].payload {
            b.coin = "ETH".into();
        }
        run_replay(&mut harness, &ev).unwrap();
        assert_eq!(harness.stats().snapshots_seen, 0);
    }

    #[tokio::test]
    async fn live_loop_consumes_scripted_source() {
        use crate::source::ScriptedSource;
        let msgs: Vec<String> = ramp_events(10)
            .iter()
            .map(|e| crate::wire::to_hl_string(&e.payload))
            .collect();
        let mut source = ScriptedSource::new(msgs);
        let mut model = StubModel;
        let sidecar = meta();
        let mut harness = Harness::new(&sidecar, vec![2], &mut model).unwrap();
        run_live(&mut harness, &mut source, Duration::from_secs(5))
            .await
            .unwrap();
        let stats = harness.stats();
        assert_eq!(stats.snapshots_seen, 10);
        assert!(stats.resolved > 0);
    }
}
