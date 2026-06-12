//! Clock abstraction controlling realtime playback pacing.
//!
//! Separating "how fast time passes" from "what the events are" lets realtime
//! replay sleep against the wall clock in production while tests use a
//! [`ManualClock`] that records requested sleeps without ever blocking —
//! keeping replay tests deterministic and instant.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

/// Controls how the replay engine waits between consecutive events.
#[async_trait]
pub trait Clock {
    /// Wait for (a possibly scaled) `ms` milliseconds of logical time.
    async fn sleep_ms(&self, ms: i64);
}

/// Wall-clock pacing with an optional speed multiplier (`1.0` = realtime,
/// `2.0` = twice as fast, etc.).
pub struct RealtimeClock {
    speed: f64,
}

impl RealtimeClock {
    pub fn new(speed: f64) -> Self {
        Self {
            speed: if speed > 0.0 { speed } else { 1.0 },
        }
    }
}

impl Default for RealtimeClock {
    fn default() -> Self {
        Self::new(1.0)
    }
}

#[async_trait]
impl Clock for RealtimeClock {
    async fn sleep_ms(&self, ms: i64) {
        if ms <= 0 {
            return;
        }
        let scaled = (ms as f64 / self.speed).round().max(0.0) as u64;
        if scaled > 0 {
            tokio::time::sleep(Duration::from_millis(scaled)).await;
        }
    }
}

/// Immediate clock: never waits. Used for tick-mode / as-fast-as-possible
/// playback where pacing is irrelevant.
#[derive(Default)]
pub struct NoopClock;

#[async_trait]
impl Clock for NoopClock {
    async fn sleep_ms(&self, _ms: i64) {}
}

/// Test clock: never actually sleeps, but records every requested duration so
/// tests can assert pacing deterministically.
#[derive(Default)]
pub struct ManualClock {
    sleeps: Mutex<Vec<i64>>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// All sleep durations requested so far, in order.
    pub fn sleeps(&self) -> Vec<i64> {
        self.sleeps.lock().unwrap().clone()
    }

    /// Total logical time elapsed across all sleeps.
    pub fn total_ms(&self) -> i64 {
        self.sleeps.lock().unwrap().iter().sum()
    }
}

#[async_trait]
impl Clock for ManualClock {
    async fn sleep_ms(&self, ms: i64) {
        self.sleeps.lock().unwrap().push(ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_clock_records_sleeps_without_blocking() {
        let clock = ManualClock::new();
        clock.sleep_ms(10).await;
        clock.sleep_ms(0).await;
        clock.sleep_ms(5).await;
        assert_eq!(clock.sleeps(), vec![10, 0, 5]);
        assert_eq!(clock.total_ms(), 15);
    }

    #[tokio::test]
    async fn realtime_clock_skips_nonpositive_waits() {
        // Should return immediately; mainly a smoke test that it doesn't panic.
        let clock = RealtimeClock::new(1.0);
        clock.sleep_ms(0).await;
        clock.sleep_ms(-5).await;
    }
}
