//! Daily-rotating sink decorator: one set of Parquet files per UTC day.
//!
//! Long (multi-day) recordings should not hold a single unfinalized Parquet
//! file open for days — a crash would lose everything since the last footer.
//! [`RotatingSink`] wraps any inner [`EventSink`] factory and rotates at
//! midnight UTC, keyed on each event's `ts_recv_ms`. Files for a day carry a
//! `YYYYMMDD_` prefix (e.g. `20260610_all_mids.parquet`).
//!
//! **No data is lost at the boundary**: the new day's sink is created *first*
//! and starts buffering immediately; the previous day's sink — including any
//! rows still buffered in memory — is handed to a **background thread** that
//! drains it and writes its footer. Ingestion never blocks on the midnight
//! finalize, and the ZSTD/IO work of closing a day runs in parallel with the
//! new day's capture (in addition to the per-day L2 shard workers when the
//! inner sink is a `ShardedParquetSink`).

use std::thread::JoinHandle;

use chrono::DateTime;
use tracing::{info, warn};

use crate::events::RecordedEvent;
use crate::sink::EventSink;

/// A boxed sink that can be finalized on a background thread.
pub type BoxedSendSink = Box<dyn EventSink + Send>;

const MS_PER_DAY: i64 = 86_400_000;

/// UTC day index of a millisecond timestamp (days since the epoch).
pub fn day_index(ts_ms: i64) -> i64 {
    ts_ms.div_euclid(MS_PER_DAY)
}

/// `YYYYMMDD_` file prefix for the UTC day containing `ts_ms`.
pub fn day_prefix(ts_ms: i64) -> String {
    let dt = DateTime::from_timestamp_millis(ts_ms).unwrap_or_default();
    dt.format("%Y%m%d_").to_string()
}

/// Wraps a sink factory and rotates the inner sink at midnight UTC.
pub struct RotatingSink<F>
where
    F: FnMut(&str) -> anyhow::Result<BoxedSendSink>,
{
    factory: F,
    current_day: Option<i64>,
    current: Option<BoxedSendSink>,
    /// Background finalizers for closed days, joined on `finalize` (and
    /// opportunistically reaped on `flush`).
    closing: Vec<(String, JoinHandle<anyhow::Result<()>>)>,
    /// First error surfaced by a background finalizer.
    background_error: Option<anyhow::Error>,
}

impl<F> RotatingSink<F>
where
    F: FnMut(&str) -> anyhow::Result<BoxedSendSink>,
{
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            current_day: None,
            current: None,
            closing: Vec::new(),
            background_error: None,
        }
    }

    /// Ensure `current` targets the event's UTC day. Rotation is forward-only:
    /// an event whose timestamp falls *before* the current day (clock skew)
    /// stays in the currently open files rather than reopening a closed day.
    fn ensure_sink_for(&mut self, ts_ms: i64) -> anyhow::Result<()> {
        let day = day_index(ts_ms);
        match self.current_day {
            Some(current) if day <= current => return Ok(()),
            _ => {}
        }
        let prefix = day_prefix(ts_ms);
        // Open the new day FIRST: if the factory fails we keep writing to the
        // old sink and surface the error, instead of dropping events.
        let new_sink = (self.factory)(&prefix)?;
        if let Some(old) = self.current.replace(new_sink) {
            let old_prefix = day_prefix(self.current_day.unwrap_or(day - 1) * MS_PER_DAY);
            info!("rotating session files: {old_prefix}* -> {prefix}* (finalizing in background)");
            self.closing.push((
                old_prefix,
                std::thread::spawn(move || {
                    let mut old = old;
                    old.finalize()
                }),
            ));
        } else {
            info!("opening session files with prefix {prefix}*");
        }
        self.current_day = Some(day);
        Ok(())
    }

    /// Join background finalizers that have already finished, capturing the
    /// first error. Non-blocking with respect to still-running closers.
    fn reap_finished(&mut self) {
        let (finished, still_running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.closing)
            .into_iter()
            .partition(|(_, handle)| handle.is_finished());
        self.closing = still_running;
        for (prefix, handle) in finished {
            self.record_join(prefix, handle);
        }
    }

    fn record_join(&mut self, prefix: String, handle: JoinHandle<anyhow::Result<()>>) {
        let result = handle
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("day finalizer thread panicked")));
        if let Err(e) = result {
            warn!("finalizing {prefix}* failed: {e:#}");
            self.background_error
                .get_or_insert(e.context(format!("finalizing {prefix}* in background")));
        }
    }
}

