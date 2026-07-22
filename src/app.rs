//! Application scene machine + frame loop (docs/ROADMAP.md M4, W3).
//!
//! The unlock model is one variable: the **sheet height** `b` (0..=H),
//! Locked ⇔ b = H, the App Wheel ⇔ b = 0. Both sides of the boundary are
//! AMOLED black, so the sheet itself is invisible — the unlock is told
//! entirely through the morph (docs/W3-APP-SCREENS-PLAN.md §3): the lock
//! digits scale/translate from center-large to the wheel's status slot,
//! the ring fades, and wheel rows rise in under the boundary — all
//! scrubbed 1:1 by sheet progress. A swipe-up drag maps `b = H − dist`; a
//! top-edge swipe-down from the wheel (relock) maps `b = dist` — one morph
//! composer serves both directions, and release settles `b` to the nearest
//! rest state with an exponential ease-out (interruption-safe, any height).
//!
//! Loop shape: fixed 20 fps cadence while resting; during a drag the cadence
//! is dropped for render-on-touch-move (composes capped at ~60 Hz). Flushes
//! are partial via the DMI except on band-heavy frames.

use esp_hal::gpio::{Input, Output};
use esp_hal::i2c::master::I2c;
use esp_hal::time::{Duration, Instant};
use esp_hal::Blocking;
use esp_println::println;

use crate::board::{LCD_COL_OFFSET, LCD_HEIGHT, LCD_WIDTH};
use crate::display::qspi_bus::QspiBus;
use crate::display::watch_fb::{RectAcc, WatchFb};
use crate::drivers::{axp2101, cst9217};
use crate::input::gestures::{GestureEvent, SwipeDir, SwipeTracker};
use crate::scenes::{apps, lock, wheel};
use crate::time::{WallClock, WallTime};

/// Fixed 20 fps cadence while the clock is static (idle frames cost ~0).
const FRAME_US: u64 = 50_000;
/// Bezel-animation cadence — matches TARGET_FPS (40) in build.rs so the
/// frame-indexed ease schedules take exactly their designed duration, at
/// double the temporal resolution of the old 20 fps sweep.
const CLOCK_ANIM_FRAME_US: u64 = 25_000;
/// Settle-animation cadence (40 fps — matches the bezel anim cadence; the
/// worst settle frame is a 24 ms full flush, inside the budget). Was 25 fps,
/// which read as end-of-unlock lag.
const ANIM_FRAME_US: u64 = 25_000;
/// Minimum gap between drag composes (≈80 Hz render-on-touch-move cap).
const COMPOSE_MIN_US: u64 = 12_000;
/// Release verdict ("magnetic snap"): past half the screen the transition
/// always completes; under half it always retracts — nothing rests midway…
const COMPLETE_DIST: i32 = LCD_HEIGHT as i32 / 2;
/// …except a quick flick: at least this velocity along the swipe at release
/// (Q8 px/ms ≈ 0.5) completes regardless of distance.
const COMPLETE_VEL_Q8: i32 = 128;
/// Ring fade completes over the first third of sheet travel, in 16 steps.
const RING_FADE_RANGE: i32 = LCD_HEIGHT as i32 / 3;
const RING_LEVELS: i32 = 16;
const Q: i32 = 16384;
/// Auto-relock after this long out of Locked without interaction.
const AUTO_RELOCK_SECS: u64 = 60;
/// Exponential settle: b += diff/2 per frame; snap when |diff| ≤ this.
/// (Was 3/8 with snap 6 — the ease-out tail crawled through near-black image
/// regions where the boundary is invisible, reading as end-of-unlock
/// stagger; 1/2 + snap 8 lands decisively.)
const SETTLE_SNAP_PX: i32 = 8;
/// Consecutive touch I2C errors before the controller is re-initialized.
const TOUCH_REINIT_ERRORS: u32 = 5;

/// M5 burn-in/power ladder: dim after this long without touch…
const IDLE_DIM_SECS: u64 = 30;
/// …ramping to this brightness (CO5300 reg 0x51; 0xFF = full)…
const IDLE_DIM_LEVEL: u8 = 0x38;
/// …then AOD (black + drifting HH:MM only, ring off) after this long…
const AOD_SECS: u64 = 120;
const AOD_BRIGHTNESS: u8 = 0x18;
/// …then full display sleep. Touch wakes instantly from every stage.
const SLEEP_SECS: u64 = 600;
/// Dim-down ramp step per frame (~1.2 s at 20 fps); wake restores instantly.
const DIM_STEP: u8 = 8;
/// Relaxed cadence while in AOD/Sleep (touch polling is unaffected — the
/// cadence wait loop polls the INT pin continuously either way).
const IDLE_FRAME_US: u64 = 500_000;

const H: i32 = LCD_HEIGHT as i32;

#[derive(Clone, Copy, PartialEq)]
enum Scene {
    Locked,
    /// The App Wheel — where unlock lands (W3).
    Wheel,
    /// An open app screen (the wheel row index IS the app index). Entered
    /// through the open morph, left via PWR (close morph) or relock.
    App(usize),
}

/// M5 idle ladder. Wake (on any touch) is instant from every state.
#[derive(Clone, Copy, PartialEq)]
enum Power {
    Awake,
    /// Panel dimmed; scene renders normally, minute sweep suppressed.
    Dim,
    /// Black canvas + drifting HH:MM only (Locked only; auto-relock runs first).
    Aod,
    /// Panel display off + sleep-in.
    Sleep,
}

/// Touch-poll state shared by the cadence wait and the drag session.
struct TouchPoll {
    log_ctr: u32,
    last_read: Instant,
    i2c_errors: u32,
    consec_errors: u32,
    int_was_low: bool,
    /// Last touch report of any kind — drives M5 idle dimming.
    last_activity: Instant,
    /// Latest raw report (incl. lift reports) + a sequence counter, for
    /// direct-manipulation surfaces that track the finger pre-classification.
    last_raw: Option<cst9217::TouchPoint>,
    raw_seq: u32,
}

/// Outcome of a direct-manipulation wheel session (finger owns the wheel
/// from the first raw report; verdict only at lift).
enum Direct {
    /// Real release velocity (signed, scroll space, Q8 px/ms) + hold time.
    Fling { raw: i32, held_ms: u32 },
    /// Slow/held release — the wheel stays where the finger left it.
    Rest,
    /// Never crossed the jitter gate: a tap at this Y.
    Tap { y: i32 },
}

pub struct App<'a, 'd> {
    pub bus: QspiBus<'d>,
    pub wfb: WatchFb<'a>,
    pub i2c: I2c<'d, Blocking>,
    pub tp_int: Input<'d>,
    pub tp_reset: Output<'d>,
    pub wall: WallClock,
    pub clock: lock::Clock,
    pub swipe: SwipeTracker,
    pub wheel_fx: wheel::WheelFx,
}

