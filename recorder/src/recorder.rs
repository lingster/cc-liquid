//! Recording orchestration: pulls raw frames from an [`EventSource`], decodes
//! and sequences them, and writes them to an [`EventSink`].
//!
//! It depends only on the two trait abstractions, so it is fully unit-testable
//! with the scripted source and in-memory sink — no network or disk required.

use std::future::Future;
use std::time::Duration;

use tracing::warn;

use crate::events::{MarketEvent, Stream};
use crate::parser::parse_message;
use crate::sequencer::Sequencer;
use crate::sink::EventSink;
use crate::source::EventSource;

/// Per-session counters produced by a recording run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecordingStats {
    pub recorded: u64,
    pub ignored: u64,
    pub parse_errors: u64,
    pub all_mids: u64,
    pub l2_book: u64,
    pub trades: u64,
}

impl RecordingStats {
    fn tally(&mut self, stream: Stream) {
        match stream {
            Stream::AllMids => self.all_mids += 1,
            Stream::L2Book => self.l2_book += 1,
            Stream::Trades => self.trades += 1,
        }
    }
}

/// Await the next tick of an optional interval. When the interval is `None`
/// (time-based flushing disabled) this never resolves, so the corresponding
/// `select!` branch stays dormant. Cancellation-safe.
async fn tick_flush(timer: &mut Option<tokio::time::Interval>) {
    match timer {
        Some(t) => {
            t.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Default cadence for the periodic, time-based flush to disk.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(300);

/// Drives a single recording session.
pub struct Recorder {
    sequencer: Sequencer,
    stats: RecordingStats,
    max_events: Option<u64>,
    flush_interval: Option<Duration>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            sequencer: Sequencer::new(),
            stats: RecordingStats::default(),
            max_events: None,
            flush_interval: Some(DEFAULT_FLUSH_INTERVAL),
        }
    }

    /// Stop automatically after `n` events have been recorded (a count-based
    /// limit, complementary to the duration deadline).
    pub fn with_max_events(mut self, n: u64) -> Self {
        self.max_events = Some(n);
        self
    }

    /// Set the periodic flush cadence. `None` disables time-based flushing
    /// (data is still flushed at the row-count threshold and on finalize).
    pub fn with_flush_interval(mut self, interval: Option<Duration>) -> Self {
        self.flush_interval = interval;
        self
    }

    /// Record until the source is exhausted, returning the run statistics.
    ///
    /// `now_ms` supplies the local receive clock and is injected for
    /// deterministic tests. A single malformed frame is logged and counted but
    /// never aborts the session.
    pub async fn run<S, K, F>(
        &mut self,
        source: &mut S,
        sink: &mut K,
        now_ms: F,
    ) -> anyhow::Result<RecordingStats>
    where
        S: EventSource,
        K: EventSink,
        F: FnMut() -> i64,
    {
        // No deadline: stop only when the source ends.
        self.run_until(source, sink, now_ms, std::future::pending::<()>())
            .await
    }

    /// Record until either the source is exhausted or `stop` resolves
    /// (e.g. a duration timer), then finalize the sink. The sink is always
    /// finalized so partial recordings remain valid.
    pub async fn run_until<S, K, F, Fut>(
        &mut self,
        source: &mut S,
        sink: &mut K,
        mut now_ms: F,
        stop: Fut,
    ) -> anyhow::Result<RecordingStats>
    where
        S: EventSource,
        K: EventSink,
        F: FnMut() -> i64,
        Fut: Future<Output = ()>,
    {
        tokio::pin!(stop);

        // Periodic, time-based flush so buffered rows reach disk even when no
        // single buffer hits the row-count threshold (and to bound memory).
        let mut flush_timer = self.flush_interval.map(|period| {
            let mut t = tokio::time::interval(period);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            t
        });
        // Consume the immediate first tick so the first flush is one full period in.
        if let Some(t) = flush_timer.as_mut() {
            t.tick().await;
        }

        let result = loop {
            tokio::select! {
                // Prefer draining available messages; check the deadline between them.
                biased;
                msg = source.next_message() => {
                    match msg {
                        None => break Ok(()),
                        Some(Ok(raw)) => {
                            if let Err(e) = self.ingest(&raw, &mut now_ms, sink) {
                                break Err(e);
                            }
                            if let Some(max) = self.max_events {
                                if self.stats.recorded >= max {
                                    break Ok(());
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!("source error: {e}");
                            self.stats.parse_errors += 1;
                        }
                    }
                }
                _ = tick_flush(&mut flush_timer) => {
                    // A failed periodic flush is logged but never aborts the
                    // session; finalize will surface a persistent failure.
                    if let Err(e) = sink.flush() {
                        warn!("periodic flush failed: {e}");
                    }
                }
                _ = &mut stop => break Ok(()),
            }
        };

        // Always finalize, even on error, so a partial session is still usable.
        let finalize = sink.finalize();
        result.and(finalize)?;
        Ok(self.stats.clone())
    }

    /// Decode one raw frame and record it (or count it as ignored/errored).
    fn ingest<K, F>(&mut self, raw: &str, now_ms: &mut F, sink: &mut K) -> anyhow::Result<()>
    where
        K: EventSink,
        F: FnMut() -> i64,
    {
        match parse_message(raw) {
            Ok(Some(event)) => self.record(event, now_ms, sink)?,
            Ok(None) => self.stats.ignored += 1,
            Err(e) => {
                warn!("parse error: {e}");
                self.stats.parse_errors += 1;
            }
        }
        Ok(())
    }

    fn record<K, F>(
        &mut self,
        event: MarketEvent,
        now_ms: &mut F,
        sink: &mut K,
    ) -> anyhow::Result<()>
    where
        K: EventSink,
        F: FnMut() -> i64,
    {
        let stream = event.stream();
        let wrapped = self.sequencer.wrap(event, now_ms());
        sink.write(&wrapped)?;
        self.stats.recorded += 1;
        self.stats.tally(stream);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::MemorySink;
    use crate::source::ScriptedSource;

    #[tokio::test]
    async fn records_data_messages_and_skips_control_frames() {
        let mut source = ScriptedSource::new(vec![
            r#"{"channel":"allMids","data":{"mids":{"BTC":"100"}}}"#.into(),
            r#"{"channel":"pong"}"#.into(),
            r#"{"channel":"trades","data":[{"coin":"BTC","side":"B","px":"100","sz":"1","time":1}]}"#.into(),
        ]);
        let mut sink = MemorySink::new();
        let mut counter = 0i64;

        let stats = Recorder::new()
            .run(&mut source, &mut sink, || {
                counter += 1;
                counter
            })
            .await
            .unwrap();

        assert_eq!(stats.recorded, 2);
        assert_eq!(stats.ignored, 1);
        assert_eq!(stats.all_mids, 1);
        assert_eq!(stats.trades, 1);
        assert_eq!(sink.events.len(), 2);
        assert!(sink.finalized);
        // seq is monotonic and gap-free across recorded events
        assert_eq!(sink.events[0].seq, 0);
        assert_eq!(sink.events[1].seq, 1);
    }

    /// Source that yields its queued messages, then blocks forever — used to
    /// prove the deadline (not the source) stops the session.
    struct DrainThenPend {
        queue: std::collections::VecDeque<String>,
    }

    #[async_trait::async_trait]
    impl EventSource for DrainThenPend {
        async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
            match self.queue.pop_front() {
                Some(m) => Some(Ok(m)),
                None => std::future::pending().await,
            }
        }
    }

    #[tokio::test]
    async fn deadline_stops_recording_and_finalizes() {
        let mut source = DrainThenPend {
            queue: vec![r#"{"channel":"allMids","data":{"mids":{"BTC":"1"}}}"#.into()].into(),
        };
        let mut sink = MemorySink::new();

        // Stop fires only after the single message has been drained (source pends).
        let stats = Recorder::new()
            .run_until(&mut source, &mut sink, || 0, std::future::ready(()))
            .await
            .unwrap();

        assert_eq!(stats.recorded, 1);
        assert!(
            sink.finalized,
            "sink must be finalized even when stopped by deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_flush_fires_on_interval_then_finalizes() {
        // Source drains one message then pends, so only timers drive the run;
        // with the clock paused, tokio auto-advances between ticks.
        let mut source = DrainThenPend {
            queue: vec![r#"{"channel":"allMids","data":{"mids":{"BTC":"1"}}}"#.into()].into(),
        };
        let mut sink = MemorySink::new();

        // Flush every second; stop at 3.5s -> ticks at 1s, 2s, 3s.
        let stats = Recorder::new()
            .with_flush_interval(Some(Duration::from_secs(1)))
            .run_until(
                &mut source,
                &mut sink,
                || 0,
                tokio::time::sleep(Duration::from_millis(3500)),
            )
            .await
            .unwrap();

        assert_eq!(stats.recorded, 1);
        assert_eq!(sink.flushes, 3, "one periodic flush per elapsed interval");
        assert!(sink.finalized, "finalize still runs after the periodic flushes");
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_flush_interval_does_not_flush_but_still_finalizes() {
        let mut source = DrainThenPend {
            queue: vec![r#"{"channel":"allMids","data":{"mids":{"BTC":"1"}}}"#.into()].into(),
        };
        let mut sink = MemorySink::new();

        let _ = Recorder::new()
            .with_flush_interval(None)
            .run_until(
                &mut source,
                &mut sink,
                || 0,
                tokio::time::sleep(Duration::from_secs(5)),
            )
            .await
            .unwrap();

        assert_eq!(sink.flushes, 0, "no periodic flush when disabled");
        assert!(sink.finalized);
    }

    #[tokio::test]
    async fn a_bad_frame_does_not_abort_the_session() {
        let mut source = ScriptedSource::new(vec![
            "garbage".into(),
            r#"{"channel":"allMids","data":{"mids":{"BTC":"100"}}}"#.into(),
        ]);
        let mut sink = MemorySink::new();

        let stats = Recorder::new()
            .run(&mut source, &mut sink, || 0)
            .await
            .unwrap();

        assert_eq!(stats.parse_errors, 1);
        assert_eq!(stats.recorded, 1);
    }
}
