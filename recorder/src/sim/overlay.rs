//! Configurable fill-price overlays (PRD §7.1.1).
//!
//! The matching engine decides *how much* fills; the overlay adjusts *at what
//! price*, letting the operator simulate market regimes (bull/bear bias, wide
//! spreads, noisy fills) without re-recording. All models are deterministic:
//! `random_spread` uses a seeded xorshift generator, so identical runs
//! produce identical fills.

use std::str::FromStr;

/// Price adjustment applied to a matched fill.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum FillOverlay {
    /// Pure book/mid price — no adjustment (default).
    #[default]
    Book,
    /// Shift the fill a fixed fraction *against* the taker on buys and
    /// *for* the taker on sells when positive (e.g. `0.01` = buys fill 1%
    /// higher, sells 1% lower). Negative biases the other way.
    BiasedOffset(f64),
    /// Impose a constant half-spread around the matched price: takers always
    /// pay `frac` away from it (buys up, sells down).
    FixedSpread(f64),
    /// Sample the half-spread uniformly from `[0, max_frac]` per fill,
    /// deterministically from `seed`.
    RandomSpread { max_frac: f64, seed: u64 },
    /// Fill at the worst price the matcher consumed (passed in as
    /// `worst_px`), regardless of the size-weighted average.
    WorstCase,
}

impl FillOverlay {
    /// Adjust a matched fill price. `avg_px` is the size-weighted matched
    /// price, `worst_px` the worst consumed level (== `avg_px` for mid-based
    /// fills). The mutable `rng` state advances only for random models.
    pub fn adjust(&self, avg_px: f64, worst_px: f64, is_buy: bool, rng: &mut Xorshift64) -> f64 {
        let signed = |frac: f64| {
            if is_buy {
                avg_px * (1.0 + frac)
            } else {
                avg_px * (1.0 - frac)
            }
        };
        match self {
            FillOverlay::Book => avg_px,
            FillOverlay::BiasedOffset(frac) => signed(*frac),
            FillOverlay::FixedSpread(frac) => signed(frac.abs()),
            FillOverlay::RandomSpread { max_frac, .. } => signed(rng.next_unit() * max_frac.abs()),
            FillOverlay::WorstCase => worst_px,
        }
    }

    /// Seed for the per-run RNG (0 for non-random models).
    pub fn seed(&self) -> u64 {
        match self {
            FillOverlay::RandomSpread { seed, .. } => *seed,
            _ => 0,
        }
    }

    /// Build from CLI-style strings: model name plus a numeric parameter.
    pub fn from_config(model: &str, param: f64, seed: u64) -> Result<Self, String> {
        match model.to_ascii_lowercase().as_str() {
            "book" => Ok(FillOverlay::Book),
            "biased_offset" => Ok(FillOverlay::BiasedOffset(param)),
            "fixed_spread" => Ok(FillOverlay::FixedSpread(param)),
            "random_spread" => Ok(FillOverlay::RandomSpread { max_frac: param, seed }),
            "worst_case" => Ok(FillOverlay::WorstCase),
            other => Err(format!(
                "unknown fill model `{other}` (use book|biased_offset|fixed_spread|random_spread|worst_case)"
            )),
        }
    }
}

impl FromStr for FillOverlay {
    type Err = String;
    /// Parse `model` or `model:param` (e.g. `biased_offset:0.01`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (model, param) = match s.split_once(':') {
            Some((m, p)) => (
                m,
                p.parse::<f64>()
                    .map_err(|_| format!("bad parameter `{p}`"))?,
            ),
            None => (s, 0.0),
        };
        FillOverlay::from_config(model, param, 0)
    }
}

/// Minimal deterministic PRNG (xorshift64*) — no external dependency, stable
/// across platforms, good enough for spread sampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    pub fn new(seed: u64) -> Self {
        // Zero state would be a fixed point; nudge it.
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng() -> Xorshift64 {
        Xorshift64::new(42)
    }

    #[test]
    fn book_passes_price_through() {
        assert_eq!(
            FillOverlay::Book.adjust(100.0, 105.0, true, &mut rng()),
            100.0
        );
    }

    #[test]
    fn biased_offset_moves_against_buyers_and_for_sellers() {
        let o = FillOverlay::BiasedOffset(0.01);
        assert!((o.adjust(100.0, 100.0, true, &mut rng()) - 101.0).abs() < 1e-9);
        assert!((o.adjust(100.0, 100.0, false, &mut rng()) - 99.0).abs() < 1e-9);
        // Negative bias flips direction (bull market for buyers).
        let o = FillOverlay::BiasedOffset(-0.01);
        assert!((o.adjust(100.0, 100.0, true, &mut rng()) - 99.0).abs() < 1e-9);
    }

    #[test]
    fn fixed_spread_always_costs_the_taker() {
        let o = FillOverlay::FixedSpread(0.002);
        assert!(o.adjust(100.0, 100.0, true, &mut rng()) > 100.0);
        assert!(o.adjust(100.0, 100.0, false, &mut rng()) < 100.0);
    }

    #[test]
    fn random_spread_is_deterministic_per_seed_and_bounded() {
        let o = FillOverlay::RandomSpread {
            max_frac: 0.01,
            seed: 7,
        };
        let mut a = Xorshift64::new(7);
        let mut b = Xorshift64::new(7);
        let run = |rng: &mut Xorshift64| -> Vec<f64> {
            (0..5).map(|_| o.adjust(100.0, 100.0, true, rng)).collect()
        };
        let fills_a = run(&mut a);
        let fills_b = run(&mut b);
        assert_eq!(fills_a, fills_b, "same seed, same fills");
        assert!(fills_a.iter().all(|px| (100.0..101.0).contains(px)));
        // The sequence must actually vary.
        assert!(fills_a.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn worst_case_uses_the_worst_consumed_level() {
        assert_eq!(
            FillOverlay::WorstCase.adjust(100.0, 104.5, true, &mut rng()),
            104.5
        );
    }

    #[test]
    fn parses_cli_forms() {
        assert_eq!("book".parse::<FillOverlay>().unwrap(), FillOverlay::Book);
        assert_eq!(
            "biased_offset:0.01".parse::<FillOverlay>().unwrap(),
            FillOverlay::BiasedOffset(0.01)
        );
        assert_eq!(
            "random_spread:0.005".parse::<FillOverlay>().unwrap(),
            FillOverlay::RandomSpread {
                max_frac: 0.005,
                seed: 0
            }
        );
        assert!("warp_drive".parse::<FillOverlay>().is_err());
        assert!("fixed_spread:abc".parse::<FillOverlay>().is_err());
    }
}