impl<'a, 'd> App<'a, 'd> {
    pub fn run(mut self) -> ! {
        let anim_start = Instant::now();

        // Prime: WatchFb::new cleared the canvas and marked it fully dirty.
        self.clock.render(&mut self.wfb, 0, &self.wall.now());
        self.bus.flush_bytes(self.wfb.bytes());
        self.wfb.clear_damage();
        println!("First frame: {} ms", anim_start.elapsed().as_millis());

        let mut scene = Scene::Locked;
        let mut sheet_b: u16 = LCD_HEIGHT;
        let mut unlocked_at = Instant::now();
        let mut tp = TouchPoll {
            log_ctr: 0,
            last_read: Instant::now(),
            i2c_errors: 0,
            consec_errors: 0,
            int_was_low: false,
            last_activity: Instant::now(),
            last_raw: None,
            raw_seq: 0,
        };
        let mut last_report = Instant::now();
        let mut ema_fps: f32 = 0.0;
        let mut brightness: u8 = 0xFF;
        let mut power = Power::Awake;
        let mut aod_minute: u8 = 255;
        // Minute shown by the wheel/app status line at rest (rollover redraw).
        let mut status_minute: u8 = 255;
        // Wheel scroll state (Q8 px) + battery cached at wheel entry.
        let mut wheel_s_q8: i32 = 0;
        let mut wheel_batt: Option<u8> = None;

        loop {
            let frame_start = Instant::now();
            let elapsed = anim_start.elapsed().as_millis() as u32;

            self.wall.maybe_resync(&mut self.i2c);
            let now = self.wall.now();

            // Auto-relock: the morph runs back down after a minute idle.
            if scene != Scene::Locked
                && unlocked_at.elapsed() >= Duration::from_secs(AUTO_RELOCK_SECS)
            {
                println!("scene: auto-relock");
                if matches!(scene, Scene::App(_)) {
                    // The app painted content the rect cache doesn't track —
                    // the relock morph reseeds from a clean wheel frame.
                    self.wheel_fx.invalidate();
                }
                scene = self.settle_from(&mut sheet_b, LCD_HEIGHT, &now, wheel_batt, wheel_s_q8);
                continue;
            }

            // Late-latch (research Rx1): one poll BEFORE decoration — a
            // gesture preempts the ring tick / intro frame render+flush and
            // is consumed by this frame's event handling with zero delay.
            self.swipe.set_free_scroll(scene == Scene::Wheel);
            let mut pre_ev = GestureEvent::None;
            if scene == Scene::Wheel && power == Power::Awake {
                let now_ms = anim_start.elapsed().as_millis() as u32;
                pre_ev = self.poll_touch_once(&mut tp, now_ms);
            }
            let preempted = !matches!(pre_ev, GestureEvent::None);

            // Direct-manipulation takeover: any press on the wheel outside
            // the relock zone owns it from the FIRST raw report — no
            // classification wait, no slop distance lost (the CST9217 can
            // sit silent for 200+ px of slow travel before classifying).
            if scene == Scene::Wheel
                && power == Power::Awake
                && self.swipe.finger_down()
                && self
                    .swipe
                    .press_origin_y()
                    .is_some_and(|y| (y as i32) >= H * 12 / 100)
            {
                self.wheel_interact(true, 0, &mut wheel_s_q8, &now, wheel_batt, anim_start, &mut tp);
                unlocked_at = Instant::now();
                continue;
            }

            let render_start = Instant::now();
            if scene == Scene::Locked && power != Power::Aod && power != Power::Sleep {
                self.clock.render(&mut self.wfb, elapsed, &now);
            } else if scene == Scene::Wheel && power == Power::Awake && !preempted {
                if self.wheel_fx.intro_active() {
                    // Entrance reveal frames (interruptible — any gesture
                    // above preempts; rows keep rising during interaction).
                    wheel::draw_scroll(
                        &mut self.wfb,
                        &now,
                        wheel_batt,
                        wheel_s_q8,
                        &mut self.wheel_fx,
                        false,
                        None,
                    );
                } else {
                    // Focus ring's animated gradient (partial redraw).
                    let focused = (((wheel_s_q8 >> 8) + wheel::PITCH_PX / 2) / wheel::PITCH_PX)
                        .clamp(0, wheel::rows() as i32 - 1) as usize;
                    wheel::tick_ring(&mut self.wfb, elapsed, focused);
                    if status_minute != now.minute {
                        status_minute = now.minute;
                        wheel::tick_status(&mut self.wfb, &now, wheel_batt);
                    }
                }
            } else if let Scene::App(idx) = scene {
                if power == Power::Awake {
                    // The app's ONE breathing element (partial redraw).
                    apps::tick(&mut self.wfb, idx, elapsed);
                    if status_minute != now.minute {
                        status_minute = now.minute;
                        wheel::tick_status(&mut self.wfb, &now, wheel_batt);
                    }
                }
            }
            let render_ms = render_start.elapsed().as_millis() as u32;

            let (flush_ms, flush_mode, span_count) = self.flush_dirty();
            let flushed = flush_mode != '-';

            // M5 idle ladder: Awake → Dim (30 s) → AOD (2 min, Locked only)
            // → Sleep (10 min). Any touch wakes instantly.
            let idle = tp.last_activity.elapsed();
            let desired = if idle < Duration::from_secs(IDLE_DIM_SECS) {
                Power::Awake
            } else if idle < Duration::from_secs(AOD_SECS) || scene != Scene::Locked {
                Power::Dim
            } else if idle < Duration::from_secs(SLEEP_SECS) {
                Power::Aod
            } else {
                Power::Sleep
            };
            match desired {
                Power::Awake => {
                    if power != Power::Awake || brightness != 0xFF {
                        self.wake_display(&mut power, &mut brightness, &now);
                    }
                }
                Power::Dim => {
                    if power != Power::Dim {
                        // No ornament animation to an empty room; the time
                        // itself still updates.
                        self.clock.set_minute_anim(false);
                        power = Power::Dim;
                    }
                    if brightness > IDLE_DIM_LEVEL {
                        brightness = brightness.saturating_sub(DIM_STEP).max(IDLE_DIM_LEVEL);
                        self.bus.write_c8d8(0x51, brightness);
                    }
                }
                Power::Aod => {
                    if power != Power::Aod {
                        power = Power::Aod;
                        aod_minute = 255; // force the first AOD frame
                        brightness = AOD_BRIGHTNESS;
                        self.bus.write_c8d8(0x51, brightness);
                        println!("power: AOD");
                    }
                    if aod_minute != now.minute {
                        aod_minute = now.minute;
                        self.clock.draw_aod(&mut self.wfb, &now);
                        self.flush_dirty();
                    }
                }
                Power::Sleep => {
                    if power != Power::Sleep {
                        power = Power::Sleep;
                        println!("power: display sleep");
                        self.bus.write_command(0x28); // display off
                        self.bus.write_command(0x10); // sleep in
                    }
                }
            }

            // PWR key: while locked, a short press ignites the lightsaber
            // flourish (waking the display first if idle). In the wheel it
            // will open the focused app (W3.2); log-only until then. The
            // chip latches presses, so once-per-frame polling never misses
            // one.
            match axp2101::poll_power_key(&mut self.i2c) {
                axp2101::PowerKey::ShortPress => {
                    tp.last_activity = Instant::now();
                    if power != Power::Awake || brightness != 0xFF {
                        self.wake_display(&mut power, &mut brightness, &now);
                    }
                    match scene {
                        Scene::Locked => {
                            self.clock.start_flourish();
                            println!("pwr: short press -> flourish");
                        }
                        Scene::Wheel => {
                            let focused = (((wheel_s_q8 >> 8) + wheel::PITCH_PX / 2)
                                / wheel::PITCH_PX)
                                .clamp(0, wheel::rows() as i32 - 1)
                                as usize;
                            println!("pwr: short press -> open app {focused}");
                            scene = self.app_morph(
                                focused, true, wheel_s_q8, &now, wheel_batt, anim_start, &mut tp,
                            );
                            unlocked_at = Instant::now();
                        }
                        Scene::App(idx) => {
                            println!("pwr: short press -> close app {idx}");
                            scene = self.app_morph(
                                idx, false, wheel_s_q8, &now, wheel_batt, anim_start, &mut tp,
                            );
                            unlocked_at = Instant::now();
                        }
                    }
                }
                axp2101::PowerKey::LongPress => {
                    tp.last_activity = Instant::now();
                    println!("pwr: long press");
                }
                axp2101::PowerKey::None => {}
            }

            let work_ms = frame_start.elapsed().as_millis() as u32;

            // Cadence remainder = touch poll window; a DragStart hands control
            // to the drag session (render-on-touch-move).
            let mut start_drag: Option<(SwipeDir, u16, u16)> = None;
            let mut flick: Option<(SwipeDir, u16, i32)> = None;
            let mut wheel_drag: Option<(SwipeDir, u16)> = None;
            let mut wheel_flick: Option<i32> = None;
            let mut wheel_tap: Option<u16> = None;
            let frame_us = match power {
                Power::Aod | Power::Sleep => IDLE_FRAME_US,
                _ if scene == Scene::Locked && self.clock.is_animating() => CLOCK_ANIM_FRAME_US,
                // Entrance reveal animates at full cadence.
                _ if scene == Scene::Wheel && self.wheel_fx.intro_active() => ANIM_FRAME_US,
                _ => FRAME_US,
            };
            let deadline = frame_start + Duration::from_micros(frame_us);
            loop {
                let now_ms = anim_start.elapsed().as_millis() as u32;
                // The pre-render poll's event (if any) is consumed first.
                let ev = if !matches!(pre_ev, GestureEvent::None) {
                    core::mem::replace(&mut pre_ev, GestureEvent::None)
                } else {
                    self.poll_touch_once(&mut tp, now_ms)
                };
                match ev {
                    GestureEvent::DragStart { dir, x, y, dist } => {
                        let mut kind = "ignored";
                        match (dir, scene) {
                            (SwipeDir::Up, Scene::Locked) => {
                                start_drag = Some((dir, dist, y));
                                kind = "sheet";
                            }
                            // Top-edge swipe-down = relock (wheel AND apps).
                            (SwipeDir::Down, Scene::Wheel | Scene::App(_))
                                if (y as i32) < H * 12 / 100 =>
                            {
                                start_drag = Some((dir, dist, y));
                                kind = "relock";
                            }
                            (_, Scene::Wheel) => {
                                wheel_drag = Some((dir, dist));
                                kind = "wheel";
                            }
                            _ => {}
                        }
                        println!(
                            "gesture: drag arm dir={} x={x} y={y} dist={dist} ({kind})",
                            dir_str(dir)
                        );
                    }
                    // DragEnd without a preceding DragStart: the whole swipe
                    // fit inside one poll gap (fast flick) — the recognizer
                    // classified it from touch-down + lift-report coordinates.
                    GestureEvent::DragEnd { dir, dist, vel_q8 } => {
                        if scene == Scene::Wheel {
                            let sign = match dir {
                                SwipeDir::Up => 1,
                                SwipeDir::Down => -1,
                            };
                            wheel_flick = Some(sign * vel_q8);
                            println!("gesture: wheel flick vel_q8={vel_q8}");
                        } else {
                            let wanted = matches!((dir, scene), (SwipeDir::Up, Scene::Locked));
                            println!(
                                "gesture: flick dir={} dist={dist} vel_q8={vel_q8}{}",
                                dir_str(dir),
                                if wanted { "" } else { " (ignored)" }
                            );
                            if wanted {
                                flick = Some((dir, dist, vel_q8));
                            }
                        }
                    }
                    GestureEvent::Tap { x, y } => {
                        if scene == Scene::Wheel {
                            wheel_tap = Some(y);
                        }
                        println!("gesture: tap x={x} y={y}");
                    }
                    _ => {}
                }
                if start_drag.is_some()
                    || flick.is_some()
                    || wheel_drag.is_some()
                    || wheel_flick.is_some()
                    || wheel_tap.is_some()
                    // A finger on the wheel ends the frame NOW — the next
                    // frame-top takeover hands it to the direct session
                    // within a few ms instead of waiting out the deadline.
                    || (scene == Scene::Wheel && self.swipe.finger_down())
                    || Instant::now() >= deadline
                {
                    break;
                }
                core::hint::spin_loop();
            }
            self.maybe_reinit_touch(&mut tp);
            // Any touch inside an app counts as interaction (auto-relock).
            if matches!(scene, Scene::App(_))
                && tp.last_activity.elapsed() < Duration::from_millis(100)
            {
                unlocked_at = Instant::now();
            }

            if let Some((dir, dist, start_y)) = start_drag {
                // A drag can arm while dimmed/AOD — the composer needs the
                // normal lock canvas and full brightness before it renders.
                if power != Power::Awake || brightness != 0xFF {
                    self.wake_display(&mut power, &mut brightness, &now);
                }
                self.abort_flourish();
                if dir == SwipeDir::Up {
                    // Fresh unlock always lands on the wheel's first row.
                    wheel_s_q8 = 0;
                }
                if matches!(scene, Scene::App(_)) {
                    // Relock from inside an app: the canvas holds content the
                    // rect cache doesn't track — reseed for the morph.
                    self.wheel_fx.invalidate();
                }
                scene = self.drag_session(
                    dir,
                    dist,
                    start_y,
                    &mut sheet_b,
                    &now,
                    anim_start,
                    &mut tp,
                    &mut wheel_batt,
                    wheel_s_q8,
                );
                if scene == Scene::Wheel {
                    unlocked_at = Instant::now();
                }
                continue;
            }
            if wheel_drag.is_some() {
                // Fallback entry (the pre-poll takeover normally wins the
                // race): the finger is still down — the session anchors at
                // its current position.
                self.wheel_interact(true, 0, &mut wheel_s_q8, &now, wheel_batt, anim_start, &mut tp);
                unlocked_at = Instant::now();
                continue;
            }
            if let Some(v) = wheel_flick {
                self.wheel_interact(false, v, &mut wheel_s_q8, &now, wheel_batt, anim_start, &mut tp);
                unlocked_at = Instant::now();
                continue;
            }
            if let Some(y) = wheel_tap {
                let s_px = wheel_s_q8 >> 8;
                let row = (y as i32 - H / 2 + s_px + wheel::PITCH_PX / 2)
                    .div_euclid(wheel::PITCH_PX)
                    .clamp(0, wheel::rows() as i32 - 1) as usize;
                let cur = (((wheel_s_q8 >> 8) + wheel::PITCH_PX / 2) / wheel::PITCH_PX)
                    .clamp(0, wheel::rows() as i32 - 1) as usize;
                if row != cur {
                    println!("wheel: tap -> row {row}");
                    self.wheel_settle(&mut wheel_s_q8, row, &now, wheel_batt, anim_start, &mut tp);
                }
                unlocked_at = Instant::now();
                continue;
            }
            if let Some((_dir, dist, vel)) = flick {
                if power != Power::Awake || brightness != 0xFF {
                    self.wake_display(&mut power, &mut brightness, &now);
                }
                self.abort_flourish();
                if dist as i32 > COMPLETE_DIST || vel > COMPLETE_VEL_Q8 {
                    // Whole-swipe unlock (the flick fit in one poll gap):
                    // morph-settle straight into the wheel.
                    wheel_s_q8 = 0;
                    self.begin_unlock_morph(&mut wheel_batt);
                    scene = self.settle_from(&mut sheet_b, 0, &now, wheel_batt, wheel_s_q8);
                    unlocked_at = Instant::now();
                    continue;
                }
            }

            let inst_fps = if work_ms > 0 { 1000.0 / work_ms as f32 } else { 0.0 };
            if flushed {
                ema_fps = if ema_fps < 1.0 { inst_fps } else { ema_fps * 0.9 + inst_fps * 0.1 };
            }
            if last_report.elapsed() >= Duration::from_secs(1) {
                println!(
                    "clock fps~{:.1} render={}ms flush={}ms({}) spans={} work={}ms terr={} int={} | centers={} cdelta={} px_writes={}",
                    ema_fps,
                    render_ms,
                    flush_ms,
                    flush_mode,
                    span_count,
                    work_ms,
                    tp.i2c_errors,
                    self.tp_int.is_low() as u8,
                    self.clock.last_bezel_centers,
                    self.clock.last_bezel_center_delta,
                    self.clock.last_bezel_writes
                );
                last_report = Instant::now();
            }
        }
    }

