//! Downsampling for time-series display.
//!
//! A price series can hold millions of points, but a chart only has ~1–2k
//! pixels of width — drawing every point is wasted work and indistinguishable
//! on screen. [`downsample`] reduces a `(ts, value)` series to roughly the
//! pixel budget while preserving visual shape: it buckets by index and keeps
//! each bucket's min *and* max (in time order), so spikes and dips survive
//! rather than being averaged away. Pure and unit-tested.

/// Reduce `series` to at most ~`max_points` points for display.
///
/// Returns the series unchanged (cloned) when it already fits or when
/// `max_points` is too small to be meaningful. The first and last points are
/// always preserved so the time range and endpoints render exactly.
pub fn downsample(series: &[(i64, f64)], max_points: usize) -> Vec<(i64, f64)> {
    let n = series.len();
    if max_points < 4 || n <= max_points {
        return series.to_vec();
    }
    // Each bucket contributes up to two points (min & max), plus the explicit
    // first/last, so aim for max_points/2 buckets.
    let buckets = (max_points / 2).max(1);
    let mut out = Vec::with_capacity(max_points + 2);
    out.push(series[0]);

    for b in 0..buckets {
        let start = b * n / buckets;
        let end = ((b + 1) * n / buckets).min(n);
        if start >= end {
            continue;
        }
        let slice = &series[start..end];
        let mut lo = slice[0];
        let mut hi = slice[0];
        for &p in slice {
            if p.1 < lo.1 {
                lo = p;
            }
            if p.1 > hi.1 {
                hi = p;
            }
        }
        // Emit the extremes in chronological order to keep the line monotone
        // in time.
        if lo.0 <= hi.0 {
            out.push(lo);
            if hi.0 != lo.0 {
                out.push(hi);
            }
        } else {
            out.push(hi);
            out.push(lo);
        }
    }

    let last = series[n - 1];
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_series_is_returned_unchanged() {
        let s = vec![(0, 1.0), (1, 2.0), (2, 3.0)];
        assert_eq!(downsample(&s, 100), s);
        // Degenerate max_points also passes through.
        assert_eq!(downsample(&s, 0), s);
    }

    #[test]
    fn reduces_to_about_the_budget() {
        let s: Vec<(i64, f64)> = (0..10_000).map(|i| (i, i as f64)).collect();
        let ds = downsample(&s, 200);
        assert!(ds.len() <= 202, "got {} points", ds.len());
        assert!(ds.len() >= 100, "got {} points", ds.len());
    }

    #[test]
    fn preserves_endpoints_and_is_time_sorted() {
        let s: Vec<(i64, f64)> = (0..10_000).map(|i| (i * 10, (i % 7) as f64)).collect();
        let ds = downsample(&s, 256);
        assert_eq!(ds.first(), s.first());
        assert_eq!(ds.last(), s.last());
        for w in ds.windows(2) {
            assert!(w[0].0 <= w[1].0, "timestamps must be non-decreasing");
        }
    }

    #[test]
    fn preserves_a_spike() {
        // A flat series with one tall spike in the middle: the spike's value
        // must survive downsampling (min/max bucketing, not averaging).
        let mut s: Vec<(i64, f64)> = (0..10_000).map(|i| (i, 1.0)).collect();
        s[5000].1 = 999.0;
        let ds = downsample(&s, 100);
        assert!(
            ds.iter().any(|&(_, v)| v == 999.0),
            "the spike must be retained"
        );
    }
}
