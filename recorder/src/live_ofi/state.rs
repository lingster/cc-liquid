//! OFI feature state: book snapshots -> normalized model windows.
//!
//! Port of `deepofi.features` + `deepofi.normalization`:
//! - per-level bid/ask order flow (Cont/Kukanov/Stoikov rules), OFI = bid - ask
//! - optional lagged 1-step mid return (bps) as the last feature column
//! - causal per-feature z-score over a trailing lookback, excluding the
//!   current row (row 0 gets identity stats and is never an endpoint)
//!
//! Parity note: Python stores features as f32 and accumulates stats in f64
//! over those f32-rounded values; we do the same (round to f32, sum as f64).

use crate::events::L2Book;

/// Top-N levels of one side; missing levels are px = NaN, sz = 0.
#[derive(Clone)]
struct SideLevels {
    px: Vec<f64>,
    sz: Vec<f64>,
}

impl SideLevels {
    fn from_levels(levels: &[crate::events::Level], n: usize) -> Self {
        let mut px = vec![f64::NAN; n];
        let mut sz = vec![0.0; n];
        for (i, l) in levels.iter().take(n).enumerate() {
            px[i] = l.px;
            sz[i] = l.sz;
        }
        Self { px, sz }
    }
}

#[derive(Clone)]
struct BookLevels {
    bid: SideLevels,
    ask: SideLevels,
    mid: f64,
}

/// `aggressive_when_up = true` for bids (higher price = more aggressive
/// demand), `false` for asks. NaN prices on either side of the transition
/// compare false everywhere -> zero flow, matching the Python rules.
fn side_flow(now: &SideLevels, prev: &SideLevels, aggressive_when_up: bool, out: &mut [f64]) {
    for m in 0..out.len() {
        let (p_now, p_prev) = (now.px[m], prev.px[m]);
        let (improved, worsened) = if aggressive_when_up {
            (p_now > p_prev, p_now < p_prev)
        } else {
            (p_now < p_prev, p_now > p_prev)
        };
        out[m] = if improved {
            now.sz[m]
        } else if p_now == p_prev {
            now.sz[m] - prev.sz[m]
        } else if worsened {
            -prev.sz[m]
        } else {
            0.0 // NaN price somewhere in the transition
        };
    }
}

/// Input representation, mirroring the Python `FeatureConfig.feature_set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    /// Per-level order flow imbalance (the paper's deep OFI).
    Ofi,
    /// deepLOB-style volume grid: size binned at tick offsets from the mid.
    Grid,
}

impl FeatureKind {
    pub fn parse(name: &str) -> anyhow::Result<Self> {
        match name {
            "ofi" => Ok(Self::Ofi),
            "grid" => Ok(Self::Grid),
            other => anyhow::bail!("unsupported feature_set `{other}` (expected ofi|grid)"),
        }
    }
}

/// Append-only feature state serving causally-normalized model windows.
pub struct OfiState {
    levels: usize,
    window: usize,
    lookback: usize,
    target_scale: f64,
    include_lag_return: bool,
    kind: FeatureKind,
    tick: f64,
    n_features: usize,
    prev: Option<BookLevels>,
    rows: Vec<Vec<f32>>,
    pub mids: Vec<f64>,
    pub ts_event_ms: Vec<i64>,
    // Per-feature prefix sums (len = rows + 1) for O(1) causal stats.
    cum: Vec<Vec<f64>>,
    cum_sq: Vec<Vec<f64>>,
}

const MIN_SIGMA: f64 = 1e-9;

impl OfiState {
    pub fn new(
        levels: usize,
        window: usize,
        lookback: usize,
        target_scale: f64,
        include_lag_return: bool,
    ) -> Self {
        Self::with_kind(
            levels,
            window,
            lookback,
            target_scale,
            include_lag_return,
            FeatureKind::Ofi,
            1.0,
        )
    }

