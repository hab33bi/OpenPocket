//! Touch gesture recognition for the lock screen (docs/ROADMAP.md M2/M4).
//!
//! Movement-based classification, not time-based: every touch starts *Pending*
//! and becomes exactly one of
//! - **Tap** — lifted without ever moving past `MOVE_SLOP_PX` (any hold
//!   duration; a long stationary press is still a tap),
//! - **Drag** — moved past slop, started in the bottom grabber zone, and the
//!   motion is upward-dominant (diagonals allowed: up ≥ |dx|/2),
//! - **Rejected** — anything else; swallowed until lift-off.
//!
//! Debouncing: an emitted Tap opens a `TAP_DEBOUNCE_MS` refractory window in
//! which further taps are swallowed (the CST9217 can repeat reports around a
//! lift). Lift-off is normally an explicit evt=0 report; the timeout is only a
//! fallback and must be generous — the chip reports ~once per second for a
//! stationary finger.

use crate::drivers::cst9217::TouchPoint;

/// Movement beyond this (px, Chebyshev) reclassifies Pending → Drag/Rejected.
const MOVE_SLOP_PX: i32 = 14;
/// Bottom fraction of the panel (in 1/8ths) that arms the swipe-up grabber.
const ARM_ZONE_EIGHTHS: u32 = 2; // bottom 25%
/// Refractory window after an emitted tap.
const TAP_DEBOUNCE_MS: u32 = 350;
/// Minimum press duration for a tap — controllers can emit sub-frame ghost
/// press/lift pairs (observed: a phantom tap at the panel edge right after
/// init); a human tap is never this short.
const MIN_TAP_MS: u32 = 40;
/// Ignore all reports this long after boot/init — the CST9217 can emit a
/// spurious report while settling.
const STARTUP_SUPPRESS_MS: u32 = 800;
/// Fallback lift-off timeout (explicit lift reports are the normal path).
const RELEASE_TIMEOUT_MS: u32 = 1_500;

#[derive(Clone, Copy, PartialEq)]
pub enum GestureEvent {
    None,
    /// Slop exceeded upward from the grabber zone; drag tracking begins.
    DragStart { x: u16, y: u16 },
    /// Drag progressed: `dy` = upward distance from touch-down (≥ 0).
    DragMove { dy: u16 },
    /// Drag finished. `vel_q8` = upward px/ms in Q8 at release (negative =
    /// moving back down).
    DragEnd { dy: u16, vel_q8: i32 },
    /// Press + lift without significant movement (debounced).
    Tap { x: u16, y: u16 },
}

#[derive(Clone, Copy)]
struct Track {
    start_x: u16,
    start_y: u16,
    start_ms: u32,
    last_x: u16,
    last_y: u16,
    last_ms: u32,
    prev_y: u16,
    prev_ms: u32,
}

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    /// Touched, not yet classified (within slop).
    Pending(Track),
    /// Classified as the swipe-up drag.
    Dragging(Track),
    /// Classified as neither tap nor drag; swallow until lift.
    Rejected(Track),
}

pub struct SwipeTracker {
    phase: Phase,
    height: u16,
    /// Timestamp of the last emitted tap (refractory anchor).
    last_tap_ms: u32,
}

impl SwipeTracker {
    pub fn new(height: u16) -> Self {
        Self {
            phase: Phase::Idle,
            height,
            last_tap_ms: 0,
        }
    }

