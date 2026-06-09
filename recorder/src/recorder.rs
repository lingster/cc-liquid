//! Recording orchestration: pulls raw frames from an [`EventSource`], decodes
//! and sequences them, and writes them to an [`EventSink`].
//!
//! It depends only on the two trait abstractions, so it is fully unit-testable
//! with the scripted source and in-memory sink — no network or disk required.

use std::future::Future;

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

/// Drives a single recording session.
pub struct Recorder {
    sequencer: Sequencer,
    stats: RecordingStats,
    max_events: Option<u64>,
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
        }
    }

    /// Stop automatically after `n` events have been recorded (a count-based
    /// limit, complementary to the duration deadline).
    pub fn with_max_events(mut self, n: u64) -> Self {
        self.max_events = Some(n);
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
