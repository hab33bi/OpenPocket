//! Application scene machine + frame loop (docs/ROADMAP.md M4).
//!
//! The unlock model is one variable: the **sheet height** `b` (0..=H).
//! Rows `[0..b)` show the lock sheet (black + ring at scrub-tracked brightness
//! + time/date sliding with the sheet); rows `[b..H)` show the unlock image.
//! Locked ⇔ b = H, Unlocked ⇔ b = 0. A swipe-up drag maps `b = H − dist`;
//! a swipe-down (relock) drag maps `b = dist` — one incremental composer
//! serves both directions, and release settles `b` to the nearest rest state
//! with an exponential ease-out (interruption-safe, works from any height).
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
use crate::drivers::cst9217;
use crate::input::gestures::{GestureEvent, SwipeDir, SwipeTracker};
use crate::scenes::{lock, unlocked};
use crate::time::{WallClock, WallTime};

/// Fixed 20 fps cadence while the clock is static (idle frames cost ~0).
const FRAME_US: u64 = 50_000;
/// Bezel-animation cadence — matches TARGET_FPS (40) in build.rs so the
/// frame-indexed ease schedules take exactly their designed duration, at
/// double the temporal resolution of the old 20 fps sweep.
const CLOCK_ANIM_FRAME_US: u64 = 25_000;
/// Settle-animation cadence (25 fps; frames are cheap during the transition).
const ANIM_FRAME_US: u64 = 40_000;
/// Minimum gap between drag composes (≈60 Hz render-on-touch-move cap).
const COMPOSE_MIN_US: u64 = 16_000;
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
/// Auto-relock after this long in Unlocked.
const AUTO_RELOCK_SECS: u64 = 60;
/// Exponential settle: b += diff·3/8 per frame; snap when |diff| ≤ this.
const SETTLE_SNAP_PX: i32 = 6;
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
const ROW_BYTES: usize = LCD_WIDTH as usize * 2;