    /// Finger-tracked sheet drag driving the unlock/relock morph (W3 §3):
    /// composes on movement until lift-off, then settles. Returns the
    /// resulting scene.
    fn drag_session(
        &mut self,
        dir: SwipeDir,
        start_dist: u16,
        start_y: u16,
        sheet_b: &mut u16,
        now: &WallTime,
        anim_start: Instant,
        tp: &mut TouchPoll,
        batt: &mut Option<u8>,
        s_q8: i32,
    ) -> Scene {
        if dir == SwipeDir::Up {
            self.begin_unlock_morph(batt);
        }
        // Map the REACHABLE finger travel (touch-down point → panel edge) onto
        // the full sheet travel: 1:1 mapping stalled the sheet at ~85-90% with
        // the finger ground against the edge (a drag starting inside the arm
        // zone can never physically travel the full H) — the user felt it as a
        // stagger right at the end of slow unlocks.
        let avail = match dir {
            SwipeDir::Up => (start_y as i32).max(1),
            SwipeDir::Down => (H - start_y as i32).max(1),
        };
        let map_target = |dist: u16| -> u16 {
            let d = ((dist as i32 * H + avail / 2) / avail).min(H) as u16;
            match dir {
                SwipeDir::Up => LCD_HEIGHT - d,
                SwipeDir::Down => d,
            }
        };
        // The sheet moves on the very first classified sample — the travel
        // already covered when the drag armed, not zero.
        let mut target_b = map_target(start_dist);
        let mut last_compose = Instant::now();
        let mut composes: u32 = 0;
        let mut max_step: i32 = 0;
        let mut lvl_prev = ring_level_idx(*sheet_b);
        // Tracking is the simple pre-filter version (user-directed revert):
        // raw mapped target, plain 2/3 pursuit — no median, no hysteresis,
        // no regime switching. The sensor's flaky top edge is handled by the
        // unlock auto-commit alone.
        let mut last_move = Instant::now();
        let (mut gap_bot, mut gap_top) = (0u32, 0u32);

        let (dist, vel) = loop {
            let now_ms = anim_start.elapsed().as_millis() as u32;
            match self.poll_touch_once(tp, now_ms) {
                GestureEvent::DragMove { dist, .. } => {
                    let g = (last_move.elapsed().as_millis() as u32).max(1);
                    last_move = Instant::now();
                    if (target_b as i32) > H / 2 {
                        gap_bot = gap_bot.max(g);
                    } else {
                        gap_top = gap_top.max(g);
                    }
                    target_b = map_target(dist);
                }
                GestureEvent::DragEnd { dist, vel_q8, .. } => {
                    // Lift uses the raw final travel (lift-report coords).
                    target_b = map_target(dist);
                    break (dist as i32, vel_q8);
                }
                _ => {}
            }
            // AUTO-COMMIT (unlock only, 85% travel): the transition completes
            // itself before the finger reaches the sensor's untrustworthy top
            // edge. The finger is still down — CANCEL the press so its lift
            // can't fling the wheel; further motion is picked up cleanly by
            // the run loop's direct-manipulation takeover.
            if dir == SwipeDir::Up && (target_b as i32) < H * 3 / 20 {
                println!(
                    "gesture: auto-commit b={} (composes={composes} gap_bot={gap_bot}ms gap_top={gap_top}ms)",
                    target_b
                );
                self.swipe.cancel();
                return self.settle_from(sheet_b, 0, now, *batt, s_q8);
            }
            if target_b != *sheet_b
                && last_compose.elapsed() >= Duration::from_micros(COMPOSE_MIN_US)
            {
                last_compose = Instant::now();
                let t0 = Instant::now();
                let diff = target_b as i32 - *sheet_b as i32;
                let next = if diff.abs() <= 2 {
                    target_b
                } else {
                    (*sheet_b as i32 + diff * 2 / 3) as u16
                };
                max_step = max_step.max((next as i32 - *sheet_b as i32).abs());
                self.compose_morph(next, now, *batt, s_q8, &mut lvl_prev);
                *sheet_b = next;
                let compose_ms = t0.elapsed().as_millis() as u32;
                let (flush_ms, mode, spans) = self.flush_dirty();
                composes += 1;
                if composes % 8 == 1 {
                    println!(
                        "drag b={} compose={}ms flush={}ms({}) spans={}",
                        *sheet_b, compose_ms, flush_ms, mode, spans
                    );
                }
            }
            self.maybe_reinit_touch(tp);
            core::hint::spin_loop();
        };

        // Verdict on SHEET travel (post-mapping), so "past 50% of the screen"
        // means what the eye sees; a fast flick completes regardless.
        let complete = match dir {
            SwipeDir::Up => (target_b as i32) < H / 2,
            SwipeDir::Down => (target_b as i32) > H / 2,
        } || vel > COMPLETE_VEL_Q8;
        let (target, verdict) = match (dir, complete) {
            (SwipeDir::Up, true) => (0, "unlock"),
            (SwipeDir::Up, false) => (LCD_HEIGHT, "springback"),
            (SwipeDir::Down, true) => (LCD_HEIGHT, "relock"),
            (SwipeDir::Down, false) => (0, "stay-unlocked"),
        };
        println!(
            "gesture: release dir={} dist={dist} vel_q8={vel} -> {verdict} (composes={composes} max_step={max_step} gap_bot={gap_bot}ms gap_top={gap_top}ms)",
            dir_str(dir)
        );
        self.settle_from(sheet_b, target, now, *batt, s_q8)
    }

