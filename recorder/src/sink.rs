//! The [`EventSink`] abstraction: where recorded events are written.
//!
//! Orchestration depends on this trait rather than any concrete storage, so the
//! Parquet writer, an in-memory test double, or a future backend are all
//! interchangeable (Dependency Inversion).

use crate::events::RecordedEvent;

/// A destination for recorded events.
pub trait EventSink {
    /// Persist a single event.
    fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()>;

    /// Flush buffered rows to durable storage *without* finalizing. May be
    /// called periodically during a session (e.g. on a timer); the artifact
    /// stays open for more writes. The default is a no-op for sinks that do not
    /// buffer.
    fn flush(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Flush buffers and finalize the underlying artifact (e.g. write the
    /// Parquet footer). Called once at the end of a session.
    fn finalize(&mut self) -> anyhow::Result<()>;
}

/// Allow a boxed sink to be used wherever an `EventSink` is expected (lets
/// callers choose between e.g. single-file vs sharded storage at runtime).
impl EventSink for Box<dyn EventSink> {
    fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
        (**self).write(event)
    }
    fn flush(&mut self) -> anyhow::Result<()> {
        (**self).flush()
    }
    fn finalize(&mut self) -> anyhow::Result<()> {
        (**self).finalize()
    }
}

/// In-memory sink that simply collects events. Used by tests and as a reference
/// implementation of the trait contract.
#[derive(Debug, Default)]
pub struct MemorySink {
    pub events: Vec<RecordedEvent>,
    pub flushes: usize,
    pub finalized: bool,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventSink for MemorySink {
    fn write(&mut self, event: &RecordedEvent) -> anyhow::Result<()> {
        self.events.push(event.clone());
        Ok(())
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        self.flushes += 1;
        Ok(())
    }

    fn finalize(&mut self) -> anyhow::Result<()> {
        self.finalized = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, MarketEvent};

    fn sample(seq: u64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 0,
            ts_recv_ms: 0,
            payload: MarketEvent::AllMids(AllMids { mids: vec![] }),
        }
    }

    #[test]
    fn memory_sink_collects_and_finalizes() {
        let mut sink = MemorySink::new();
        sink.write(&sample(0)).unwrap();
        sink.write(&sample(1)).unwrap();
        sink.finalize().unwrap();
        assert_eq!(sink.events.len(), 2);
        assert!(sink.finalized);
    }
}