    /// Feed a report (`Some` when INT fired and the read succeeded) or an idle
    /// tick (`None`). Returns at most one event per call.
    pub fn feed(&mut self, report: Option<TouchPoint>, now_ms: u32) -> GestureEvent {
        // Settling window: the controller can emit spurious reports after init.
        if now_ms < STARTUP_SUPPRESS_MS {
            return GestureEvent::None;
        }
        match self.phase {
            Phase::Idle => {
                if let Some(t) = report {
                    if t.pressed {
                        self.phase = Phase::Pending(Track {
                            start_x: t.x,
                            start_y: t.y,
                            start_ms: now_ms,
                            last_x: t.x,
                            last_y: t.y,
                            last_ms: now_ms,
                            prev_y: t.y,
                            prev_ms: now_ms,
                        });
                    }
                }
                GestureEvent::None
            }

            Phase::Pending(mut tr) => match self.classify_input(&tr, report, now_ms) {
                Input::Move(t) => {
                    tr.prev_y = tr.last_y;
                    tr.prev_ms = tr.last_ms;
                    tr.last_x = t.x;
                    tr.last_y = t.y;
                    tr.last_ms = now_ms;

                    let dx = t.x as i32 - tr.start_x as i32;
                    let dy_up = tr.start_y as i32 - t.y as i32;
                    if dx.abs().max(dy_up.abs()) > MOVE_SLOP_PX {
                        let in_zone = tr.start_y as u32
                            >= self.height as u32 * (8 - ARM_ZONE_EIGHTHS) / 8;
                        // Upward-dominant (diagonals allowed).
                        if in_zone && dy_up > 0 && dy_up * 2 >= dx.abs() {
                            self.phase = Phase::Dragging(tr);
                            return GestureEvent::DragStart {
                                x: tr.start_x,
                                y: tr.start_y,
                            };
                        }
                        self.phase = Phase::Rejected(tr);
                    } else {
                        self.phase = Phase::Pending(tr);
                    }
                    GestureEvent::None
                }
                Input::Lift => {
                    self.phase = Phase::Idle;
                    // Ghost filter: sub-40ms press/lift pairs aren't human.
                    if now_ms.wrapping_sub(tr.start_ms) < MIN_TAP_MS {
                        return GestureEvent::None;
                    }
                    // Debounce: swallow taps inside the refractory window.
                    if now_ms.wrapping_sub(self.last_tap_ms) < TAP_DEBOUNCE_MS {
                        return GestureEvent::None;
                    }
                    self.last_tap_ms = now_ms;
                    GestureEvent::Tap {
                        x: tr.start_x,
                        y: tr.start_y,
                    }
                }
                Input::Nothing => GestureEvent::None,
            },

            Phase::Dragging(mut tr) => match self.classify_input(&tr, report, now_ms) {
                Input::Move(t) => {
                    tr.prev_y = tr.last_y;
                    tr.prev_ms = tr.last_ms;
                    tr.last_x = t.x;
                    tr.last_y = t.y;
                    tr.last_ms = now_ms;
                    self.phase = Phase::Dragging(tr);
                    GestureEvent::DragMove {
                        dy: tr.start_y.saturating_sub(tr.last_y),
                    }
                }
                Input::Lift => {
                    self.phase = Phase::Idle;
                    let dt = tr.last_ms.wrapping_sub(tr.prev_ms).max(1) as i32;
                    let dpx = tr.prev_y as i32 - tr.last_y as i32;
                    GestureEvent::DragEnd {
                        dy: tr.start_y.saturating_sub(tr.last_y),
                        vel_q8: (dpx << 8) / dt,
                    }
                }
                Input::Nothing => GestureEvent::None,
            },

            Phase::Rejected(tr) => match self.classify_input(&tr, report, now_ms) {
                Input::Lift => {
                    self.phase = Phase::Idle;
                    GestureEvent::None
                }
                Input::Move(t) => {
                    let mut tr = tr;
                    tr.last_x = t.x;
                    tr.last_y = t.y;
                    tr.last_ms = now_ms;
                    self.phase = Phase::Rejected(tr);
                    GestureEvent::None
                }
                Input::Nothing => GestureEvent::None,
            },
        }
    }

    fn classify_input(&self, tr: &Track, report: Option<TouchPoint>, now_ms: u32) -> Input {
        match report {
            Some(t) if t.pressed => Input::Move(t),
            Some(_) => Input::Lift, // explicit lift-off report (evt != contact)
            None if now_ms.wrapping_sub(tr.last_ms) > RELEASE_TIMEOUT_MS => Input::Lift,
            None => Input::Nothing,
        }
    }
}

enum Input {
    Move(TouchPoint),
    Lift,
    Nothing,
}