    /// Ease `sheet_b` to `target` (exponential decay), driving the morph,
    /// then finalize the scene. Also the entry point for auto-relock (no
    /// prior drag) and the whole-swipe flick unlock.
    fn settle_from(
        &mut self,
        sheet_b: &mut u16,
        target: u16,
        now: &WallTime,
        batt: Option<u8>,
        s_q8: i32,
    ) -> Scene {
        let settle_start = Instant::now();
        let mut settle_frames = 0u32;
        let mut lvl_prev = ring_level_idx(*sheet_b);
        while *sheet_b != target {
            settle_frames += 1;
            let frame_start = Instant::now();
            let diff = target as i32 - *sheet_b as i32;
            let next = if diff.abs() <= SETTLE_SNAP_PX {
                target as i32
            } else {
                // Exponential ease-out; the ±1 floor guarantees progress.
                let step = diff / 2;
                *sheet_b as i32 + if step == 0 { diff.signum() } else { step }
            };
            self.compose_morph(next as u16, now, batt, s_q8, &mut lvl_prev);
            *sheet_b = next as u16;
            if *sheet_b == target {
                // Final frame: flush together with the normalize below —
                // two back-to-back full flushes read as an end-of-animation
                // flicker.
                break;
            }
            self.flush_dirty();
            while Instant::now() < frame_start + Duration::from_micros(ANIM_FRAME_US) {
                core::hint::spin_loop();
            }
        }

        if target == LCD_HEIGHT {
            // Fully locked: normalize the canvas (full ring + text at rest,
            // cache-registered) and resume the clock. A minute change during
            // the drag re-animates on the next render.
            self.clock.repaint_full(&mut self.wfb, now);
            self.flush_dirty();
            println!(
                "scene: -> Locked (settle {} frames in {}ms)",
                settle_frames,
                settle_start.elapsed().as_millis()
            );
            Scene::Locked
        } else {
            // Unlocked = the wheel. Normalize with a crisp seed frame: rows
            // at rest and the real status line replacing the morphed digits
            // at the exact same slot.
            self.wheel_fx.invalidate();
            wheel::draw_scroll(&mut self.wfb, now, batt, s_q8, &mut self.wheel_fx, false, None);
            self.flush_dirty();
            println!(
                "scene: -> Wheel (settle {} frames in {}ms)",
                settle_frames,
                settle_start.elapsed().as_millis()
            );
            Scene::Wheel
        }
    }

    /// Arm the unlock morph: fresh battery for the status line, and hand
    /// the canvas to the wheel renderer WITHOUT clearing it — the black
    /// base and the lock ring stay put; the resting lock text is queued
    /// for the renderer's targeted erase.
    fn begin_unlock_morph(&mut self, batt: &mut Option<u8>) {
        *batt = axp2101::battery_percent(&mut self.i2c);
        let (x0, y0, x1, y1) = self.clock.canvas_text_bbox();
        self.wheel_fx.seed_silent();
        self.wheel_fx.push(x0, y0, x1, y1);
    }

    /// One unlock/relock morph frame at sheet height `b` (W3 §3). Both
    /// sides of the boundary are AMOLED black, so the sheet itself is
    /// invisible — the frame is: wheel rows revealing under the boundary,
    /// the lock ring fading with the scrub, and the time/date morph on top.
    fn compose_morph(
        &mut self,
        b: u16,
        now: &WallTime,
        batt: Option<u8>,
        s_q8: i32,
        lvl_prev: &mut i32,
    ) {
        let p = ((H - b as i32) * 256) / H;
        wheel::draw_scroll(&mut self.wfb, now, batt, s_q8, &mut self.wheel_fx, false, Some(p));
        // Ring: repainted whole while visible — level steps recolor it and
        // row/glow clears can nibble its rim pixels. Once faded, one last
        // level-0 (black) repaint retires it for the session.
        let lvl = ring_level_idx(b);
        if lvl > 0 || *lvl_prev > 0 {
            let mut acc = RectAcc::empty();
            let fb = self.wfb.buf_mut();
            self.clock.draw_ring_rows(fb, 0, H, lvl * Q / RING_LEVELS, &mut acc);
            if !acc.is_empty() {
                self.wfb.mark_rect(acc.x0 - 1, acc.y0, acc.x1 + 1, acc.y1);
            }
        }
        *lvl_prev = lvl;
        self.draw_morph_text(p, now, batt);
    }

