//! Indexed, read-only view of a recorded session for the viewer.
//!
//! [`SessionData`] folds a `Vec<RecordedEvent>` into:
//! - the sorted set of coins that have at least one L2 snapshot,
//! - per-coin L2 book snapshots (a "tick" is one snapshot) in seq order, and
//! - the per-coin distinct price ladder (every px seen on any level, sorted).
//!
//! It is pure: construction takes already-loaded events; `from_dir` is the only
//! I/O entry point and simply defers to [`crate::replay::load_session`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::events::{L2Book, MarketEvent, RecordedEvent};

/// One L2 book snapshot tagged with its exchange event time.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub ts_event_ms: i64,
    pub book: L2Book,
}

/// Per-coin index built from a recorded session.
#[derive(Debug, Clone, Default)]
struct CoinIndex {
    snapshots: Vec<Snapshot>,
    /// Price ladder as `price -> occurrence count`. Keyed by [`OrderedPrice`] to
    /// keep rungs sorted and de-duplicated despite `f64` not being `Ord`; the
    /// value counts how many times that price appeared across every level of
    /// every snapshot, so the UI can aggregate duplicates with a count column.
    prices: BTreeMap<OrderedPrice, usize>,
    /// Smallest/largest finite level `sz` (volume) seen across every level of
    /// every snapshot. `None` until at least one finite size is observed. Used
    /// to scale the depth chart's volume axis to a fixed range so bars don't
    /// rescale frame-to-frame during playback.
    size_min: Option<f64>,
    size_max: Option<f64>,
}

/// Wrapper giving a genuine total order over price `f64`s for ladder
/// de-duplication.
///
/// Uses [`f64::total_cmp`] so ordering is total even for adversarial inputs,
/// and normalizes `-0.0` to `0.0` on construction so the two zeros collapse to
/// a single ladder rung. Non-finite prices are filtered out before they ever
/// reach the ladder (see [`SessionData::from_events`]), but the total order
/// keeps `BTreeSet` invariants sound regardless.
#[derive(Debug, Clone, Copy)]
struct OrderedPrice(f64);

impl OrderedPrice {
    fn new(px: f64) -> Self {
        // Collapse -0.0 / +0.0 to a single canonical zero so they dedupe.
        Self(if px == 0.0 { 0.0 } else { px })
    }
}

impl PartialEq for OrderedPrice {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for OrderedPrice {}
impl PartialOrd for OrderedPrice {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrderedPrice {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Read-only, indexed view over a recorded session.
#[derive(Debug, Clone, Default)]
pub struct SessionData {
    coins: BTreeMap<String, CoinIndex>,
}

impl SessionData {
    /// Build the index from already-loaded events (pure, no I/O).
    pub fn from_events(events: &[RecordedEvent]) -> Self {
        let mut coins: BTreeMap<String, CoinIndex> = BTreeMap::new();
        for ev in events {
            if let MarketEvent::L2Book(book) = &ev.payload {
                let idx = coins.entry(book.coin.clone()).or_default();
                for level in book.bids.iter().chain(book.asks.iter()) {
                    // Skip NaN / ±Inf prices so corrupt parquet can never inject
                    // a non-finite rung into the ladder (M1).
                    if level.px.is_finite() {
                        *idx.prices.entry(OrderedPrice::new(level.px)).or_insert(0) += 1;
                    }
                    // Track the volume (size) range; skip non-finite so corrupt
                    // parquet can never poison the chart's axis scaling.
                    if level.sz.is_finite() {
                        idx.size_min = Some(idx.size_min.map_or(level.sz, |m| m.min(level.sz)));
                        idx.size_max = Some(idx.size_max.map_or(level.sz, |m| m.max(level.sz)));
                    }
                }
                idx.snapshots.push(Snapshot {
                    ts_event_ms: ev.ts_event_ms,
                    book: book.clone(),
                });
            }
        }
        Self { coins }
    }

    /// Convenience loader: read a session directory and index it.
    pub fn from_dir(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let events = crate::replay::load_session(dir)?;
        Ok(Self::from_events(&events))
    }

    /// Coins that have at least one L2 snapshot, sorted ascending.
    pub fn coins(&self) -> Vec<String> {
        self.coins.keys().cloned().collect()
    }

    /// Whether any coin has snapshots.
    pub fn is_empty(&self) -> bool {
        self.coins.is_empty()
    }

