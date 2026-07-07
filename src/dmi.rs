//! DMI — Dirty Metadata Index (docs/09 P1/P3).
//!
//! Compact SRAM index of dirty display rows for partial DMA flush. Composers
//! record damage as rects/spans while writing the retained framebuffer; the
//! partial-flush path (P3) windows the panel per span and DMAs only those
//! bytes, falling back to a full-frame flush when the index overflows.
//! Fixed-capacity array — no heap, no allocation in hot paths.

/// Inclusive horizontal run of dirty pixels on one row.
#[derive(Clone, Copy, Default)]
pub struct Span {
    pub y: u16,
    pub x0: u16,
    /// Inclusive.
    pub x1: u16,
}

/// 512 spans × 6 B ≈ 3 KiB SRAM. Sized so a text bbox (~150 rows) plus both
/// arc end windows fit without overflowing on typical anim frames.
pub const MAX_SPANS: usize = 512;

pub struct DmiIndex {
    spans: [Span; MAX_SPANS],
    count: usize,
    overflow: bool,
}

impl DmiIndex {
    pub const fn new() -> Self {
        Self {
            spans: [Span { y: 0, x0: 0, x1: 0 }; MAX_SPANS],
            count: 0,
            overflow: false,
        }
    }

    pub fn clear(&mut self) {
        self.count = 0;
        self.overflow = false;
    }

    /// True when no damage at all was recorded this frame.
    pub fn is_empty(&self) -> bool {
        self.count == 0 && !self.overflow
    }

    /// Capacity exceeded — caller must full-frame flush.
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans[..self.count]
    }

    /// Record one row span, merging into the previous span when it touches or
    /// overlaps it on the same row (composers emit row-ordered damage, so this
    /// catches most adjacency without a sort).
    pub fn add_span(&mut self, y: u16, x0: u16, x1: u16) {
        if self.overflow || x1 < x0 {
            return;
        }
        if self.count > 0 {
            let last = &mut self.spans[self.count - 1];
            if last.y == y && x0 <= last.x1.saturating_add(1) && last.x0 <= x1.saturating_add(1) {
                last.x0 = last.x0.min(x0);
                last.x1 = last.x1.max(x1);
                return;
            }
        }
        if self.count == MAX_SPANS {
            self.overflow = true;
            return;
        }
        self.spans[self.count] = Span { y, x0, x1 };
        self.count += 1;
    }

    /// Record an inclusive rect as one span per row.
    pub fn add_rect(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) {
        for y in y0..=y1 {
            self.add_span(y, x0, x1);
            if self.overflow {
                return;
            }
        }
    }
}
