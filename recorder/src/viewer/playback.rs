//! Deterministic, wall-clock-paced playback math.
//!
//! [`PlaybackClock`] answers one question for the UI loop: *given how much real
//! time has elapsed since playback began, which tick should we be on now?* It
//! maps real elapsed milliseconds onto the exchange-time (`ts_event_ms`) deltas
//! between snapshots, scaled by a speed multiplier, so realtime replay matches
//! the cadence the data was recorded at.
//!
//! The core ([`PlaybackClock::target_index`]) takes elapsed time as a parameter
//! — it never reads a real clock — so it is fully unit-testable. The eframe
//! binary owns the `Instant` and feeds the elapsed duration in.

/// Whether playback is currently advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
}

/// Playback engine over a fixed snapshot timeline.
///
/// `timeline` is the per-tick `ts_event_ms`, ascending. Playback always runs
/// from an *anchor* tick (the tick we were on when Play was pressed); elapsed
/// wall-clock time is measured from that anchor.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    timeline: Vec<i64>,
    speed: f64,
    state: PlaybackState,
    anchor_index: usize,
}

impl PlaybackClock {
    /// Build a clock over a snapshot timeline. Defaults to paused at tick 0,
    /// speed 1.0.
    pub fn new(timeline: Vec<i64>) -> Self {
        Self {
            timeline,
            speed: 1.0,
            state: PlaybackState::Paused,
            anchor_index: 0,
        }
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn is_playing(&self) -> bool {
        self.state == PlaybackState::Playing
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Set the speed multiplier, clamped to a sane positive range.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.01, 1000.0);
    }

    pub fn len(&self) -> usize {
        self.timeline.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timeline.is_empty()
    }

    /// Begin playing from `from_index` (the tick currently shown). The caller
    /// resets its elapsed-time stopwatch to zero in the same instant.
    pub fn play(&mut self, from_index: usize) {
        self.anchor_index = self.clamp_index(from_index);
        self.state = PlaybackState::Playing;
    }

    /// Stop advancing. The current tick is whatever the UI last computed.
    pub fn pause(&mut self) {
        self.state = PlaybackState::Paused;
    }

    /// The anchor tick playback is measured from.
    pub fn anchor_index(&self) -> usize {
        self.anchor_index
    }

    /// Compute the tick index for `elapsed_ms` of real time since the anchor.
    ///
    /// Exchange time advances `speed * elapsed_ms` from the anchor's timestamp;
    /// the result is the latest tick whose timestamp is `<=` that target. The
    /// index is clamped to the final tick (end-of-session). Independent of
    /// playback state so the UI may call it freely.
    pub fn target_index(&self, elapsed_ms: f64) -> usize {
        if self.timeline.is_empty() {
            return 0;
        }
        let anchor = self.clamp_index(self.anchor_index);
        let anchor_ts = self.timeline[anchor];
        let advanced = (elapsed_ms.max(0.0) * self.speed) as i64;
        let target_ts = anchor_ts.saturating_add(advanced);

        // Walk forward from the anchor to the last tick whose ts <= target_ts.
        let mut idx = anchor;
        while idx + 1 < self.timeline.len() && self.timeline[idx + 1] <= target_ts {
            idx += 1;
        }
        idx
    }

    /// `true` once `target_index(elapsed_ms)` has reached the final tick.
    pub fn reached_end(&self, elapsed_ms: f64) -> bool {
        !self.timeline.is_empty() && self.target_index(elapsed_ms) + 1 == self.timeline.len()
    }

    fn clamp_index(&self, idx: usize) -> usize {
        if self.timeline.is_empty() {
            0
        } else {
            idx.min(self.timeline.len() - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> PlaybackClock {
        // 100 ms between each tick.
        PlaybackClock::new(vec![1000, 1100, 1200, 1300])
    }

    #[test]
    fn starts_paused_at_speed_one() {
        let c = clock();
        assert_eq!(c.state(), PlaybackState::Paused);
        assert!(!c.is_playing());
        assert_eq!(c.speed(), 1.0);
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn play_pause_toggles_state() {
        let mut c = clock();
        c.play(0);
        assert!(c.is_playing());
        c.pause();
        assert!(!c.is_playing());
    }

    #[test]
    fn realtime_advance_maps_elapsed_to_ticks() {
        let mut c = clock(); // deltas of 100 ms
        c.play(0);
        assert_eq!(c.target_index(0.0), 0);
        assert_eq!(c.target_index(99.0), 0); // not yet at next tick
        assert_eq!(c.target_index(100.0), 1); // exactly one delta
        assert_eq!(c.target_index(250.0), 2);
        assert_eq!(c.target_index(300.0), 3);
    }

    #[test]
    fn speed_multiplier_scales_advancement() {
        let mut c = clock();
        c.set_speed(2.0);
        c.play(0);
        // 2x speed: 50 ms real == 100 ms exchange == one tick.
        assert_eq!(c.target_index(50.0), 1);
        assert_eq!(c.target_index(150.0), 3);

        c.set_speed(0.5);
        // 0.5x: need 200 ms real for one 100 ms tick.
        assert_eq!(c.target_index(199.0), 0);
        assert_eq!(c.target_index(200.0), 1);
    }

    #[test]
    fn clamps_at_end_of_session() {
        let mut c = clock();
        c.play(0);
        assert_eq!(c.target_index(10_000.0), 3);
        assert!(c.reached_end(10_000.0));
        assert!(!c.reached_end(0.0));
    }

    #[test]
    fn play_from_midpoint_anchors_there() {
        let mut c = clock();
        c.play(2); // anchor at ts 1200
        assert_eq!(c.anchor_index(), 2);
        assert_eq!(c.target_index(0.0), 2);
        assert_eq!(c.target_index(100.0), 3);
        assert_eq!(c.target_index(100_000.0), 3);
    }

    #[test]
    fn play_from_out_of_range_clamps_anchor() {
        let mut c = clock();
        c.play(99);
        assert_eq!(c.anchor_index(), 3);
        assert_eq!(c.target_index(0.0), 3);
    }

    #[test]
    fn empty_timeline_is_inert() {
        let mut c = PlaybackClock::new(vec![]);
        assert!(c.is_empty());
        c.play(0);
        assert_eq!(c.target_index(123.0), 0);
        assert!(!c.reached_end(123.0));
    }

    #[test]
    fn single_tick_timeline_stays_put() {
        let mut c = PlaybackClock::new(vec![5000]);
        c.play(0);
        assert_eq!(c.target_index(0.0), 0);
        assert_eq!(c.target_index(9999.0), 0);
        assert!(c.reached_end(9999.0));
    }

    #[test]
    fn set_speed_clamps_to_positive_range() {
        let mut c = clock();
        c.set_speed(0.0);
        assert!(c.speed() >= 0.01);
        c.set_speed(-5.0);
        assert!(c.speed() >= 0.01);
        c.set_speed(1e9);
        assert!(c.speed() <= 1000.0);
    }

    #[test]
    fn negative_elapsed_treated_as_zero() {
        let mut c = clock();
        c.play(1);
        assert_eq!(c.target_index(-50.0), 1);
    }
}