    /// The unlock time-morph (M6 §11 / W3 §3): the lock digits scale and
    /// translate from center-large (TIME_GLYPHS) to the wheel's status
    /// slot, tracked 1:1 by sheet progress `p`; the date fades out in
    /// place; the status tail (battery) fades in at its resting spot over
    /// the last stretch. Endpoints are pixel-exact hand-offs: p=0 matches
    /// the resting lock text, p=256 lands where draw_status paints.
    fn draw_morph_text(&mut self, p: i32, now: &WallTime, batt: Option<u8>) {
        let cx = LCD_WIDTH as i32 / 2;
        let cy = H / 2;
        let mut s = [b'0'; 5];
        s[0] = b'0' + now.hour / 10;
        s[1] = b'0' + now.hour % 10;
        s[2] = b':';
        s[3] = b'0' + now.minute / 10;
        s[4] = b'0' + now.minute % 10;
        let t_str = core::str::from_utf8(&s).unwrap_or("00:00");
        let tw_big = wheel::text_width(t_str, &lock::TIME_GLYPHS).max(1);
        let th_big = lock::get_glyph(&lock::TIME_GLYPHS, '0')
            .map(|g| g.height as i32)
            .unwrap_or(80);
        let (tgt_left, tgt_tw, tgt_w) = wheel::status_metrics(now, batt);
        // Uniform scale that lands the digits at exactly the status width.
        let ratio_q8 = ((tgt_tw << 8) / tw_big).min(256);
        let sc = 256 + (((ratio_q8 - 256) * p) >> 8);
        let x_lock = cx - tw_big / 2;
        let x = x_lock + (((tgt_left - x_lock) * p) >> 8);
        let by = (cy + 5) + (((wheel::STATUS_BASE_Y - (cy + 5)) * p) >> 8);
        let alpha = 256 + (((wheel::STATUS_ALPHA - 256) * p) >> 8);
        // Date fades out over the first half of travel, in place.
        let da = (256 - 2 * p).max(0);
        let mut dbuf = [0u8; 24];
        let mut dlen = 0usize;
        if da > 0 {
            let d = self.clock.date_line(now);
            dlen = d.len().min(dbuf.len());
            dbuf[..dlen].copy_from_slice(&d.as_bytes()[..dlen]);
        }
        let d_str = core::str::from_utf8(&dbuf[..dlen]).unwrap_or("");
        let dw = wheel::text_width(d_str, &lock::TEXT_GLYPHS);
        // Battery tail fades in over the last quarter of travel.
        let ta = ((p - 192) * 4).clamp(0, 256);
        {
            let fb = self.wfb.buf_mut();
            wheel::draw_text_scaled(fb, t_str, x, by, alpha, &lock::TIME_GLYPHS, sc, false);
            if da > 0 {
                wheel::draw_text_at(fb, d_str, cx - dw / 2, cy + 70, da, &lock::TEXT_GLYPHS);
            }
            if ta > 0 {
                wheel::draw_status_tail(fb, now, batt, (ta * wheel::STATUS_ALPHA) >> 8);
            }
        }
        // Damage + next-frame clear rects (pushed into the wheel's rect
        // cache so draw_scroll erases them like any content).
        let tw_s = (tw_big * sc) >> 8;
        let th_s = (th_big * sc) >> 8;
        let tr = (x - 2, by - th_s - 4, x + tw_s + 2, by + 8);
        self.wheel_fx.push(tr.0, tr.1, tr.2, tr.3);
        self.wfb.mark_rect(tr.0, tr.1, tr.2, tr.3);
        if da > 0 {
            let dr = (cx - dw / 2 - 2, cy + 70 - 32, cx + dw / 2 + 2, cy + 70 + 10);
            self.wheel_fx.push(dr.0, dr.1, dr.2, dr.3);
            self.wfb.mark_rect(dr.0, dr.1, dr.2, dr.3);
        }
        if ta > 0 {
            let sr = (tgt_left + tgt_tw - 2, 26, tgt_left + tgt_w + 4, 66);
            self.wheel_fx.push(sr.0, sr.1, sr.2, sr.3);
            self.wfb.mark_rect(sr.0, sr.1, sr.2, sr.3);
        }
    }

    /// Flush the DMI-dirty regions: partial when small, full otherwise.
    /// Returns (flush_ms, mode 'P'/'F'/'-', span count).
    fn flush_dirty(&mut self) -> (u32, char, usize) {
        if self.wfb.is_clean() {
            return (0, '-', 0);
        }
        let byte_count = self.wfb.bytes().len();
        let spans = self.wfb.dmi.spans();
        let span_count = spans.len();
        let dirty_bytes: usize = spans.iter().map(|s| (s.x1 - s.x0 + 1) as usize * 2).sum();
        // 3/4 threshold: span flushes coalesce vertically into few window
        // bursts (the wheel's union bbox is ONE), so partial wins right up
        // until damage approaches the whole frame.
        let partial = !self.wfb.dmi.overflowed() && dirty_bytes < byte_count * 3 / 4;
        let flush_start = Instant::now();
        if partial {
            self.bus
                .flush_spans(self.wfb.bytes(), self.wfb.dmi.spans(), LCD_WIDTH, LCD_COL_OFFSET);
        } else {
            // Partial flushes shrink the panel window — restore it first.
            self.bus
                .set_window(LCD_COL_OFFSET, LCD_COL_OFFSET + LCD_WIDTH - 1, 0, LCD_HEIGHT - 1);
            self.bus.write_command(0x2C);
            self.bus.flush_bytes(self.wfb.bytes());
        }
        let flush_ms = flush_start.elapsed().as_millis() as u32;
        self.wfb.clear_damage();
        (flush_ms, if partial { 'P' } else { 'F' }, span_count)
    }

    /// One touch poll iteration: read gate → I2C read → feed the recognizer.
    /// Returns the recognizer's event.
    ///
    /// Idle: INT-edge gated (bus hygiene at rest). Finger down: fixed ~10 ms
    /// timer, no INT gating — composes/flushes block 5–25 ms at a time and
    /// swallow INT pulses; a missed lift-off would stall the release on the
    /// 1.5 s fallback timeout (hardware-observed as `vel_q8=0` releases).
    fn poll_touch_once(&mut self, tp: &mut TouchPoll, now_ms: u32) -> GestureEvent {
        let int_low = self.tp_int.is_low();
        let falling_edge = int_low && !tp.int_was_low;
        tp.int_was_low = int_low;
        let do_read = if self.swipe.finger_down() {
            tp.last_read.elapsed() >= Duration::from_millis(10)
        } else {
            (falling_edge && tp.last_read.elapsed() >= Duration::from_millis(2))
                || (int_low && tp.last_read.elapsed() >= Duration::from_millis(20))
        };

        let report = if do_read {
            tp.last_read = Instant::now();
            let t0 = Instant::now();
            let r = cst9217::read_touch(&mut self.i2c);
            let read_ms = t0.elapsed().as_millis() as u32;
            if read_ms > 5 {
                println!("touch: slow read {read_ms}ms");
            }
            match r {
                Ok(r) => {
                    tp.consec_errors = 0;
                    r
                }
                Err(()) => {
                    tp.i2c_errors += 1;
                    tp.consec_errors += 1;
                    None
                }
            }
        } else {
            None
        };
        if let Some(t) = report {
            tp.last_activity = Instant::now();
            // Raw-report stash for direct-manipulation surfaces (lift
            // reports included — they carry the final coordinates).
            tp.last_raw = Some(t);
            tp.raw_seq = tp.raw_seq.wrapping_add(1);
            tp.log_ctr += 1;
            if tp.log_ctr % 32 == 0 {
                println!(
                    "touch raw x={} y={} fingers={} pressed={}",
                    t.x, t.y, t.fingers, t.pressed as u8
                );
            }
        }
        self.swipe.feed(report, now_ms)
    }

    /// Wheel interaction umbrella: direct session → coast → (retouch?) → …
    /// A touch during any phase hands the wheel to the finger instantly.
    /// Momentum is ADDITIVE across quick successive flicks: a re-flick in
    /// the glide's direction stacks onto the surviving momentum; an
    /// opposite flick (or a long deliberate drag) starts fresh.
    fn wheel_interact(
        &mut self,
        mut grab: bool,
        vel_raw: i32,
        s_q8: &mut i32,
        now: &WallTime,
        batt: Option<u8>,
        anim_start: Instant,
        tp: &mut TouchPoll,
    ) {
        let mut vel = wheel_power(vel_raw);
        let mut carry: i32 = 0;
        loop {
            if core::mem::replace(&mut grab, false) {
                match self.wheel_direct(s_q8, now, batt, anim_start, tp) {
                    // Carried momentum fades linearly over the hold — a
                    // quick catch-and-reflick keeps nearly all of it, a
                    // deliberate drag kills it. A real same-direction flick
                    // chains: carry + raw release feed the power curve
                    // TOGETHER, so consecutive flicks compound.
                    Direct::Fling { raw, held_ms } => {
                        let carry_eff = carry * CHAIN_HOLD_MS.saturating_sub(held_ms) as i32
                            / CHAIN_HOLD_MS as i32;
                        vel = if raw.abs() >= FLING_FLOOR_Q8 && raw.signum() == carry_eff.signum()
                        {
                            wheel_power(carry_eff + raw)
                        } else {
                            wheel_power(raw)
                        };
                    }
                    Direct::Rest => vel = 0,
                    Direct::Tap { y } => {
                        if carry != 0 {
                            // Tap caught a moving wheel: it's already
                            // stopped under the finger — settle nearest.
                            vel = 0;
                        } else {
                            let row = (y - H / 2 + (*s_q8 >> 8) + wheel::PITCH_PX / 2)
                                .div_euclid(wheel::PITCH_PX)
                                .clamp(0, wheel::rows() as i32 - 1)
                                as usize;
                            let cur = (((*s_q8 >> 8) + wheel::PITCH_PX / 2) / wheel::PITCH_PX)
                                .clamp(0, wheel::rows() as i32 - 1)
                                as usize;
                            if row != cur {
                                println!("wheel: tap -> row {row}");
                            }
                            // Always settle: a micro-scrolled tap leaves a
                            // sub-row offset that must ease back.
                            self.wheel_settle(s_q8, row, now, batt, anim_start, tp);
                            return;
                        }
                    }
                }
            }
            match self.wheel_coast(s_q8, vel, now, batt, anim_start, tp) {
                None => break,
                Some(c) => {
                    grab = true;
                    carry = c;
                }
            }
        }
    }

