//! Volume-representation grid + rolling causal normalization.
//!
//! Port of `orderbooker.live.state` (which itself mirrors the training
//! pipeline): volume binned at exact tick offsets from the mid, z-scored with
//! statistics over only *prior* snapshots.

use crate::events::L2Book;

/// One snapshot -> (flattened `[bid(depth) | ask(depth)]` grid, mid).
/// `None` when either side of the book is empty.
pub fn snapshot_grid(book: &L2Book, depth: usize, tick: f64) -> Option<(Vec<f32>, f64)> {
    let best_bid = book.bids.first()?;
    let best_ask = book.asks.first()?;
    let mid = (best_bid.px + best_ask.px) / 2.0;
    let mut grid = vec![0f32; 2 * depth];
    let eps = tick * 1e-6;
    for (channel, levels, is_bid) in [(0usize, &book.bids, true), (1, &book.asks, false)] {
        for level in levels {
            let dist = if is_bid {
                mid - level.px
            } else {
                level.px - mid
            };
            let bin = (dist / tick - eps).ceil() as i64;
            if (1..=depth as i64).contains(&bin) {
                grid[channel * depth + (bin - 1) as usize] += level.sz as f32;
            }
        }
    }
    Some((grid, mid))
}

/// Input normalization scheme (mirrors the Python `GridConfig.norm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormMode {
    ZScore,
    RankGauss,
}

impl NormMode {
    pub fn parse(name: &str) -> anyhow::Result<Self> {
        match name {
            "zscore" => Ok(Self::ZScore),
            "rankgauss" => Ok(Self::RankGauss),
            other => anyhow::bail!("unknown normalization `{other}` (expected zscore|rankgauss)"),
        }
    }
}

/// Snapshots between rank-gauss reference re-sorts (must match the Python
/// `normalization.RANK_REFRESH`).
const RANK_REFRESH: usize = 16;
const P_CLAMP: f64 = 1e-6;

/// Append-only book state serving causally-normalized model windows.
pub struct RollingBook {
    depth: usize,
    window: usize,
    lookback: usize,
    tick: f64,
    norm: NormMode,
    /// Z-score mode: raw grids. Rank-gauss mode: grids already normalized.
    grids: Vec<Vec<f32>>,
    /// Rank-gauss mode only: raw cell history for reference building.
    raw: Vec<Vec<f32>>,
    sorted_ref: Vec<f64>,
    pub mids: Vec<f64>,
    pub ts_event_ms: Vec<i64>,
    // Prefix sums of per-snapshot cell mean / squared-cell mean (len = n + 1),
    // so the rolling z-score stats are O(1) per window.
    cum_mean: Vec<f64>,
    cum_sq: Vec<f64>,
}

impl RollingBook {
    pub fn new(depth: usize, window: usize, lookback: usize, tick: f64) -> Self {
        Self::with_norm(depth, window, lookback, tick, NormMode::ZScore)
    }

    pub fn with_norm(
        depth: usize,
        window: usize,
        lookback: usize,
        tick: f64,
        norm: NormMode,
    ) -> Self {
        Self {
            depth,
            window,
            lookback,
            tick,
            norm,
            grids: Vec::new(),
            raw: Vec::new(),
            sorted_ref: Vec::new(),
            mids: Vec::new(),
            ts_event_ms: Vec::new(),
            cum_mean: vec![0.0],
            cum_sq: vec![0.0],
        }
    }

    pub fn len(&self) -> usize {
        self.grids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.grids.is_empty()
    }

    /// Fold one snapshot in; returns its mid (`None` if a side was empty).
    pub fn push(&mut self, book: &L2Book) -> Option<f64> {
        let (grid, mid) = snapshot_grid(book, self.depth, self.tick)?;
        match self.norm {
            NormMode::ZScore => {
                let cells = (2 * self.depth) as f64;
                let mean: f64 = grid.iter().map(|v| *v as f64).sum::<f64>() / cells;
                let sq_mean: f64 =
                    grid.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / cells;
                self.cum_mean.push(self.cum_mean.last().unwrap() + mean);
                self.cum_sq.push(self.cum_sq.last().unwrap() + sq_mean);
                self.grids.push(grid);
            }
            NormMode::RankGauss => {
                // Mirrors Python `rank_gauss_normalize`: snapshot index s gets
                // a reference re-sorted when s % K == 0 or s <= K, covering
                // raw cells of [max(0, s - lookback), s). s = 0 -> zeros.
                let s = self.raw.len();
                if s == 0 {
                    self.grids.push(vec![0.0; 2 * self.depth]);
                } else {
                    if s.is_multiple_of(RANK_REFRESH) || s <= RANK_REFRESH {
                        let start = s.saturating_sub(self.lookback);
                        let mut ref_cells: Vec<f64> = self.raw[start..s]
                            .iter()
                            .flat_map(|g| g.iter().map(|v| *v as f64))
                            .collect();
                        ref_cells.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        self.sorted_ref = ref_cells;
                    }
                    self.grids.push(
                        grid.iter()
                            .map(|v| gauss_rank(*v as f64, &self.sorted_ref) as f32)
                            .collect(),
                    );
                }
                self.raw.push(grid);
            }
        }
        self.mids.push(mid);
        self.ts_event_ms.push(book.time_ms);
        Some(mid)
    }