    /// Snapshots for `coin` in seq order (empty slice if coin unknown).
    pub fn snapshots(&self, coin: &str) -> &[Snapshot] {
        self.coins
            .get(coin)
            .map(|c| c.snapshots.as_slice())
            .unwrap_or(&[])
    }

    /// Distinct price ladder for `coin`, sorted ascending.
    pub fn price_ladder(&self, coin: &str) -> Vec<f64> {
        self.coins
            .get(coin)
            .map(|c| c.prices.keys().map(|p| p.0).collect())
            .unwrap_or_default()
    }

    /// Price ladder with aggregated occurrence counts: `(price, count)` pairs
    /// sorted ascending by price. `count` is how many times the price appeared
    /// across every level of every snapshot for `coin`.
    pub fn price_ladder_counts(&self, coin: &str) -> Vec<(f64, usize)> {
        self.coins
            .get(coin)
            .map(|c| c.prices.iter().map(|(p, n)| (p.0, *n)).collect())
            .unwrap_or_default()
    }

    /// Number of snapshots (ticks) for `coin`.
    pub fn tick_count(&self, coin: &str) -> usize {
        self.snapshots(coin).len()
    }

    /// Inclusive `(min, max)` of level volume (`sz`) across every level of
    /// every snapshot for `coin`. `None` when the coin has no finite sizes.
    pub fn size_range(&self, coin: &str) -> Option<(f64, f64)> {
        self.coins
            .get(coin)
            .and_then(|c| match (c.size_min, c.size_max) {
                (Some(lo), Some(hi)) => Some((lo, hi)),
                _ => None,
            })
    }

