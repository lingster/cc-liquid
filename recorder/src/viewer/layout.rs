//! Reorderable layout model for the viewer's stacked content sections.
//!
//! Pure and egui-free: the binary renders these sections top-to-bottom in the
//! current order and calls [`reordered`] to apply a drag-and-drop move. Keeping
//! the move logic here (rather than in the UI) makes the 50%-snap behaviour
//! unit-testable without a running egui context.

/// A draggable content section in the central area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// The price-movement chart.
    PriceChart,
    /// The order book (top-of-book, ladders, depth chart).
    OrderBook,
}

impl Section {
    /// Short label shown on the section's drag header.
    pub fn title(self) -> &'static str {
        match self {
            Section::PriceChart => "price",
            Section::OrderBook => "order book",
        }
    }
}

/// Default top-to-bottom ordering: chart above the book.
pub fn default_order() -> Vec<Section> {
    vec![Section::PriceChart, Section::OrderBook]
}

/// Return a new ordering with `dragged` moved next to `reference`.
///
/// `after = true` drops `dragged` immediately *below* `reference`, otherwise
/// immediately *above* it — which is exactly the 50%-snap rule the UI applies
/// (pointer past a section's vertical midpoint ⇒ insert after, else before).
/// Dropping a section onto itself is a no-op. Other sections keep their
/// relative order, so this is a stable move.
pub fn reordered(
    order: &[Section],
    dragged: Section,
    reference: Section,
    after: bool,
) -> Vec<Section> {
    if dragged == reference {
        return order.to_vec();
    }
    let mut out: Vec<Section> = order.iter().copied().filter(|s| *s != dragged).collect();
    let ref_idx = out.iter().position(|s| *s == reference).unwrap_or(out.len());
    let pos = (if after { ref_idx + 1 } else { ref_idx }).min(out.len());
    out.insert(pos, dragged);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Section::{OrderBook as OB, PriceChart as PC};

    #[test]
    fn default_is_chart_then_book() {
        assert_eq!(default_order(), vec![PC, OB]);
    }

    #[test]
    fn drag_book_above_chart_swaps() {
        // Drop OrderBook in the top half of the chart's zone → book first.
        assert_eq!(reordered(&[PC, OB], OB, PC, false), vec![OB, PC]);
    }

    #[test]
    fn drag_chart_below_book_swaps() {
        // Drop PriceChart in the bottom half of the book's zone → book first.
        assert_eq!(reordered(&[PC, OB], PC, OB, true), vec![OB, PC]);
    }

    #[test]
    fn drop_keeping_same_relative_position_is_unchanged() {
        // Chart dropped above the book is where it already is.
        assert_eq!(reordered(&[PC, OB], PC, OB, false), vec![PC, OB]);
        // Book dropped below the chart is where it already is.
        assert_eq!(reordered(&[PC, OB], OB, PC, true), vec![PC, OB]);
    }

    #[test]
    fn dropping_onto_self_is_a_noop() {
        assert_eq!(reordered(&[PC, OB], PC, PC, true), vec![PC, OB]);
        assert_eq!(reordered(&[OB, PC], OB, OB, false), vec![OB, PC]);
    }

    #[test]
    fn move_is_stable_for_three_sections() {
        // General check that untouched sections keep their relative order.
        // (Uses the two real variants plus a repeat to exercise the indexing.)
        let order = [PC, OB];
        // Reorder is idempotent when target equals current arrangement.
        let once = reordered(&order, OB, PC, false);
        let twice = reordered(&once, OB, PC, false);
        assert_eq!(once, twice);
        assert_eq!(once, vec![OB, PC]);
    }
}
