//! Application scene machine + frame loop (docs/ROADMAP.md M3).
//!
//! Scenes: `Locked` (clock) ⇄ `Unlocked` (fullscreen Spike). M3 wires the
//! switch to a confirmed tap as a temporary trigger; M4 replaces it with the
//! swipe-up grabber drag + eased transitions.
//!
//! Loop shape: fixed 20 fps cadence; each frame composes incremental deltas
//! onto the retained WatchFb, flushes partially (or fully when large/overflow),
//! and spends the cadence remainder polling the touch INT pin — so touch reads
//! happen within microseconds of the controller asserting INT.

use esp_hal::gpio::{Input, Output};
use esp_hal::i2c::master::I2c;
use esp_hal::time::{Duration, Instant};
use esp_hal::Blocking;
use esp_println::println;

use crate::board::{LCD_COL_OFFSET, LCD_HEIGHT, LCD_WIDTH};
use crate::display::qspi_bus::QspiBus;
use crate::display::watch_fb::WatchFb;
use crate::drivers::cst9217;
use crate::input::gestures::{GestureEvent, SwipeTracker};
use crate::scenes::{lock, unlocked};
use crate::time::WallClock;

/// Fixed 20 fps cadence — matches TARGET_FPS in build.rs so the frame-indexed
/// ease schedules take exactly their designed wall-clock duration.
const FRAME_US: u64 = 50_000;
/// Minimum gap between scene switches — a second debounce layer above the
/// recognizer's tap refractory, so the scene can never flicker.
const SCENE_COOLDOWN_MS: u64 = 400;
/// Swipe-up release verdict: unlock when the drag traveled this far…
const UNLOCK_DY: i32 = 466 / 4;
/// …or released with at least this upward velocity (Q8 px/ms ≈ 0.5 px/ms).
const UNLOCK_VEL_Q8: i32 = 128;
/// Consecutive touch I2C errors before the controller is re-initialized.
const TOUCH_REINIT_ERRORS: u32 = 5;

#[derive(Clone, Copy, PartialEq)]
enum Scene {
    Locked,
    Unlocked,
}

