//! Swipe-up recognizer for the lock screen (docs/ROADMAP.md M2/M4).
//!
//! Feed it every touch report (and periodic `None`-when-idle ticks so it can
//! time out a lost lift-off). Emits drag lifecycle events; the swipe-up arms
//! only when the touch lands in the bottom `ARM_ZONE_FRAC` of the panel.
//! Velocity is tracked from the last two samples in Q8 px/ms for the
//! release-decision (complete vs spring back).

use crate::drivers::cst9217::TouchPoint;

/// Bottom fraction of the panel (in 1/8ths) that arms the swipe-up grabber.
const ARM_ZONE_EIGHTHS: u32 = 2; // bottom 25%
/// No report for this long during a touch → treat as lift-off.
const RELEASE_TIMEOUT_MS: u32 = 120;
/// Max movement (px) and duration (ms) for a tap.
const TAP_SLOP_PX: i32 = 12;
const TAP_MAX_MS: u32 = 300;

#[derive(Clone, Copy, PartialEq)]
pub enum GestureEvent {
    None,
    /// Touch landed in the grabber zone; drag tracking begins.
    DragStart { x: u16, y: u16 },
    /// Drag progressed: `dy` = upward distance from the touch-down point (≥ 0).
    DragMove { dy: u16 },
    /// Finger lifted (or timed out) during a drag. `vel_q8` = upward px/ms in
    /// Q8 at release (negative = moving back down).
    DragEnd { dy: u16, vel_q8: i32 },
    /// Short, small-movement touch anywhere outside a drag.
    Tap { x: u16, y: u16 },
}

#[derive(Clone, Copy)]
enum State {
    Idle,
    /// Touch in progress. `armed` = started in the grabber zone.
    Touching {
        start_x: u16,
        start_y: u16,
        start_ms: u32,
        armed: bool,
        last_y: u16,
        last_ms: u32,
        prev_y: u16,
        prev_ms: u32,
    },
}

pub struct SwipeTracker {
    state: State,
    height: u16,
}

impl SwipeTracker {
    pub fn new(height: u16) -> Self {
        Self {
            state: State::Idle,
            height,
        }
    }

    /// Feed a report (`Some` when the INT fired and the read succeeded) or an
    /// idle tick (`None`). Returns at most one event per call.
    pub fn feed(&mut self, report: Option<TouchPoint>, now_ms: u32) -> GestureEvent {
        match (self.state, report) {
            (State::Idle, Some(t)) if t.pressed => {
                let armed = t.y as u32 >= self.height as u32 * (8 - ARM_ZONE_EIGHTHS) / 8;
                self.state = State::Touching {
                    start_x: t.x,
                    start_y: t.y,
                    start_ms: now_ms,
                    armed,
                    last_y: t.y,
                    last_ms: now_ms,
                    prev_y: t.y,
                    prev_ms: now_ms,
                };
                if armed {
                    GestureEvent::DragStart { x: t.x, y: t.y }
                } else {
                    GestureEvent::None
                }
            }
            (State::Idle, _) => GestureEvent::None,
            (
                State::Touching {
                    start_x,
                    start_y,
                    start_ms,
                    armed,
                    last_y,
                    last_ms,
                    ..
                },
                Some(t),
            ) if t.pressed => {
                self.state = State::Touching {
                    start_x,
                    start_y,
                    start_ms,
                    armed,
                    last_y: t.y,
                    last_ms: now_ms,
                    prev_y: last_y,
                    prev_ms: last_ms,
                };
                if armed {
                    GestureEvent::DragMove {
                        dy: start_y.saturating_sub(t.y),
                    }
                } else {
                    GestureEvent::None
                }
            }
            // Lift-off report, or report timeout while touching.
            (
                State::Touching {
                    start_x,
                    start_y,
                    start_ms,
                    armed,
                    last_y,
                    last_ms,
                    prev_y,
                    prev_ms,
                },
                r,
            ) => {
                let lifted = matches!(r, Some(t) if !t.pressed)
                    || (r.is_none() && now_ms.wrapping_sub(last_ms) > RELEASE_TIMEOUT_MS);
                if !lifted {
                    return GestureEvent::None;
                }
                self.state = State::Idle;
                let dy = start_y.saturating_sub(last_y);
                if armed {
                    // Upward velocity from the last two samples, Q8 px/ms.
                    let dt = last_ms.wrapping_sub(prev_ms).max(1) as i32;
                    let dpx = prev_y as i32 - last_y as i32;
                    GestureEvent::DragEnd {
                        dy,
                        vel_q8: (dpx << 8) / dt,
                    }
                } else {
                    let moved = (start_y as i32 - last_y as i32).abs().max(
                        // x slop uses the release sample's x only via start; y dominates
                        0,
                    );
                    if moved <= TAP_SLOP_PX && now_ms.wrapping_sub(start_ms) <= TAP_MAX_MS {
                        GestureEvent::Tap {
                            x: start_x,
                            y: start_y,
                        }
                    } else {
                        GestureEvent::None
                    }
                }
            }
        }
    }
}
