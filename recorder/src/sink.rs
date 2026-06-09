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

    /// Flush buffers and finalize the underlying artifact. Called once at the
    /// end of a session.
    fn finalize(&mut self) -> anyhow::Result<()>;
}

/// In-memory sink that simply collects events. Used by tests and as a reference
/// implementation of the trait contract.
#[derive(Debug, Default)]
pub struct MemorySink {
    pub events: Vec<RecordedEvent>,
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