    /// Direct-manipulation scroll session: the wheel is anchored to the
    /// finger from the FIRST raw report — signed, bidirectional, 1:1 in
    /// list pixels, anchored at TOUCH-DOWN rather than at gesture
    /// classification. The CST9217 stops reporting during slow movement
    /// and then bursts; anchoring at touch-down means a burst catches the
    /// wheel up to the finger's true total travel (nothing is discarded —
    /// the measured failure of classification-anchored tracking, where a
    /// slow 300 px drag arrived with dist≈200-360 already consumed).
    /// Bursts are eased in by the pursuit, never extrapolated. Tap vs drag
    /// vs flick resolve only at lift.
    fn wheel_direct(
        &mut self,
        s_q8: &mut i32,
        now: &WallTime,
        batt: Option<u8>,
        anim_start: Instant,
        tp: &mut TouchPoll,
    ) -> Direct {
        /// Jitter gate (~0.6 mm): movement below this never scrolls;
        /// crossing it re-anchors at the gate edge (AOSP slop-subtraction),
        /// so content engages from zero.
        const GATE_PX: i32 = 6;
        /// Retroactive tap radius (~1.2 mm ≈ Android's 8 dp touch slop):
        /// a lift whose max displacement-from-anchor stayed inside this is
        /// a tap even if it micro-scrolled — the bistable chip wanders.
        const TAP_RADIUS_PX: i32 = 13;
        /// A pause this long before lift zeroes the release velocity —
        /// drag-and-hold means "stay here", not "fling" (AOSP
        /// ASSUME_POINTER_STOPPED 40 ms, stretched for 10 ms polls).
        const STALE_MS: u32 = 50;
        /// Velocity pair dt floor: the chip's FIFO can dump several queued
        /// positions into one poll — dividing by ~1 ms explodes velocity
        /// (AOSP VelocityTracker min-dt rule).
        const DT_FLOOR_MS: i32 = 5;

        let t0 = Instant::now();
        let mut seq = tp.raw_seq;
        // Anchor at the finger's current position (touch-down for fresh
        // presses; the catch point when taking over a glide).
        let mut anchor_y = match tp.last_raw {
            Some(t) if t.pressed => t.y as i32,
            _ => {
                // Entered without a live pressed report (race): wait for one.
                loop {
                    let now_ms = anim_start.elapsed().as_millis() as u32;
                    let _ = self.poll_touch_once(tp, now_ms);
                    if !self.swipe.finger_down() {
                        return Direct::Rest;
                    }
                    if let Some(t) = tp.last_raw {
                        if t.pressed {
                            break t.y as i32;
                        }
                    }
                    core::hint::spin_loop();
                }
            }
        };
        let anchor_s = *s_q8;
        let origin_y = anchor_y;
        let mut max_disp: i32 = 0;
        let mut last_y = anchor_y;
        let mut gate_open = false;
        let mut target = anchor_s;
        // Last three distinct-position samples (y, ms) for release velocity.
        let start_ms = anim_start.elapsed().as_millis() as u32;
        let mut ring = [(anchor_y, start_ms); 3];

        loop {
            let now_ms = anim_start.elapsed().as_millis() as u32;
            // Feeds the recognizer (liveness, lift fallback); classified
            // events are superseded by this session and dropped.
            let _ = self.poll_touch_once(tp, now_ms);
            if seq != tp.raw_seq {
                seq = tp.raw_seq;
                if let Some(t) = tp.last_raw {
                    let y = t.y as i32;
                    if y != last_y {
                        ring[0] = ring[1];
                        ring[1] = ring[2];
                        ring[2] = (y, now_ms);
                        last_y = y;
                    }
                    max_disp = max_disp.max((origin_y - y).abs());
                    if !gate_open {
                        let d = anchor_y - y;
                        if d.abs() > GATE_PX {
                            gate_open = true;
                            anchor_y -= GATE_PX * d.signum();
                        }
                    }
                    if gate_open && t.pressed {
                        // Scroll space: finger up (y shrinking) = forward.
                        target = anchor_s + ((anchor_y - y) << 8);
                    }
                }
            }
            if !self.swipe.finger_down() {
                break;
            }
            let t_eff = wheel_rubber(target, wheel_s_max());
            let diff = t_eff - *s_q8;
            if diff != 0 {
                // 3/4 pursuit: 1:1 feel on live tracking, and a report
                // burst after a chip stall eases in over ~3 frames instead
                // of teleporting.
                *s_q8 += if diff.abs() <= 512 { diff } else { diff * 3 / 4 };
                self.draw_wheel(now, batt, *s_q8, diff.abs() > FAST_LOD_Q8);
                self.flush_dirty();
            }
            self.maybe_reinit_touch(tp);
            core::hint::spin_loop();
        }

        let held_ms = t0.elapsed().as_millis() as u32;
        // Retroactive tap: max displacement stayed inside the tap radius
        // and the press was short — even if it micro-scrolled a few px
        // (the settle pulls the sub-row offset back).
        if max_disp <= TAP_RADIUS_PX && held_ms < 350 {
            return Direct::Tap { y: last_y };
        }
        if !gate_open {
            return Direct::Rest;
        }
        // Release velocity from the last two real movement segments
        // (recency-weighted, hotter-of like the recognizer); a pause
        // before lift means the drag ends where it stands.
        let now_ms = anim_start.elapsed().as_millis() as u32;
        let mut raw = 0;
        if now_ms.wrapping_sub(ring[2].1) <= STALE_MS {
            let dt_b = ring[2].1.wrapping_sub(ring[1].1).max(DT_FLOOR_MS as u32) as i32;
            let dt_a = ring[1].1.wrapping_sub(ring[0].1).max(DT_FLOOR_MS as u32) as i32;
            let v_b = ((ring[1].0 - ring[2].0) << 8) / dt_b;
            let v_a = ((ring[0].0 - ring[1].0) << 8) / dt_a;
            let v_w = (2 * v_b + v_a) / 3;
            raw = if dt_b > 60 {
                v_b
            } else if (v_b >= 0) == (v_w >= 0) && v_b.abs() > v_w.abs() {
                v_b
            } else {
                v_w
            };
            // End-of-flick acceleration carry (AOSP impulse spirit — the
            // energy the finger was still ADDING at lift counts): a flick
            // speeding up through its final segments reads as its last
            // velocity plus half the measured gain. Velocity-domain only —
            // never position prediction.
            if (v_b >= 0) == (v_a >= 0) && v_b.abs() > v_a.abs() {
                let acc = v_b + (v_b - v_a) / 2;
                if (acc >= 0) == (raw >= 0) && acc.abs() > raw.abs() {
                    raw = acc;
                }
            }
        }
        // Whole-press fallback for quick flicks: the chip may emit only
        // one or two motion reports for a short ballistic swipe, gutting
        // the segment estimate (huge dt or a stale repeated-coord lift
        // report). A short press is one motion by construction. Its mean
        // velocity understates the RELEASE velocity of an accelerating
        // swipe by ~2x (constant-acceleration kinematics: v_end = 2·v_avg
        // from rest) — corrected by the conservative middle, 3/2.
        if held_ms <= 180 {
            let vp = ((origin_y - last_y) << 8) / held_ms.max(DT_FLOOR_MS as u32) as i32;
            let vp = vp * 3 / 2;
            if raw == 0 || ((vp >= 0) == (raw >= 0) && vp.abs() > raw.abs()) {
                raw = vp;
            }
        }
        if raw == 0 {
            return Direct::Rest;
        }
        Direct::Fling { raw, held_ms }
    }