    /// True once a full window (plus one snapshot of stats history) exists.
    pub fn ready(&self) -> bool {
        self.len() >= self.window.max(2)
    }

    /// Causal (mu, sigma) over the `lookback` snapshots before `t` — never `t`
    /// itself. Index 0 has no history and gets identity stats.
    fn stats_at(&self, t: usize) -> (f64, f64) {
        if t == 0 {
            return (0.0, 1.0);
        }
        let start = t.saturating_sub(self.lookback);
        let count = (t - start) as f64;
        let mu = (self.cum_mean[t] - self.cum_mean[start]) / count;
        let var = ((self.cum_sq[t] - self.cum_sq[start]) / count - mu * mu).max(0.0);
        let sigma = var.sqrt();
        (mu, if sigma > 1e-9 { sigma } else { 1.0 })
    }

    /// The newest model input, layout `(2, window, depth)` channel-major —
    /// the exact tensor the Python pipeline feeds the network.
    pub fn latest_window(&self) -> anyhow::Result<Vec<f32>> {
        anyhow::ensure!(
            self.ready(),
            "need {} snapshots, have {}",
            self.window.max(2),
            self.len()
        );
        let t = self.len() - 1;
        let (mu, sigma) = match self.norm {
            NormMode::ZScore => self.stats_at(t),
            NormMode::RankGauss => (0.0, 1.0), // grids are already normalized
        };
        let start = t + 1 - self.window;
        let mut out = vec![0f32; 2 * self.window * self.depth];
        for channel in 0..2 {
            for (w, grid) in self.grids[start..=t].iter().enumerate() {
                for d in 0..self.depth {
                    let v = grid[channel * self.depth + d] as f64;
                    out[channel * self.window * self.depth + w * self.depth + d] =
                        ((v - mu) / sigma) as f32;
                }
            }
        }
        Ok(out)
    }
}

/// Gauss-rank one value against a sorted reference (midrank for ties), then
/// map through the inverse normal CDF. Matches Python `normalization.gauss_rank`.
pub fn gauss_rank(value: f64, sorted_ref: &[f64]) -> f64 {
    if sorted_ref.is_empty() {
        return 0.0;
    }
    let left = sorted_ref.partition_point(|x| *x < value);
    let right = sorted_ref.partition_point(|x| *x <= value);
    let midrank = (left + right) as f64 / 2.0;
    let p = (midrank + 0.5) / (sorted_ref.len() as f64 + 1.0);
    inverse_normal_cdf(p.clamp(P_CLAMP, 1.0 - P_CLAMP))
}

