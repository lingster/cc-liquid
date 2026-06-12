//! The [`EventStream`] abstraction: an ordered source of recorded events.
//!
//! Replay depends on this trait, not on Parquet or any generator, so recorded
//! sessions and synthetic data are interchangeable (Dependency Inversion).
//! Events MUST be yielded in ascending `seq` order.

use crate::events::RecordedEvent;

/// A pull-based, ordered stream of events.
pub trait EventStream {
    /// Return the next event, or `None` when exhausted. Synthetic streams may be
    /// effectively infinite (always `Some`).
    fn next_event(&mut self) -> Option<RecordedEvent>;
}

/// An in-memory stream backed by a `Vec`, for tests and for streams already
/// materialized in memory (e.g. a loaded Parquet session).
pub struct VecEventStream {
    events: std::collections::VecDeque<RecordedEvent>,
}

impl VecEventStream {
    /// Build from events, sorting by `seq` to guarantee the ordering contract.
    pub fn new(mut events: Vec<RecordedEvent>) -> Self {
        events.sort_by_key(|e| e.seq);
        Self {
            events: events.into(),
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl EventStream for VecEventStream {
    fn next_event(&mut self) -> Option<RecordedEvent> {
        self.events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, MarketEvent};

    fn ev(seq: u64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: seq as i64,
            ts_recv_ms: seq as i64,
            payload: MarketEvent::AllMids(AllMids { mids: vec![] }),
        }
    }

    #[test]
    fn vec_stream_yields_in_seq_order_even_if_unsorted() {
        let mut s = VecEventStream::new(vec![ev(2), ev(0), ev(1)]);
        assert_eq!(s.next_event().unwrap().seq, 0);
        assert_eq!(s.next_event().unwrap().seq, 1);
        assert_eq!(s.next_event().unwrap().seq, 2);
        assert!(s.next_event().is_none());
    }
}
