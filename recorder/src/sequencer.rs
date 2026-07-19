//! Assigns deterministic ordering metadata to parsed market events.
//!
//! The sequencer is the single source of monotonic `seq` numbers. Keeping it
//! separate from transport makes ordering deterministic and unit-testable.

use crate::events::{MarketEvent, RecordedEvent};

/// Stateful sequence-number allocator.
#[derive(Debug, Default)]
pub struct Sequencer {
    next_seq: u64,
}

impl Sequencer {
    pub fn new() -> Self {
        Self { next_seq: 0 }
    }

    /// Start numbering at `first` instead of 0 — used when a restart appends
    /// to existing session files, so `seq` stays monotonic across the gap.
    pub fn starting_at(first: u64) -> Self {
        Self { next_seq: first }
    }

    /// Wrap a parsed event with a fresh `seq` and the supplied local receive
    /// time. The exchange event time is extracted from the payload when present,
    /// otherwise it falls back to `ts_recv_ms`.
    pub fn wrap(&mut self, payload: MarketEvent, ts_recv_ms: i64) -> RecordedEvent {
        let seq = self.next_seq;
        self.next_seq += 1;
        let ts_event_ms = exchange_time(&payload).unwrap_or(ts_recv_ms);
        RecordedEvent {
            seq,
            ts_event_ms,
            ts_recv_ms,
            payload,
        }
    }

    /// Number of events wrapped so far.
    pub fn count(&self) -> u64 {
        self.next_seq
    }
}

/// Best-effort extraction of the exchange-provided event time.
fn exchange_time(payload: &MarketEvent) -> Option<i64> {
    match payload {
        MarketEvent::L2Book(b) => Some(b.time_ms),
        MarketEvent::Trades(ts) => ts.iter().map(|t| t.time_ms).max(),
        // allMids carries no per-message exchange timestamp.
        MarketEvent::AllMids(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, L2Book};

    #[test]
    fn seq_is_monotonic_and_gap_free() {
        let mut s = Sequencer::new();
        let a = s.wrap(MarketEvent::AllMids(AllMids { mids: vec![] }), 100);
        let b = s.wrap(MarketEvent::AllMids(AllMids { mids: vec![] }), 101);
        assert_eq!(a.seq, 0);
        assert_eq!(b.seq, 1);
        assert_eq!(s.count(), 2);
    }

    #[test]
    fn starting_at_continues_an_existing_sequence() {
        let mut s = Sequencer::starting_at(100);
        let ev = s.wrap(MarketEvent::AllMids(AllMids { mids: vec![] }), 1);
        assert_eq!(ev.seq, 100);
        assert_eq!(s.count(), 101);
    }

    #[test]
    fn exchange_time_used_when_available() {
        let mut s = Sequencer::new();
        let ev = s.wrap(
            MarketEvent::L2Book(L2Book {
                coin: "BTC".into(),
                time_ms: 555,
                bids: vec![],
                asks: vec![],
            }),
            999,
        );
        assert_eq!(ev.ts_event_ms, 555);
        assert_eq!(ev.ts_recv_ms, 999);
    }

    #[test]
    fn falls_back_to_recv_time_when_no_exchange_time() {
        let mut s = Sequencer::new();
        let ev = s.wrap(MarketEvent::AllMids(AllMids { mids: vec![] }), 42);
        assert_eq!(ev.ts_event_ms, 42);
    }
}