#[derive(Clone, Copy, PartialEq)]
enum Scene {
    Locked,
    Unlocked,
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
        };
        let mut last_report = Instant::now();
        let mut ema_fps: f32 = 0.0;
        let mut brightness: u8 = 0xFF;
        let mut power = Power::Awake;
        let mut aod_minute: u8 = 255;

        loop {
            let frame_start = Instant::now();
            let elapsed = anim_start.elapsed().as_millis() as u32;

            self.wall.maybe_resync(&mut self.i2c);
            let now = self.wall.now();

            // Auto-relock: sheet slides back down after a minute on the image.
            if scene == Scene::Unlocked
                && unlocked_at.elapsed() >= Duration::from_secs(AUTO_RELOCK_SECS)
            {
                println!("scene: auto-relock");
                scene = self.settle(&mut sheet_b, LCD_HEIGHT, &now);
                continue;
            }

            let render_start = Instant::now();
            if scene == Scene::Locked && power != Power::Aod && power != Power::Sleep {
                self.clock.render(&mut self.wfb, elapsed, &now);
            }
            let render_ms = render_start.elapsed().as_millis() as u32;

            let (flush_ms, flush_mode, span_count) = self.flush_dirty();
            let flushed = flush_mode != '-';

            // M5 idle ladder: Awake → Dim (30 s) → AOD (2 min, Locked only)
            // → Sleep (10 min). Any touch wakes instantly.
            let idle = tp.last_activity.elapsed();
            let desired = if idle < Duration::from_secs(IDLE_DIM_SECS) {
                Power::Awake
            } else if idle < Duration::from_secs(AOD_SECS) || scene == Scene::Unlocked {
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

            let work_ms = frame_start.elapsed().as_millis() as u32;

            // Cadence remainder = touch poll window; a DragStart hands control
            // to the drag session (render-on-touch-move).
            let mut start_drag: Option<(SwipeDir, u16, u16)> = None;
            let mut flick: Option<(SwipeDir, u16, i32)> = None;
            let frame_us = match power {
                Power::Aod | Power::Sleep => IDLE_FRAME_US,
                _ if scene == Scene::Locked && self.clock.is_animating() => CLOCK_ANIM_FRAME_US,
                _ => FRAME_US,
            };
            let deadline = frame_start + Duration::from_micros(frame_us);
            loop {
                let now_ms = anim_start.elapsed().as_millis() as u32;
                let ev = self.poll_touch_once(&mut tp, now_ms);
                match ev {
                    GestureEvent::DragStart { dir, x, y, dist } => {
                        let wanted = matches!(
                            (dir, scene),
                            (SwipeDir::Up, Scene::Locked) | (SwipeDir::Down, Scene::Unlocked)
                        );
                        println!(
                            "gesture: drag arm dir={} x={x} y={y} dist={dist}{}",
                            dir_str(dir),
                            if wanted { "" } else { " (ignored)" }
                        );
                        if wanted {
                            start_drag = Some((dir, dist, y));
                        }
                    }
                    // DragEnd without a preceding DragStart: the whole swipe
                    // fit inside one poll gap (fast flick) — the recognizer
                    // classified it from touch-down + lift-report coordinates.
                    GestureEvent::DragEnd { dir, dist, vel_q8 } => {
                        let wanted = matches!(
                            (dir, scene),
                            (SwipeDir::Up, Scene::Locked) | (SwipeDir::Down, Scene::Unlocked)
                        );
                        println!(
                            "gesture: flick dir={} dist={dist} vel_q8={vel_q8}{}",
                            dir_str(dir),
                            if wanted { "" } else { " (ignored)" }
                        );
                        if wanted {
                            flick = Some((dir, dist, vel_q8));
                        }
                    }
                    GestureEvent::Tap { x, y } => println!("gesture: tap x={x} y={y}"),
                    _ => {}
                }
                if start_drag.is_some() || flick.is_some() || Instant::now() >= deadline {
                    break;
                }
                core::hint::spin_loop();
            }
            self.maybe_reinit_touch(&mut tp);

            if let Some((dir, dist, start_y)) = start_drag {
                // A drag can arm while dimmed/AOD — the composer needs the
                // normal lock canvas and full brightness before it renders.
                if power != Power::Awake || brightness != 0xFF {
                    self.wake_display(&mut power, &mut brightness, &now);
                }
                scene =
                    self.drag_session(dir, dist, start_y, &mut sheet_b, &now, anim_start, &mut tp);
                if scene == Scene::Unlocked {
                    unlocked_at = Instant::now();
                }
                continue;
            }
            if let Some((dir, dist, vel)) = flick {
                if power != Power::Awake || brightness != 0xFF {
                    self.wake_display(&mut power, &mut brightness, &now);
                }
                if dist as i32 > COMPLETE_DIST || vel > COMPLETE_VEL_Q8 {
                    let (target, bbox) = match dir {
                        SwipeDir::Up => (0, self.clock.canvas_text_bbox()),
                        SwipeDir::Down => (LCD_HEIGHT, (0, 0, -1, -1)),
                    };
                    scene = self.settle_from(&mut sheet_b, target, &now, bbox);
                    if scene == Scene::Unlocked {
                        unlocked_at = Instant::now();
                    }
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

    /// Finger-tracked drag: composes on movement until lift-off, then settles.
    /// Returns the resulting scene.
    fn drag_session(
        &mut self,
        dir: SwipeDir,
        start_dist: u16,
        start_y: u16,
        sheet_b: &mut u16,
        now: &WallTime,
        anim_start: Instant,
        tp: &mut TouchPoll,
    ) -> Scene {
        // Take over the sheet text from whatever the canvas currently shows.
        let mut text_bbox = match dir {
            SwipeDir::Up => self.clock.canvas_text_bbox(),
            SwipeDir::Down => (0, 0, -1, -1),
        };
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

        let (dist, vel) = loop {
            let now_ms = anim_start.elapsed().as_millis() as u32;
            match self.poll_touch_once(tp, now_ms) {
                GestureEvent::DragMove { dist, .. } => {
                    target_b = map_target(dist);
                }
                GestureEvent::DragEnd { dist, vel_q8, .. } => {
                    target_b = map_target(dist);
                    break (dist as i32, vel_q8);
                }
                _ => {}
            }
            if target_b != *sheet_b && last_compose.elapsed() >= Duration::from_micros(COMPOSE_MIN_US)
            {
                last_compose = Instant::now();
                let t0 = Instant::now();
                // Critically-damped tracking: half the remaining distance per
                // compose (~25 ms time constant at the 60 Hz compose cap) —
                // absorbs the touch sensor's edge-of-panel jitter without
                // perceptible lag.
                let diff = target_b as i32 - *sheet_b as i32;
                let next = if diff.abs() <= 2 {
                    target_b
                } else {
                    (*sheet_b as i32 + diff / 2) as u16
                };
                self.compose_sheet(*sheet_b, next, now, &mut text_bbox);
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
            "gesture: release dir={} dist={dist} vel_q8={vel} -> {verdict}",
            dir_str(dir)
        );
        // Carry the drag text bbox into the settle (same composer).
        self.settle_from(sheet_b, target, now, text_bbox)
    }

    /// Ease `sheet_b` to `target` (exponential decay) and finalize the scene.
    /// Entry point for auto-relock (no prior drag).
    fn settle(&mut self, sheet_b: &mut u16, target: u16, now: &WallTime) -> Scene {
        self.settle_from(sheet_b, target, now, (0, 0, -1, -1))
    }

    fn settle_from(
        &mut self,
        sheet_b: &mut u16,
        target: u16,
        now: &WallTime,
        mut text_bbox: (i32, i32, i32, i32),
    ) -> Scene {
        let settle_start = Instant::now();
        let mut settle_frames = 0u32;
        while *sheet_b != target {
            settle_frames += 1;
            let frame_start = Instant::now();
            let diff = target as i32 - *sheet_b as i32;
            let next = if diff.abs() <= SETTLE_SNAP_PX {
                target as i32
            } else {
                // Exponential ease-out; the ±1 floor guarantees progress.
                let step = diff * 3 / 8;
                *sheet_b as i32 + if step == 0 { diff.signum() } else { step }
            };
            self.compose_sheet(*sheet_b, next as u16, now, &mut text_bbox);
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
            // Fully unlocked: normalize to the pristine image.
            unlocked::draw(&mut self.wfb);
            self.flush_dirty();
            println!(
                "scene: -> Unlocked (settle {} frames in {}ms)",
                settle_frames,
                settle_start.elapsed().as_millis()
            );
            Scene::Unlocked
        }
    }

    /// Incremental sheet composer: update the canvas from sheet height
    /// `b_prev` to `b`. Sheet rows `[0..b)`: black + ring (scrub-faded) +
    /// text at shift `b − H`. Image rows `[b..H)`.
    fn compose_sheet(
        &mut self,
        b_prev: u16,
        b: u16,
        now: &WallTime,
        text_bbox: &mut (i32, i32, i32, i32),
    ) {
        let bp = b_prev as i32;
        let bi = b as i32;
        let mut rects: [(i32, i32, i32, i32); 4] = [(0, 0, -1, -1); 4];
        let mut nrects = 0usize;

        let lvl = ring_level_idx(b);
        let lvl_prev = ring_level_idx(b_prev);
        let old_text = *text_bbox;

        {
            let fb = self.wfb.buf_mut();

            // 1) Band between old and new boundary.
            if bi < bp {
                // Sheet shrinks: reveal image rows [bi..bp).
                let (s, e) = (bi as usize * ROW_BYTES, bp as usize * ROW_BYTES);
                let n = e.min(fb.len()).min(unlocked::SPIKE_RGB565.len());
                if s < n {
                    fb[s..n].copy_from_slice(&unlocked::SPIKE_RGB565[s..n]);
                }
                rects[nrects] = (0, bi, LCD_WIDTH as i32 - 1, bp - 1);
                nrects += 1;
            } else if bi > bp {
                // Sheet grows: black-fill rows [bp..bi) (ring/text drawn below).
                let (s, e) = (bp as usize * ROW_BYTES, bi as usize * ROW_BYTES);
                let e = e.min(fb.len());
                if s < e {
                    fb[s..e].fill(0);
                }
                rects[nrects] = (0, bp, LCD_WIDTH as i32 - 1, bi - 1);
                nrects += 1;
            }

            // 2) Erase the text at its old spot (sheet rows only — image rows
            // were just overwritten by the band blit). Must run BEFORE the ring
            // pass: the erase is a black rect, and when the sliding text
            // crosses the annulus it would otherwise punch a black box out of
            // freshly drawn ring pixels (visible during relock).
            let erased = old_text.2 >= old_text.0 && old_text.1 < bi;
            if erased {
                lock::clear_rect(fb, old_text.0, old_text.1, old_text.2, old_text.3.min(bi - 1));
            }

            // 3) Ring pass — only the rows that actually need ring pixels:
            //    - brightness stepped → all rows < bi (full recolor),
            //    - sheet grew → just the fresh band rows [bp..bi),
            //    - text erase reached the annulus → the erased rows.
            // Repainting from row 0 every frame marked ~the whole screen and
            // forced a 24 ms full flush per relock frame (visible flicker) on
            // top of a 15+ ms PSRAM-cache-thrashing rewrite of 19k scattered
            // pixels. Skipped entirely while the ring is uniformly faded out
            // (level 0 → its pixels are black, same as the fresh band fill).
            let erase_hit_ring = erased
                && lvl > 0
                && lock::rect_touches_ring(old_text.0, old_text.1, old_text.2, old_text.3.min(bi - 1));
            let ring_min = if lvl != lvl_prev {
                0
            } else {
                let mut m = i32::MAX;
                if bi > bp && lvl > 0 {
                    m = bp;
                }
                if erase_hit_ring {
                    m = m.min(old_text.1.max(0));
                }
                m
            };
            if ring_min < bi {
                let mut acc = RectAcc::empty();
                self.clock
                    .draw_ring_rows(fb, ring_min, bi, lvl * Q / RING_LEVELS, &mut acc);
                if !acc.is_empty() {
                    rects[nrects] = (acc.x0 - 1, acc.y0, acc.x1 + 1, acc.y1);
                    nrects += 1;
                }
            }

            // 4) Text: redraw at the new shift.
            let nb = self.clock.draw_sheet_text(fb, now, bi - H, bi);
            let dirty_text = if old_text.2 < old_text.0 {
                nb
            } else {
                (
                    old_text.0.min(nb.0),
                    old_text.1.min(nb.1),
                    old_text.2.max(nb.2),
                    old_text.3.max(nb.3),
                )
            };
            rects[nrects] = dirty_text;
            nrects += 1;
            *text_bbox = nb;
        }

        for r in rects.iter().take(nrects) {
            if r.2 >= r.0 && r.3 >= r.1 {
                self.wfb.mark_rect(r.0, r.1, r.2, r.3);
            }
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
        let partial = !self.wfb.dmi.overflowed() && dirty_bytes < byte_count / 3;
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