    /// Velocity-projected glide (WWDC-803 / SnapHelper model, per
    /// docs/research/WHEEL-FEEL-RESEARCH.md): project the natural stopping
    /// point from the release velocity, clamp it to the list, round to a
    /// row, and decelerate STRAIGHT to that row as one continuous motion —
    /// no free coast, no boundary bounce (an overshooting fling dampens onto
    /// the first/last row, never reverses), no disjoint snap. Fully
    /// interruptible: ANY touch returns Some(carry) — the live momentum —
    /// and the direct session takes the wheel (iOS catch + AOSP flywheel);
    /// a whole-flick that fits in one poll gap (salvaged DragEnd) injects
    /// ADDITIVELY right here — same direction stacks, opposite reverses.
    fn wheel_coast(
        &mut self,
        s_q8: &mut i32,
        v0_q8: i32,
        now: &WallTime,
        batt: Option<u8>,
        anim_start: Instant,
        tp: &mut TouchPoll,
    ) -> Option<i32> {
        // iOS-FAST deceleration (the picker rate): f = 199/256 per 25 ms
        // (≈0.99/ms). Projection horizon K = dt/(1−f) = 25·256/57 ≈ 112 ms —
        // a medium flick lands 2–4 rows away, considered, picker-like.
        const DECAY_NUM: i32 = 199;
        const K_MS: i32 = 112;

        let s_max = wheel_s_max();
        let pitch_q8 = wheel::PITCH_PX << 8;
        let nearest = |s: i32| -> i32 {
            ((s + pitch_q8 / 2) / pitch_q8).clamp(0, wheel::rows() as i32 - 1) * pitch_q8
        };
        let project = |s: i32, v: i32| -> i32 {
            if v.abs() < FLING_FLOOR_Q8 {
                nearest(s)
            } else {
                nearest((s as i64 + v as i64 * K_MS as i64).clamp(0, s_max as i64) as i32)
            }
        };
        // Picker detent floor: a definite flick ALWAYS advances at least
        // one row — small flicks never die on the current row.
        let aim = |s: i32, v: i32| -> i32 {
            let t = project(s, v);
            if v.abs() >= FLING_FLOOR_Q8 && t == nearest(s) {
                (nearest(s) + v.signum() * pitch_q8).clamp(0, s_max)
            } else {
                t
            }
        };
        let mut target = aim(*s_q8, v0_q8);
        if v0_q8 != 0 {
            println!("wheel: glide v={v0_q8} -> row {}", target / pitch_q8);
        }
        // Velocity that lands exactly on the target under the decay — for an
        // unclamped projection this ≈ v0 (continuity); for a clamped one it
        // becomes a smooth damped approach to the edge row.
        let mut v_q8 = (target - *s_q8) / K_MS;
        let mut dt_ms: i32 = 25;
        // Landed linger: after the wheel rests, keep polling here for a
        // beat before handing back to the run loop — the next gesture
        // starts with ZERO scene-machine overhead between actions.
        let mut landed_at: Option<Instant> = None;

        loop {
            let fs = Instant::now();
            let now_ms = anim_start.elapsed().as_millis() as u32;
            match self.poll_touch_once(tp, now_ms) {
                // Whole flick inside one poll gap: chain it without ever
                // leaving this loop. Raw flick + live velocity feed the
                // power curve TOGETHER, so consecutive flicks compound
                // superlinearly. Opposite direction starts fresh (brake/
                // reverse); a sub-floor release stops the wheel.
                GestureEvent::DragEnd { dir, vel_q8: rel, .. } => {
                    let sign: i32 = match dir {
                        SwipeDir::Up => 1,
                        SwipeDir::Down => -1,
                    };
                    let raw = sign * rel;
                    v_q8 = if rel >= FLING_FLOOR_Q8 && raw.signum() == v_q8.signum() {
                        wheel_power(v_q8 + raw)
                    } else {
                        wheel_power(raw)
                    };
                    landed_at = None;
                    target = aim(*s_q8, v_q8);
                    println!("wheel: chain v={v_q8} -> row {}", target / pitch_q8);
                    v_q8 = (target - *s_q8) / K_MS;
                }
                GestureEvent::Tap { y, .. } => {
                    if landed_at.is_some() {
                        // Tap while resting (linger window) = row select,
                        // same hit-test as the run loop's handler.
                        let row = (y as i32 - H / 2 + (*s_q8 >> 8) + wheel::PITCH_PX / 2)
                            .div_euclid(wheel::PITCH_PX)
                            .clamp(0, wheel::rows() as i32 - 1)
                            as usize;
                        let cur = (((*s_q8 >> 8) + wheel::PITCH_PX / 2) / wheel::PITCH_PX)
                            .clamp(0, wheel::rows() as i32 - 1)
                            as usize;
                        if row != cur {
                            println!("wheel: tap -> row {row}");
                            self.wheel_settle(s_q8, row, now, batt, anim_start, tp);
                        }
                        return None;
                    }
                    // A resolved tap on a MOVING wheel stops it dead.
                    v_q8 = 0;
                    target = nearest(*s_q8);
                    println!("wheel: tap-stop");
                }
                _ => {}
            }
            if self.swipe.finger_down() {
                // Touch = the finger owns the wheel: hand off to the direct
                // session instantly, passing the live momentum as carry.
                return Some(v_q8);
            }
            if let Some(t0) = landed_at {
                // At rest: pure poll loop. Hand back after the linger.
                if t0.elapsed() >= Duration::from_millis(WHEEL_LINGER_MS) {
                    return None;
                }
                core::hint::spin_loop();
                continue;
            }
            let diff = target - *s_q8;
            // Overscroll spring-back: released out of range, the wheel
            // returns with a fast damped spring (~110 ms) — the glide's
            // velocity math would crawl back instead.
            if (*s_q8 < 0 || *s_q8 > s_max) && diff != 0 {
                *s_q8 += if diff.abs() <= 512 { diff } else { diff / 2 };
            } else
            // Glide until ~16 px out, then hand to the damped tail — at that
            // range the tail is invisible continuation, not a second flick.
            if diff.signum() == v_q8.signum() && diff.abs() > (16 << 8) {
                *s_q8 += v_q8 * dt_ms;
                v_q8 = v_q8 * (256 - (256 - DECAY_NUM) * dt_ms / 25) / 256;
            } else if diff.abs() <= 256 {
                *s_q8 = target;
                v_q8 = 0;
                self.draw_wheel(now, batt, *s_q8, false);
                self.flush_dirty();
                landed_at = Some(Instant::now());
                continue;
            } else {
                // Soft damped landing (τ≈90 ms) — blends out of the glide
                // with no velocity seam; also the caught/slow path.
                *s_q8 += diff * 5 / 16;
            }
            let fast = v_q8.abs() > FAST_LOD_Q8 / 25;
            self.draw_wheel(now, batt, *s_q8, fast);
            self.flush_dirty();
            // Fixed 25 ms cadence: consistent frame intervals read premium.
            wheel::pace(fs);
            dt_ms = (fs.elapsed().as_millis() as i32).clamp(10, 50);
        }
    }

    /// Ease the wheel to rest exactly on `row` — soft damped landing
    /// (5/16 per 25 ms frame, τ≈90 ms), matching the glide's tail.
    /// Interruptible: a finger on the glass aborts the ease mid-flight —
    /// the takeover owns the wheel from its next report, and whatever
    /// interaction follows re-settles on release.
    fn wheel_settle(
        &mut self,
        s_q8: &mut i32,
        row: usize,
        now: &WallTime,
        batt: Option<u8>,
        anim_start: Instant,
        tp: &mut TouchPoll,
    ) {
        let target = (row as i32 * wheel::PITCH_PX) << 8;
        while *s_q8 != target {
            let fs = Instant::now();
            let now_ms = anim_start.elapsed().as_millis() as u32;
            let _ = self.poll_touch_once(tp, now_ms);
            if self.swipe.finger_down() {
                return;
            }
            let diff = target - *s_q8;
            *s_q8 += if diff.abs() <= 256 { diff } else { diff * 5 / 16 };
            self.draw_wheel(now, batt, *s_q8, false);
            self.flush_dirty();
            wheel::pace(fs);
        }
    }

    /// One wheel frame through the damage-minimized renderer (targeted
    /// clear + union-bbox partial flush; motion LOD when `fast`).
    fn draw_wheel(&mut self, now: &WallTime, batt: Option<u8>, s_q8: i32, fast: bool) {
        wheel::draw_scroll(&mut self.wfb, now, batt, s_q8, &mut self.wheel_fx, fast, None);
    }

