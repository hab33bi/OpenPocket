//! WatchFb — bespoke retained framebuffer for the watch UI (docs/09 P1).
//!
//! One RGB565 big-endian PSRAM canvas, retained across frames: composers
//! (clock ring/text) write incremental deltas only — no per-frame clear and
//! no 434 KiB ping-pong copy (the copy existed solely to carry ring pixels;
//! retention does that for free with a blocking flush). All damage is
//! recorded in a [`DmiIndex`] so the caller can skip the flush on clean
//! frames today and window partial flushes per span in P3.
//! No heap allocation after construction; hot paths write straight to the slice.

use crate::display::dmi::DmiIndex;

pub struct WatchFb<'a> {
    buf: &'a mut [u8],
    pub dmi: DmiIndex,
    width: u16,
    height: u16,
}

impl<'a> WatchFb<'a> {
    /// Wrap a caller-owned buffer (PSRAM vec or static). Primes it to black and
    /// marks the full frame dirty so the first flush pushes the cleared canvas.
    pub fn new(buf: &'a mut [u8], width: u16, height: u16) -> Self {
        buf.fill(0);
        let mut fb = Self {
            buf,
            dmi: DmiIndex::new(),
            width,
            height,
        };
        fb.mark_rect(0, 0, width as i32 - 1, height as i32 - 1);
        fb
    }

    /// Display-ready bytes for flushing.
    pub fn bytes(&self) -> &[u8] {
        self.buf
    }

    /// Raw retained canvas for composers. Record what you touch via `mark_rect`.
    pub fn buf_mut(&mut self) -> &mut [u8] {
        self.buf
    }

    /// Inclusive rect damage, clamped to the panel. Empty/inverted rects ignored.
    pub fn mark_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let x0 = x0.max(0);
        let y0 = y0.max(0);
        let x1 = x1.min(self.width as i32 - 1);
        let y1 = y1.min(self.height as i32 - 1);
        if x1 < x0 || y1 < y0 {
            return;
        }
        self.dmi.add_rect(x0 as u16, y0 as u16, x1 as u16, y1 as u16);
    }

    /// Nothing dirty this frame → the caller can skip the flush entirely
    /// (the CO5300 retains its own GRAM).
    pub fn is_clean(&self) -> bool {
        self.dmi.is_empty()
    }

    /// Reset damage after a flush.
    pub fn clear_damage(&mut self) {
        self.dmi.clear();
    }
}

/// Grow-only rect accumulator for coarse per-frame damage (e.g. the ring's
/// touched region), converted to one `mark_rect` at frame end.
#[derive(Clone, Copy)]
pub struct RectAcc {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl RectAcc {
    pub const fn empty() -> Self {
        Self {
            x0: i32::MAX,
            y0: i32::MAX,
            x1: i32::MIN,
            y1: i32::MIN,
        }
    }

    #[inline]
    pub fn add(&mut self, x: i32, y: i32) {
        if x < self.x0 {
            self.x0 = x;
        }
        if x > self.x1 {
            self.x1 = x;
        }
        if y < self.y0 {
            self.y0 = y;
        }
        if y > self.y1 {
            self.y1 = y;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 < self.x0
    }
}