/// Scene-changing intents produced by the touch poll, applied at frame top.
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Toggle,
    Unlock,
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
        let byte_count = self.wfb.bytes().len();

        // Prime: WatchFb::new cleared the canvas and marked it fully dirty.
        self.clock.render(&mut self.wfb, 0, &self.wall.now());
        self.bus.flush_bytes(self.wfb.bytes());
        self.wfb.clear_damage();
        println!("First frame: {} ms", anim_start.elapsed().as_millis());

        let mut scene = Scene::Locked;
        let mut action: Option<Action> = None;
        let mut last_scene_switch = Instant::now();
        let mut last_report = Instant::now();
        let mut ema_fps: f32 = 0.0;
        let mut touch_log_ctr: u32 = 0;
        let mut last_touch_read = Instant::now();
        let mut i2c_errors: u32 = 0;
        let mut consec_errors: u32 = 0;

        loop {
            let frame_start = Instant::now();
            let elapsed = anim_start.elapsed().as_millis() as u32;

            self.wall.maybe_resync(&mut self.i2c);
            let now = self.wall.now();

            // Apply at most one scene change per cooldown window (debounce
            // layer 2 — layer 1 is the recognizer's tap refractory).
            if let Some(a) = action.take() {
                let cooled = last_scene_switch.elapsed()
                    >= Duration::from_millis(SCENE_COOLDOWN_MS);
                let next = match (a, scene) {
                    _ if !cooled => None,
                    // Swipe-up unlock (pre-M4: instant switch on release verdict).
                    (Action::Unlock, Scene::Locked) => Some(Scene::Unlocked),
                    // Temporary M3 trigger: a debounced tap toggles either way.
                    (Action::Toggle, Scene::Locked) => Some(Scene::Unlocked),
                    (Action::Toggle, Scene::Unlocked) => Some(Scene::Locked),
                    _ => None,
                };
                if let Some(next) = next {
                    last_scene_switch = Instant::now();
                    match next {
                        Scene::Unlocked => {
                            unlocked::draw(&mut self.wfb);
                            println!("scene: -> Unlocked");
                        }
                        Scene::Locked => {
                            self.clock.repaint_full(&mut self.wfb);
                            println!("scene: -> Locked");
                        }
                    }
                    scene = next;
                }
            }

            let render_start = Instant::now();
            if scene == Scene::Locked {
                self.clock.render(&mut self.wfb, elapsed, &now);
            }
            let render_ms = render_start.elapsed().as_millis() as u32;

            // Skip the flush when nothing changed — panel keeps showing its GRAM.
            // Otherwise: partial windowed flush of dirty spans when the dirty area
            // is small; full frame when the DMI overflowed or per-window overhead
            // would exceed a straight full flush.
            let mut flush_ms = 0u32;
            let mut span_count = 0usize;
            let flush_mode = if self.wfb.is_clean() {
                '-'
            } else {
                let spans = self.wfb.dmi.spans();
                span_count = spans.len();
                let dirty_bytes: usize =
                    spans.iter().map(|s| (s.x1 - s.x0 + 1) as usize * 2).sum();
                let partial = !self.wfb.dmi.overflowed() && dirty_bytes < byte_count / 3;
                let flush_start = Instant::now();
                if partial {
                    self.bus
                        .flush_spans(self.wfb.bytes(), self.wfb.dmi.spans(), LCD_WIDTH, LCD_COL_OFFSET);
                } else {
                    // Partial flushes shrink the panel window — restore it first.
                    self.bus.set_window(
                        LCD_COL_OFFSET,
                        LCD_COL_OFFSET + LCD_WIDTH - 1,
                        0,
                        LCD_HEIGHT - 1,
                    );
                    self.bus.write_command(0x2C);
                    self.bus.flush_bytes(self.wfb.bytes());
                }
                flush_ms = flush_start.elapsed().as_millis() as u32;
                self.wfb.clear_damage();
                if partial { 'P' } else { 'F' }
            };
            let flushed = flush_mode != '-';

            let work_ms = frame_start.elapsed().as_millis() as u32;
            // Cadence remainder = touch poll window.
            self.poll_touch_until(
                frame_start + Duration::from_micros(FRAME_US),
                anim_start,
                &mut touch_log_ctr,
                &mut action,
                &mut last_touch_read,
                &mut i2c_errors,
                &mut consec_errors,
            );

            // Bus-health recovery: repeated touch read failures → re-init chip.
            if consec_errors >= TOUCH_REINIT_ERRORS {
                consec_errors = 0;
                let ok = cst9217::init(&mut self.i2c, &mut self.tp_reset).is_ok();
                println!("touch: reinit after errors ok={}", ok as u8);
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
                    i2c_errors,
                    self.tp_int.is_low() as u8,
                    self.clock.last_bezel_centers,
                    self.clock.last_bezel_center_delta,
                    self.clock.last_bezel_writes
                );
                last_report = Instant::now();
            }
        }
    }

    /// Busy-wait until `deadline` while polling the touch INT pin; INT asserted
    /// → read the report (latest wins, rate-limited to one I2C transaction per
    /// 2 ms so a held-low INT can't hammer the shared bus) and feed the
    /// recognizer. A scene-changing gesture breaks out early so the switch
    /// lands on the very next frame.
    #[allow(clippy::too_many_arguments)]
    fn poll_touch_until(
        &mut self,
        deadline: Instant,
        anim_start: Instant,
        log_ctr: &mut u32,
        action: &mut Option<Action>,
        last_read: &mut Instant,
        i2c_errors: &mut u32,
        consec_errors: &mut u32,
    ) {
        loop {
            let now_ms = anim_start.elapsed().as_millis() as u32;
            let report = if self.tp_int.is_low()
                && last_read.elapsed() >= Duration::from_millis(2)
            {
                *last_read = Instant::now();
                match cst9217::read_touch(&mut self.i2c) {
                    Ok(r) => {
                        *consec_errors = 0;
                        r
                    }
                    Err(()) => {
                        *i2c_errors += 1;
                        *consec_errors += 1;
                        None
                    }
                }
            } else {
                None
            };
            if let Some(t) = report {
                *log_ctr += 1;
                if *log_ctr % 16 == 0 {
                    println!(
                        "touch raw x={} y={} fingers={} pressed={}",
                        t.x, t.y, t.fingers, t.pressed as u8
                    );
                }
            }
            match self.swipe.feed(report, now_ms) {
                GestureEvent::DragStart { x, y } => println!("gesture: drag arm x={x} y={y}"),
                GestureEvent::DragMove { dy } => {
                    if *log_ctr % 8 == 0 {
                        println!("gesture: drag dy={dy}");
                    }
                }
                GestureEvent::DragEnd { dy, vel_q8 } => {
                    let unlock = dy as i32 > UNLOCK_DY || vel_q8 > UNLOCK_VEL_Q8;
                    println!(
                        "gesture: release dy={dy} vel_q8={vel_q8} -> {}",
                        if unlock { "unlock" } else { "springback" }
                    );
                    if unlock {
                        *action = Some(Action::Unlock);
                    }
                }
                GestureEvent::Tap { x, y } => {
                    println!("gesture: tap x={x} y={y}");
                    *action = Some(Action::Toggle);
                }
                GestureEvent::None => {}
            }
            if action.is_some() || Instant::now() >= deadline {
                break;
            }
            core::hint::spin_loop();
        }
    }
}
