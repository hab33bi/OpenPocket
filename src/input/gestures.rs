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
const MOVE_SLOP_PX: i32 = 10;
/// Edge fraction of the panel (in 1/8ths) that arms a swipe: swipe-up must
/// start in the bottom zone, swipe-down in the top zone.
const ARM_ZONE_EIGHTHS: u32 = 3; // 37.5% at each edge — a generous grab
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

/// Swipe direction (determines which edge zone arms it).
#[derive(Clone, Copy, PartialEq)]
pub enum SwipeDir {
    /// Upward swipe, armed from the bottom zone (unlock).
    Up,
    /// Downward swipe, armed from the top zone (relock).
    Down,
}

#[derive(Clone, Copy, PartialEq)]
pub enum GestureEvent {
    None,
    /// Slop exceeded along `dir` from its arm zone; drag tracking begins.
    /// `dist` = travel already covered at classification, so the sheet can
    /// move on the very first classified sample.
    DragStart { dir: SwipeDir, x: u16, y: u16, dist: u16 },
    /// Drag progressed: `dist` = travel along `dir` from touch-down (≥ 0).
    DragMove { dir: SwipeDir, dist: u16 },
    /// Drag finished. `vel_q8` = px/ms along `dir` in Q8 at release
    /// (negative = moving back against the swipe).
    DragEnd { dir: SwipeDir, dist: u16, vel_q8: i32 },
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
    /// Classified as a swipe drag along a direction.
    Dragging(Track, SwipeDir),
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

    /// Whether a touch is currently being tracked (any non-Idle phase). The
    /// poll loop uses this to switch from INT-gated to fixed-rate reads: INT
    /// pulses are missed during blocking compose/flush windows, and a missed
    /// lift-off otherwise stalls the release until the fallback timeout.
    pub fn finger_down(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
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
                        if let Some(dir) = self.classify_dir(tr.start_y, dx, dy_up) {
                            self.phase = Phase::Dragging(tr, dir);
                            return GestureEvent::DragStart {
                                dir,
                                x: tr.start_x,
                                y: tr.start_y,
                                dist: dist_along(&tr, dir),
                            };
                        }
                        self.phase = Phase::Rejected(tr);
                    } else {
                        self.phase = Phase::Pending(tr);
                    }
                    GestureEvent::None
                }
                Input::Lift(lift) => {
                    self.phase = Phase::Idle;
                    // Flick salvage: a fast swipe can fit entirely inside one
                    // blocked compose/flush window, so the only samples seen
                    // are the latched touch-down and this lift report. The
                    // lift report carries the release coordinates — classify
                    // the whole gesture from them instead of degrading to a
                    // tap at the touch-down point.
                    if let Some(t) = lift {
                        let dx = t.x as i32 - tr.start_x as i32;
                        let dy_up = tr.start_y as i32 - t.y as i32;
                        if dx.abs().max(dy_up.abs()) > MOVE_SLOP_PX {
                            if let Some(dir) = self.classify_dir(tr.start_y, dx, dy_up) {
                                let dist = match dir {
                                    SwipeDir::Up => tr.start_y.saturating_sub(t.y),
                                    SwipeDir::Down => t.y.saturating_sub(tr.start_y),
                                };
                                let dt = now_ms.wrapping_sub(tr.start_ms).max(1) as i32;
                                return GestureEvent::DragEnd {
                                    dir,
                                    dist,
                                    vel_q8: ((dist as i32) << 8) / dt,
                                };
                            }
                            // Moved past slop but not a swipe: not a tap either.
                            return GestureEvent::None;
                        }
                    }
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

            Phase::Dragging(mut tr, dir) => match self.classify_input(&tr, report, now_ms) {
                Input::Move(t) => {
                    tr.prev_y = tr.last_y;
                    tr.prev_ms = tr.last_ms;
                    tr.last_x = t.x;
                    tr.last_y = t.y;
                    tr.last_ms = now_ms;
                    self.phase = Phase::Dragging(tr, dir);
                    GestureEvent::DragMove {
                        dir,
                        dist: dist_along(&tr, dir),
                    }
                }
                Input::Lift(lift) => {
                    self.phase = Phase::Idle;
                    // Fold the lift report's coordinates in as the final
                    // movement sample — a fast swipe's last real travel often
                    // arrives only in the lift report.
                    if let Some(t) = lift {
                        if t.x != tr.last_x || t.y != tr.last_y {
                            tr.prev_y = tr.last_y;
                            tr.prev_ms = tr.last_ms;
                            tr.last_x = t.x;
                            tr.last_y = t.y;
                            tr.last_ms = now_ms;
                        }
                    }
                    let dt = tr.last_ms.wrapping_sub(tr.prev_ms).max(1) as i32;
                    let dpx = match dir {
                        SwipeDir::Up => tr.prev_y as i32 - tr.last_y as i32,
                        SwipeDir::Down => tr.last_y as i32 - tr.prev_y as i32,
                    };
                    GestureEvent::DragEnd {
                        dir,
                        dist: dist_along(&tr, dir),
                        vel_q8: (dpx << 8) / dt,
                    }
                }
                Input::Nothing => GestureEvent::None,
            },

            Phase::Rejected(tr) => match self.classify_input(&tr, report, now_ms) {
                Input::Lift(_) => {
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
            // Explicit lift-off report (evt != contact) — carries the release
            // coordinates.
            Some(t) => Input::Lift(Some(t)),
            None if now_ms.wrapping_sub(tr.last_ms) > RELEASE_TIMEOUT_MS => Input::Lift(None),
            None => Input::Nothing,
        }
    }

    /// Zone + direction-dominance classification (diagonals allowed: primary
    /// axis ≥ |dx|/2). Swipe-up arms from the bottom zone, swipe-down from the
    /// top zone.
    fn classify_dir(&self, start_y: u16, dx: i32, dy_up: i32) -> Option<SwipeDir> {
        let in_bottom = start_y as u32 >= self.height as u32 * (8 - ARM_ZONE_EIGHTHS) / 8;
        let in_top = (start_y as u32) < self.height as u32 * ARM_ZONE_EIGHTHS / 8;
        if in_bottom && dy_up > 0 && dy_up * 2 >= dx.abs() {
            Some(SwipeDir::Up)
        } else if in_top && dy_up < 0 && -dy_up * 2 >= dx.abs() {
            Some(SwipeDir::Down)
        } else {
            None
        }
    }
}

enum Input {
    Move(TouchPoint),
    Lift(Option<TouchPoint>),
    Nothing,
}

/// Travel from touch-down along the swipe direction, clamped ≥ 0.
fn dist_along(tr: &Track, dir: SwipeDir) -> u16 {
    match dir {
        SwipeDir::Up => tr.start_y.saturating_sub(tr.last_y),
        SwipeDir::Down => tr.last_y.saturating_sub(tr.start_y),
    }
}