    /// Inclusive `(min, max)` of `ts_event_ms` across `coin`'s snapshots.
    pub fn time_range(&self, coin: &str) -> Option<(i64, i64)> {
        let snaps = self.snapshots(coin);
        match (snaps.first(), snaps.last()) {
            (Some(f), Some(l)) => Some((f.ts_event_ms, l.ts_event_ms)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, Level, Side, Trade};

    fn book_event(seq: u64, ts: i64, coin: &str, bids: &[f64], asks: &[f64]) -> RecordedEvent {
        let mk = |px: &f64| Level {
            px: *px,
            sz: 1.0,
            n: 1,
        };
        RecordedEvent {
            seq,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::L2Book(L2Book {
                coin: coin.into(),
                time_ms: ts,
                bids: bids.iter().map(mk).collect(),
                asks: asks.iter().map(mk).collect(),
            }),
        }
    }

    #[test]
    fn empty_session_has_no_coins() {
        let s = SessionData::from_events(&[]);
        assert!(s.is_empty());
        assert!(s.coins().is_empty());
        assert_eq!(s.tick_count("BTC"), 0);
        assert_eq!(s.time_range("BTC"), None);
        assert!(s.price_ladder("BTC").is_empty());
    }

    #[test]
    fn discovers_coins_from_l2_books_only_sorted() {
        let events = vec![
            RecordedEvent {
                seq: 0,
                ts_event_ms: 1,
                ts_recv_ms: 1,
                // AllMids must NOT introduce coins into the L2 coin set.
                payload: MarketEvent::AllMids(AllMids {
                    mids: vec![("ZZZ".into(), 1.0)],
                }),
            },
            book_event(1, 2, "ETH", &[10.0], &[11.0]),
            book_event(2, 3, "BTC", &[100.0], &[101.0]),
            RecordedEvent {
                seq: 3,
                ts_event_ms: 4,
                ts_recv_ms: 4,
                payload: MarketEvent::Trades(vec![Trade {
                    coin: "DOGE".into(),
                    side: Side::Buy,
                    px: 1.0,
                    sz: 1.0,
                    time_ms: 4,
                }]),
            },
        ];
        let s = SessionData::from_events(&events);
        assert_eq!(s.coins(), vec!["BTC".to_string(), "ETH".to_string()]);
    }

    #[test]
    fn snapshots_preserved_in_seq_order_per_coin() {
        let events = vec![
            book_event(0, 10, "BTC", &[100.0], &[101.0]),
            book_event(1, 20, "ETH", &[10.0], &[11.0]),
            book_event(2, 30, "BTC", &[102.0], &[103.0]),
        ];
        let s = SessionData::from_events(&events);
        assert_eq!(s.tick_count("BTC"), 2);
        assert_eq!(s.tick_count("ETH"), 1);
        let btc = s.snapshots("BTC");
        assert_eq!(btc[0].ts_event_ms, 10);
        assert_eq!(btc[1].ts_event_ms, 30);
        assert_eq!(btc[1].book.bids[0].px, 102.0);
    }

    #[test]
    fn price_ladder_is_distinct_and_sorted() {
        let events = vec![
            book_event(0, 10, "BTC", &[100.0, 99.0], &[101.0, 102.0]),
            // 100.0 and 101.0 repeat; 98.0 and 103.0 are new.
            book_event(1, 20, "BTC", &[100.0, 98.0], &[101.0, 103.0]),
        ];
        let s = SessionData::from_events(&events);
        assert_eq!(
            s.price_ladder("BTC"),
            vec![98.0, 99.0, 100.0, 101.0, 102.0, 103.0]
        );
    }

    #[test]
    fn price_ladder_counts_aggregate_duplicates() {
        let events = vec![
            book_event(0, 10, "BTC", &[100.0, 99.0], &[101.0, 102.0]),
            // 100.0 and 101.0 repeat; 98.0 and 103.0 are new.
            book_event(1, 20, "BTC", &[100.0, 98.0], &[101.0, 103.0]),
        ];
        let s = SessionData::from_events(&events);
        assert_eq!(
            s.price_ladder_counts("BTC"),
            vec![
                (98.0, 1),
                (99.0, 1),
                (100.0, 2),
                (101.0, 2),
                (102.0, 1),
                (103.0, 1),
            ]
        );
        // Unknown coin yields an empty ladder, not a panic.
        assert!(s.price_ladder_counts("ETH").is_empty());
    }

    #[test]
    fn size_range_spans_min_max_volume_skipping_non_finite() {
        // Build books with explicit, varying sizes (the book_event helper fixes
        // sz=1.0, so construct levels directly here).
        let lvl = |px: f64, sz: f64| Level { px, sz, n: 1 };
        let mk = |seq: u64, ts: i64, bids: Vec<Level>, asks: Vec<Level>| RecordedEvent {
            seq,
            ts_event_ms: ts,
            ts_recv_ms: ts,
            payload: MarketEvent::L2Book(L2Book {
                coin: "BTC".into(),
                time_ms: ts,
                bids,
                asks,
            }),
        };
        let events = vec![
            mk(0, 10, vec![lvl(100.0, 2.5)], vec![lvl(101.0, 8.0)]),
            // 0.5 is the new min; f64::NAN/INFINITY must be ignored.
            mk(
                1,
                20,
                vec![lvl(100.0, 0.5), lvl(99.0, f64::NAN)],
                vec![lvl(101.0, f64::INFINITY)],
            ),
        ];
        let s = SessionData::from_events(&events);
        assert_eq!(s.size_range("BTC"), Some((0.5, 8.0)));
        assert_eq!(s.size_range("ETH"), None);
    }

    #[test]
    fn ladder_skips_non_finite_and_collapses_zeros() {
        // Adversarial parquet: NaN, ±Inf must be dropped; -0.0 and 0.0 must
        // collapse to a single rung; finite prices stay sorted (M1).
        let events = vec![book_event(
            0,
            10,
            "BTC",
            &[f64::NAN, f64::INFINITY, -0.0, 0.0, 99.0],
            &[f64::NEG_INFINITY, 100.0, 101.0],
        )];
        let s = SessionData::from_events(&events);
        let ladder = s.price_ladder("BTC");
        assert_eq!(ladder, vec![0.0, 99.0, 100.0, 101.0]);
        // Exactly one zero rung (the two zeros collapsed).
        assert_eq!(ladder.iter().filter(|p| **p == 0.0).count(), 1);
        // No non-finite values leaked in.
        assert!(ladder.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn time_range_spans_first_to_last_snapshot() {
        let events = vec![
            book_event(0, 100, "BTC", &[1.0], &[2.0]),
            book_event(1, 250, "BTC", &[1.0], &[2.0]),
            book_event(2, 400, "BTC", &[1.0], &[2.0]),
        ];
        let s = SessionData::from_events(&events);
        assert_eq!(s.time_range("BTC"), Some((100, 400)));
    }

    #[test]
    fn single_tick_session_has_zero_width_range() {
        let s = SessionData::from_events(&[book_event(0, 500, "BTC", &[1.0], &[2.0])]);
        assert_eq!(s.tick_count("BTC"), 1);
        assert_eq!(s.time_range("BTC"), Some((500, 500)));
    }
}