/// Acklam's inverse normal CDF — constants shared verbatim with the Python
/// implementation so live preprocessing matches training bit-for-bit.
pub fn inverse_normal_cdf(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e01,
        2.209460984245205e02,
        -2.759285104469687e02,
        #[allow(clippy::excessive_precision)] // constants shared verbatim with Python
        1.383577518672690e02,
        -3.066479806614716e01,
        2.506628277459239e00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e01,
        1.615858368580409e02,
        -1.556989798598866e02,
        6.680131188771972e01,
        -1.328068155288572e01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e00,
        -2.549732539343734e00,
        4.374664141464968e00,
        2.938163982698783e00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e00,
        3.754408661907416e00,
    ];
    const P_LOW: f64 = 0.02425;

    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p > 1.0 - P_LOW {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -((((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0))
    } else {
        let q = p - 0.5;
        let r = q * q;
        ((((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q)
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
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
    fn volumes_land_in_tick_bins() {
        // Mirrors the Python test: mid = 100, tick 1, depth 4.
        let b = book(&[(99.0, 2.0), (97.0, 1.5)], &[(101.0, 3.0)]);
        let (grid, mid) = snapshot_grid(&b, 4, 1.0).unwrap();
        assert_eq!(mid, 100.0);
        assert_eq!(&grid[..4], &[2.0, 0.0, 1.5, 0.0]); // bids
        assert_eq!(&grid[4..], &[3.0, 0.0, 0.0, 0.0]); // asks
    }

    #[test]
    fn one_tick_spread_maps_touch_to_first_bin() {
        let b = book(&[(100.0, 1.0)], &[(101.0, 4.0)]);
        let (grid, mid) = snapshot_grid(&b, 3, 1.0).unwrap();
        assert_eq!(mid, 100.5);
        assert_eq!(grid[0], 1.0);
        assert_eq!(grid[3], 4.0);
    }

    #[test]
    fn volume_beyond_depth_dropped_and_empty_side_rejected() {
        let b = book(&[(99.0, 2.0)], &[(101.0, 3.0), (110.0, 9.0)]);
        let (grid, _) = snapshot_grid(&b, 4, 1.0).unwrap();
        assert_eq!(grid[4..].iter().sum::<f32>(), 3.0);
        assert!(snapshot_grid(&book(&[], &[(101.0, 1.0)]), 4, 1.0).is_none());
    }

    #[test]
    fn causal_stats_exclude_current_and_respect_lookback() {
        let mut rb = RollingBook::new(1, 2, 3, 1.0);
        // Cell values: snapshots with bid vol v at bin 1 -> mean over 2 cells = v/2.
        for v in [2.0, 4.0, 6.0, 8.0] {
            rb.push(&book(&[(99.0, v)], &[(101.0, 0.0)]));
        }
        // stats at t=3 pool the cells of snapshots 0..3: {2,0,4,0,6,0}.
        // mu = 2; var = E[x^2] - mu^2 = (4+16+36)/6 - 4 = 16/3 (matches Python's
        // pooled-cell statistics, not the variance of per-snapshot means).
        let (mu, sigma) = rb.stats_at(3);
        assert!((mu - 2.0).abs() < 1e-12);
        assert!((sigma - (16.0f64 / 3.0).sqrt()).abs() < 1e-12);
        // Index 0 is identity.
        assert_eq!(rb.stats_at(0), (0.0, 1.0));
    }

    #[test]
    fn inverse_normal_cdf_matches_reference_quantiles() {
        assert!((inverse_normal_cdf(0.5)).abs() < 1e-9);
        assert!((inverse_normal_cdf(0.975) - 1.959964).abs() < 2e-4);
        assert!((inverse_normal_cdf(0.025) + 1.959964).abs() < 2e-4);
        // Symmetry across the piecewise tails.
        assert!((inverse_normal_cdf(0.01) + inverse_normal_cdf(0.99)).abs() < 1e-9);
    }

    #[test]
    fn gauss_rank_orders_bounds_and_ties() {
        let reference: Vec<f64> = (0..=100).map(|v| v as f64).collect();
        let mid = gauss_rank(50.0, &reference);
        assert!(mid.abs() < 1e-2);
        assert!(gauss_rank(0.0, &reference) < mid);
        assert!(gauss_rank(1e9, &reference) < 4.0); // outliers bounded
        assert_eq!(gauss_rank(7.0, &[]), 0.0); // empty reference -> neutral
                                               // Ties share one value (midrank).
        let tied = vec![0.0, 0.0, 0.0, 1.0, 2.0];
        assert_eq!(gauss_rank(0.0, &tied), gauss_rank(0.0, &tied));
        assert!(gauss_rank(0.0, &tied) < 0.0); // zeros sit in the lower mass
    }

    #[test]
    fn rankgauss_mode_emits_bounded_normalized_windows() {
        let mut rb = RollingBook::with_norm(2, 2, 50, 1.0, NormMode::RankGauss);
        for v in [1.0, 3.0, 2.0, 5.0, 4.0] {
            rb.push(&book(&[(99.0, v)], &[(101.0, v + 1.0)]));
        }
        let w = rb.latest_window().unwrap();
        assert_eq!(w.len(), 2 * 2 * 2);
        assert!(w.iter().all(|x| x.abs() < 5.0));
        // Snapshot 0 has no history and is all zeros in the buffer.
        assert!(rb.grids[0].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn latest_window_is_normalized_and_channel_major() {
        let mut rb = RollingBook::new(2, 2, 10, 1.0);
        rb.push(&book(&[(99.0, 1.0)], &[(101.0, 2.0)]));
        rb.push(&book(&[(99.0, 3.0)], &[(101.0, 4.0)]));
        let w = rb.latest_window().unwrap();
        assert_eq!(w.len(), 2 * 2 * 2);
        // Stats at t=1: cells of snapshot 0 = [1,0,2,0] -> mu=0.75, var=mean(sq)-mu^2.
        let mu = 0.75;
        let sigma = ((1.0 + 4.0) / 4.0f64 - mu * mu).sqrt();
        // Channel 0 (bids), w=0 (snapshot 0), d=0 holds volume 1.0.
        assert!((w[0] as f64 - (1.0 - mu) / sigma).abs() < 1e-6);
        // Channel 1 (asks), w=1 (snapshot 1), d=0 holds volume 4.0.
        assert!((w[2 * 2 + 2] as f64 - (4.0 - mu) / sigma).abs() < 1e-6);
    }
}