    pub fn with_kind(
        levels: usize,
        window: usize,
        lookback: usize,
        target_scale: f64,
        include_lag_return: bool,
        kind: FeatureKind,
        tick: f64,
    ) -> Self {
        let base = match kind {
            FeatureKind::Ofi => levels,
            FeatureKind::Grid => 2 * levels,
        };
        let n_features = base + usize::from(include_lag_return);
        Self {
            levels,
            window,
            lookback,
            target_scale,
            include_lag_return,
            kind,
            tick,
            n_features,
            prev: None,
            rows: Vec::new(),
            mids: Vec::new(),
            ts_event_ms: Vec::new(),
            cum: vec![vec![0.0; n_features]],
            cum_sq: vec![vec![0.0; n_features]],
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn n_features(&self) -> usize {
        self.n_features
    }

    /// Fold one snapshot. Returns the mid when a feature row was produced
    /// (i.e. from the second valid snapshot onward); `None` for the first
    /// valid snapshot or a one-sided book (which is skipped entirely,
    /// exactly like the Python pipeline drops it from the series).
    pub fn push(&mut self, book: &L2Book) -> Option<f64> {
        let (Some(bb), Some(ba)) = (book.bids.first(), book.asks.first()) else {
            return None;
        };
        let current = BookLevels {
            bid: SideLevels::from_levels(&book.bids, self.levels),
            ask: SideLevels::from_levels(&book.asks, self.levels),
            mid: (bb.px + ba.px) / 2.0,
        };
        let Some(prev) = self.prev.replace(current.clone()) else {
            return None;
        };

        let mut row = Vec::with_capacity(self.n_features);
        match self.kind {
            FeatureKind::Ofi => {
                let mut bid_flow = vec![0.0; self.levels];
                let mut ask_flow = vec![0.0; self.levels];
                side_flow(&current.bid, &prev.bid, true, &mut bid_flow);
                side_flow(&current.ask, &prev.ask, false, &mut ask_flow);
                for m in 0..self.levels {
                    row.push((bid_flow[m] - ask_flow[m]) as f32);
                }
            }
            FeatureKind::Grid => {
                // Mirrors Python `_grid_state`: size summed into tick bins
                // 1..=levels per side ([bid | ask] columns), f64 then f32.
                let mut grid = vec![0.0f64; 2 * self.levels];
                let eps = self.tick * 1e-6;
                for (offset, side, sign) in
                    [(0usize, &current.bid, 1.0f64), (self.levels, &current.ask, -1.0)]
                {
                    for m in 0..self.levels {
                        let px = side.px[m];
                        if px.is_nan() {
                            continue;
                        }
                        let bin = (sign * (current.mid - px) / self.tick - eps).ceil() as i64;
                        if (1..=self.levels as i64).contains(&bin) {
                            grid[offset + (bin - 1) as usize] += side.sz[m];
                        }
                    }
                }
                row.extend(grid.iter().map(|v| *v as f32));
            }
        }
        if self.include_lag_return {
            row.push(((current.mid / prev.mid - 1.0) * self.target_scale) as f32);
        }

        let last = self.cum.last().unwrap().clone();
        let last_sq = self.cum_sq.last().unwrap().clone();
        let mut next = last;
        let mut next_sq = last_sq;
        for (f, v) in row.iter().enumerate() {
            let v = *v as f64;
            next[f] += v;
            next_sq[f] += v * v;
        }
        self.cum.push(next);
        self.cum_sq.push(next_sq);
        self.rows.push(row);
        self.mids.push(current.mid);
        self.ts_event_ms.push(book.time_ms);
        Some(current.mid)
    }

    /// True once a full window with at least one row of stats history exists.
    pub fn ready(&self) -> bool {
        self.len() >= self.window.max(2)
    }

    /// Causal per-feature (mu, sigma) over rows [max(0, t-lookback), t).
    fn stats_at(&self, t: usize) -> (Vec<f64>, Vec<f64>) {
        if t == 0 {
            return (vec![0.0; self.n_features], vec![1.0; self.n_features]);
        }
        let start = t.saturating_sub(self.lookback);
        let count = (t - start) as f64;
        let mut mu = vec![0.0; self.n_features];
        let mut sigma = vec![1.0; self.n_features];
        for f in 0..self.n_features {
            let m = (self.cum[t][f] - self.cum[start][f]) / count;
            let var = ((self.cum_sq[t][f] - self.cum_sq[start][f]) / count - m * m).max(0.0);
            let s = var.sqrt();
            mu[f] = m;
            sigma[f] = if s > MIN_SIGMA { s } else { 1.0 };
        }
        (mu, sigma)
    }

    /// Newest normalized window, layout (window, n_features) row-major —
    /// the exact tensor the Python dataset feeds the network.
    pub fn latest_window(&self) -> anyhow::Result<Vec<f32>> {
        anyhow::ensure!(
            self.ready(),
            "need {} feature rows, have {}",
            self.window.max(2),
            self.len()
        );
        let t = self.len() - 1;
        let (mu, sigma) = self.stats_at(t);
        let start = t + 1 - self.window;
        let mut out = Vec::with_capacity(self.window * self.n_features);
        for row in &self.rows[start..=t] {
            for f in 0..self.n_features {
                out.push(((row[f] as f64 - mu[f]) / sigma[f]) as f32);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Level;

    fn book(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> L2Book {
        let lvl = |&(px, sz): &(f64, f64)| Level { px, sz, n: 1 };
        L2Book {
            coin: "BTC".into(),
            time_ms: 0,
            bids: bids.iter().map(lvl).collect(),
            asks: asks.iter().map(lvl).collect(),
        }
    }

    #[test]
    fn ofi_rules_match_python() {
        // Mirrors deepOFI tests/test_features.py::test_bid_side_rules.
        let mut st = OfiState::new(1, 2, 10, 1e4, false);
        assert!(st.push(&book(&[(100.0, 5.0)], &[(102.0, 3.0)])).is_none());
        st.push(&book(&[(101.0, 7.0)], &[(102.0, 3.0)]));
        st.push(&book(&[(101.0, 9.0)], &[(102.0, 3.0)]));
        st.push(&book(&[(100.0, 4.0)], &[(102.0, 3.0)]));
        let vals: Vec<f32> = st.rows.iter().map(|r| r[0]).collect();
        assert_eq!(vals, vec![7.0, 2.0, -9.0]);
    }

    #[test]
    fn ask_side_inverted_and_missing_levels_zero() {
        let mut st = OfiState::new(2, 2, 10, 1e4, false);
        st.push(&book(&[(100.0, 5.0)], &[(102.0, 3.0)])); // level 2 missing
        st.push(&book(&[(100.0, 5.0), (99.0, 2.0)], &[(101.0, 6.0)]));
        // Level 1: ask px down -> f = 6 -> ofi = -6. Level 2: NaN prev -> 0.
        assert_eq!(st.rows[0][0], -6.0);
        assert_eq!(st.rows[0][1], 0.0);
    }

    #[test]
    fn lag_return_in_bps_and_one_sided_books_skipped() {
        let mut st = OfiState::new(1, 2, 10, 1e4, true);
        st.push(&book(&[(100.0, 1.0)], &[(102.0, 1.0)])); // mid 101
        assert!(st.push(&book(&[], &[(102.0, 1.0)])).is_none()); // skipped
        st.push(&book(&[(102.0, 1.0)], &[(104.0, 1.0)])); // mid 103
        let expect = ((103.0f64 / 101.0 - 1.0) * 1e4) as f32;
        assert!((st.rows[0][1] - expect).abs() < 1e-3);
        assert_eq!(st.len(), 1);
    }

    #[test]
    fn windows_are_causally_normalized() {
        let mut st = OfiState::new(1, 2, 10, 1e4, false);
        // Constant book -> OFI rows of zeros except controlled size changes.
        st.push(&book(&[(100.0, 1.0)], &[(102.0, 1.0)]));
        st.push(&book(&[(100.0, 2.0)], &[(102.0, 1.0)])); // ofi = +1
        st.push(&book(&[(100.0, 4.0)], &[(102.0, 1.0)])); // ofi = +2
        st.push(&book(&[(100.0, 5.0)], &[(102.0, 1.0)])); // ofi = +1
        assert!(st.ready());
        let w = st.latest_window().unwrap();
        // Stats at t=2 cover rows 0..2 = {1, 2}: mu = 1.5, sigma = 0.5.
        assert_eq!(w.len(), 2);
        assert!((w[0] - (2.0 - 1.5) / 0.5).abs() < 1e-6);
        assert!((w[1] - (1.0 - 1.5) / 0.5).abs() < 1e-6);
    }
}