impl<F> EventSink for RotatingSink<F>
where
    F: FnMut(&str) -> anyhow::Result<BoxedSendSink>,
{
    fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
        self.ensure_sink_for(event.ts_recv_ms)?;
        self.current
            .as_mut()
            .expect("ensure_sink_for installs a sink")
            .write(event)
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        self.reap_finished();
        if let Some(sink) = self.current.as_mut() {
            sink.flush()?;
        }
        Ok(())
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        if let Some(mut sink) = self.current.take() {
            sink.finalize()?;
        }
        for (prefix, handle) in std::mem::take(&mut self.closing) {
            self.record_join(prefix, handle);
        }
        match self.background_error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, MarketEvent};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// Shared-state sink so tests can observe writes/finalizes across the
    /// move into the rotating sink and its background threads.
    #[derive(Clone, Default)]
    struct ProbeSink {
        seqs: Arc<Mutex<Vec<u64>>>,
        finalized: Arc<AtomicBool>,
        fail_finalize: bool,
    }

    impl EventSink for ProbeSink {
        fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
            self.seqs.lock().unwrap().push(event.seq);
            Ok(())
        }
        fn finalize(&mut self) -> anyhow::Result<()> {
            self.finalized.store(true, Ordering::SeqCst);
            anyhow::ensure!(!self.fail_finalize, "boom");
            Ok(())
        }
    }

    fn ev(seq: u64, ts_recv_ms: i64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: ts_recv_ms,
            ts_recv_ms,
            payload: MarketEvent::AllMids(AllMids { mids: vec![] }),
        }
    }

    /// 2026-06-10T23:59:59.999Z and the next millisecond.
    const BEFORE_MIDNIGHT: i64 = 1781135999999;
    const AFTER_MIDNIGHT: i64 = 1781136000000;

    #[test]
    fn prefix_formats_utc_days() {
        assert_eq!(day_prefix(0), "19700101_");
        assert_eq!(day_prefix(BEFORE_MIDNIGHT), "20260610_");
        assert_eq!(day_prefix(AFTER_MIDNIGHT), "20260611_");
        assert_eq!(day_index(BEFORE_MIDNIGHT) + 1, day_index(AFTER_MIDNIGHT));
    }

    type CreatedProbes = Arc<Mutex<Vec<(String, ProbeSink)>>>;

    fn rotating_with_probes() -> (
        RotatingSink<impl FnMut(&str) -> anyhow::Result<BoxedSendSink>>,
        CreatedProbes,
    ) {
        let created: CreatedProbes = Arc::default();
        let created_in = created.clone();
        let sink = RotatingSink::new(move |prefix: &str| -> anyhow::Result<BoxedSendSink> {
            let probe = ProbeSink::default();
            created_in
                .lock()
                .unwrap()
                .push((prefix.to_string(), probe.clone()));
            Ok(Box::new(probe))
        });
        (sink, created)
    }

    #[test]
    fn same_day_events_share_one_sink() {
        let (mut sink, created) = rotating_with_probes();
        sink.write(&ev(0, BEFORE_MIDNIGHT - 1000)).unwrap();
        sink.write(&ev(1, BEFORE_MIDNIGHT)).unwrap();
        sink.finalize().unwrap();
        let created = created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "20260610_");
        assert_eq!(*created[0].1.seqs.lock().unwrap(), vec![0, 1]);
        assert!(created[0].1.finalized.load(Ordering::SeqCst));
    }

    #[test]
    fn midnight_rotation_preserves_every_event_and_finalizes_old_day() {
        let (mut sink, created) = rotating_with_probes();
        sink.write(&ev(0, BEFORE_MIDNIGHT)).unwrap();
        sink.write(&ev(1, AFTER_MIDNIGHT)).unwrap(); // first event of new day
        sink.write(&ev(2, AFTER_MIDNIGHT + 5)).unwrap();
        sink.finalize().unwrap();

        let created = created.lock().unwrap();
        assert_eq!(created.len(), 2);
        assert_eq!(created[0].0, "20260610_");
        assert_eq!(created[1].0, "20260611_");
        // Every event landed in exactly one day's sink, none dropped.
        assert_eq!(*created[0].1.seqs.lock().unwrap(), vec![0]);
        assert_eq!(*created[1].1.seqs.lock().unwrap(), vec![1, 2]);
        // The old day was finalized (by the background closer).
        assert!(created[0].1.finalized.load(Ordering::SeqCst));
        assert!(created[1].1.finalized.load(Ordering::SeqCst));
    }

    #[test]
    fn rotation_is_forward_only_under_clock_skew() {
        let (mut sink, created) = rotating_with_probes();
        sink.write(&ev(0, AFTER_MIDNIGHT)).unwrap();
        sink.write(&ev(1, BEFORE_MIDNIGHT)).unwrap(); // skewed backwards
        sink.finalize().unwrap();
        let created = created.lock().unwrap();
        assert_eq!(created.len(), 1, "no rotation backwards");
        assert_eq!(*created[0].1.seqs.lock().unwrap(), vec![0, 1]);
    }

    #[test]
    fn background_finalize_error_surfaces_at_finalize() {
        let mut day = 0;
        let mut sink = RotatingSink::new(move |_prefix: &str| -> anyhow::Result<BoxedSendSink> {
            day += 1;
            Ok(Box::new(ProbeSink {
                fail_finalize: day == 1, // first day's footer write fails
                ..ProbeSink::default()
            }))
        });
        sink.write(&ev(0, BEFORE_MIDNIGHT)).unwrap();
        sink.write(&ev(1, AFTER_MIDNIGHT)).unwrap();
        let err = sink.finalize().unwrap_err();
        assert!(err.to_string().contains("20260610_"), "{err:#}");
    }

    #[test]
    fn factory_failure_keeps_old_sink_and_surfaces_error() {
        let mut calls = 0;
        let probe = ProbeSink::default();
        let probe_out = probe.clone();
        let mut sink = RotatingSink::new(move |_prefix: &str| -> anyhow::Result<BoxedSendSink> {
            calls += 1;
            anyhow::ensure!(calls == 1, "disk full");
            Ok(Box::new(probe_out.clone()))
        });
        sink.write(&ev(0, BEFORE_MIDNIGHT - 1)).unwrap();
        assert!(sink.write(&ev(1, AFTER_MIDNIGHT)).is_err());
        // The old sink is still installed; same-day writes keep working.
        sink.write(&ev(2, BEFORE_MIDNIGHT)).unwrap();
        assert_eq!(*probe.seqs.lock().unwrap(), vec![0, 2]);
    }
}