    /// The wheel ↔ app open/close morph with a splash beat (W3 §2, user-
    /// refined): the focused icon flies from its row slot to screen center
    /// growing 56→128 px while the other rows fade away — then the LOGO
    /// HOLDS, big and centered with the app title beneath it (the loading
    /// beat) — then a content app crossfades splash → content. Template
    /// apps simply REST on the splash. Close runs the same timeline
    /// backward, faster (content fades under the persistent status bar,
    /// the logo shows, the icon flies home). Smoothstep-eased phases,
    /// 25 ms cadence. PWR mid-morph REVERSES from the current progress —
    /// never queues, never snaps.
    fn app_morph(
        &mut self,
        idx: usize,
        opening: bool,
        s_q8: i32,
        now: &WallTime,
        batt: Option<u8>,
        anim_start: Instant,
        tp: &mut TouchPoll,
    ) -> Scene {
        // Timeline (ms, open direction): flight → splash hold → content.
        const T_FLIGHT: i32 = 260;
        const T_HOLD: i32 = 480;
        const T_CONTENT: i32 = 280;
        const STEP_OPEN: i32 = 25;
        const STEP_CLOSE: i32 = 34; // returns are always faster
        let t_end = T_FLIGHT + T_HOLD + if apps::has_content(idx) { T_CONTENT } else { 0 };
        let mut t: i32 = if opening { 0 } else { t_end };
        let mut dirn: i32 = if opening { 1 } else { -1 };
        if !opening {
            // Coming from the app's rest frame: the canvas holds content
            // the rect cache doesn't track — reseed on the first frame.
            self.wheel_fx.invalidate();
        }
        let smooth = |x: i32| {
            let x = x.clamp(0, 256);
            (x * x * (768 - 2 * x)) >> 16
        };
        loop {
            let fs = Instant::now();
            t += dirn * if dirn > 0 { STEP_OPEN } else { STEP_CLOSE };
            let tc = t.clamp(0, t_end);
            // (flight, splash-icon alpha, splash-title alpha, content q).
            let (f, icon_a, title_a, q) = if tc < T_FLIGHT {
                (smooth(tc * 256 / T_FLIGHT), 256, 0, 0)
            } else if tc < T_FLIGHT + T_HOLD {
                // Title fades in over the hold's first 120 ms.
                (256, 256, ((tc - T_FLIGHT) * 256 / 120).min(256), 0)
            } else {
                let q = smooth((tc - T_FLIGHT - T_HOLD) * 256 / T_CONTENT);
                (256, 256 - q, 256 - q, q)
            };
            wheel::draw_open_morph(
                &mut self.wfb,
                now,
                batt,
                s_q8,
                &mut self.wheel_fx,
                idx,
                f,
                icon_a,
            );
            if title_a > 0 {
                apps::draw_splash_title(&mut self.wfb, &mut self.wheel_fx, idx, title_a);
            }
            let elapsed = anim_start.elapsed().as_millis() as u32;
            if q > 0 {
                apps::draw_reveal(&mut self.wfb, &mut self.wheel_fx, now, idx, q, elapsed);
            }
            self.flush_dirty();
            // PWR mid-morph: reverse from the current progress.
            if axp2101::poll_power_key(&mut self.i2c) == axp2101::PowerKey::ShortPress {
                dirn = -dirn;
                println!("pwr: morph reverse");
            }
            // Keep the recognizer fed; gestures during the morph are
            // superseded by it.
            let _ = self.poll_touch_once(tp, elapsed);
            if t >= t_end && dirn > 0 {
                return Scene::App(idx);
            }
            if t <= 0 && dirn < 0 {
                return Scene::Wheel;
            }
            wheel::pace(fs);
        }
    }

    /// A gesture takes over mid-flourish: write the flourish's zero-glow
    /// closing frame (exact resting ring) so the drag composer starts from a
    /// clean canvas. The damage flushes with the gesture's first frame.
    fn abort_flourish(&mut self) {
        if self.clock.flourish_active() {
            let mut acc = RectAcc::empty();
            self.clock.cancel_flourish(self.wfb.buf_mut(), &mut acc);
            if !acc.is_empty() {
                self.wfb.mark_rect(acc.x0, acc.y0, acc.x1, acc.y1);
            }
        }
    }

    /// Instant wake from any idle state: sleep-out / AOD repaint as needed,
    /// full brightness, minute animation re-enabled.
    fn wake_display(&mut self, power: &mut Power, brightness: &mut u8, now: &WallTime) {
        match *power {
            Power::Sleep => {
                println!("power: wake from sleep");
                self.bus.write_command(0x11); // sleep out
                let t0 = Instant::now();
                // Panel needs ~120 ms after sleep-out before display-on.
                while t0.elapsed() < Duration::from_millis(120) {
                    core::hint::spin_loop();
                }
                self.clock.repaint_full(&mut self.wfb, now);
                self.flush_dirty();
                self.bus.write_command(0x29); // display on
            }
            Power::Aod => {
                println!("power: wake from AOD");
                self.clock.repaint_full(&mut self.wfb, now);
                self.flush_dirty();
            }
            _ => {}
        }
        self.clock.set_minute_anim(true);
        if *brightness != 0xFF {
            *brightness = 0xFF;
            self.bus.write_c8d8(0x51, 0xFF);
        }
        *power = Power::Awake;
    }

    /// Bus-health recovery: repeated touch read failures → re-init the chip.
    fn maybe_reinit_touch(&mut self, tp: &mut TouchPoll) {
        if tp.consec_errors >= TOUCH_REINIT_ERRORS {
            tp.consec_errors = 0;
            let ok = cst9217::init(&mut self.i2c, &mut self.tp_reset).is_ok();
            println!("touch: reinit after errors ok={}", ok as u8);
        }
    }
}

/// Wheel scroll range (Q8): rows 0..N-1, row N-1 rests at (N-1)·PITCH.
fn wheel_s_max() -> i32 {
    ((wheel::rows() as i32 - 1) * wheel::PITCH_PX) << 8
}

/// Flick power reference (Q8 px/ms): at |v|=V_REF the curve doubles the
/// velocity; well below it the response is near-linear. Raised so railing
/// takes a genuinely violent flick (picker doctrine — NumberPicker caps
/// wheel flings at 1/8 of a list's, docs/research/TOUCH-PIPELINE-RESEARCH).
const V_REF_Q8: i32 = 1200;
/// Momentum ceiling = exactly full-list travel under the K=112 ms
/// projection (9 rows · 68 px): the hardest flick or chain rails end to
/// end and nothing ever moves faster than that — every fling accountable.
const V_MAX_Q8: i32 = 1400;
/// Fling floor (Q8 px/ms): slower releases settle to the nearest row and
/// never chain — catching a railing wheel and lifting gently must not
/// re-launch it.
const FLING_FLOOR_Q8: i32 = 77;
/// Carried momentum fades linearly to zero over this hold duration — a
/// quick catch-and-reflick keeps nearly all of it, a deliberate drag none.
const CHAIN_HOLD_MS: u32 = 400;
/// Motion LOD threshold: per-frame travel (Q8) above which the glow halo
/// is dropped and scaling goes nearest-neighbor (~50 px/frame — the eye
/// can't track either at that speed). Restored in the slow landing frames.
const FAST_LOD_Q8: i32 = 50 << 8;
/// After a glide lands, keep polling in-loop this long before returning
/// to the run loop — consecutive actions start with zero scene overhead.
const WHEEL_LINGER_MS: u64 = 150;

/// Continuous flick power curve: v_eff = v·(1 + |v|/V_REF). Reach grows
/// superlinearly with flick speed — gentle ≈1–2 rows, a normal flick ≈3–5,
/// a hard flick rails to the first/last row (projection clamps to the list).
fn wheel_power(v: i32) -> i32 {
    (v + v.saturating_mul(v.abs()) / V_REF_Q8).clamp(-V_MAX_Q8, V_MAX_Q8)
}

/// Progressive rubber band past the wheel's ends (Apple's curve, integer
/// form): give = x·c·d / (d + c·x) with c=0.55 (141/256), d=96 px — stretchy
/// at first pull, stiffening the harder you drag.
fn wheel_rubber(t: i32, max: i32) -> i32 {
    // d = 128 px (~1.9 rows max stretch): easier overscroll (user), entry
    // slope stays Apple's c=0.55 — d bounds travel, c sets softness.
    const D_Q8: i64 = 128 << 8;
    let give = |x: i32| -> i32 {
        let x = x as i64;
        ((x * 141 * D_Q8) / (256 * D_Q8 + 141 * x)) as i32
    };
    if t < 0 {
        -give(-t)
    } else if t > max {
        max + give(t - max)
    } else {
        t
    }
}

/// Quantized ring brightness step (0..=16) for a sheet height: full at rest,
/// fading out over the first `RING_FADE_RANGE` of downward travel.
fn ring_level_idx(b: u16) -> i32 {
    let x = b as i32 - (H - RING_FADE_RANGE);
    let f = x.clamp(0, RING_FADE_RANGE);
    (f * RING_LEVELS + RING_FADE_RANGE / 2) / RING_FADE_RANGE
}

fn dir_str(dir: SwipeDir) -> &'static str {
    match dir {
        SwipeDir::Up => "up",
        SwipeDir::Down => "down",
    }
}
