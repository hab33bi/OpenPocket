//! W3 app screens (docs/W3-APP-SCREENS-PLAN.md §4): the shared template
//! frame + per-app content, entered through the wheel's open/close morph.
//!
//! Template doctrine (§1): the status clock at the top never moves (drawn
//! by the wheel-side morph renderer / tick_status — the one fixed point);
//! app title in spaced caps at y≈92 in the app's accent at 60%; hero zone
//! y 120–360; ONE cheap signature animation per app, breathing on the
//! saber tempo. W3.2 ships the frame proven on Photos; the other rows show
//! an honest icon-hero placeholder until their W3.3–3.5 passes.

use crate::display::watch_fb::WatchFb;
use crate::scenes::lock::{self, Glyph};
use crate::scenes::water::Water;
use crate::scenes::wheel;
use crate::time::WallTime;
use esp_hal::time::Instant;

const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = W / 2;
const CY: i32 = H / 2;

// Gallery pages: display-ready RGB565-BE, build-time ingested from
// assets/Spike.jpg (page 1) + assets/gallery/* (finale sorts last).
include!(concat!(env!("OUT_DIR"), "/gallery_assets.rs"));

/// Wheel row indices (the wheel row IS the app).
pub const TIME: usize = 0;
pub const GALLERY: usize = 1;
pub const PHONE: usize = 2;
pub const MESSAGES: usize = 3;
pub const ACTIVITY: usize = 4;
pub const SETTINGS: usize = 5;
pub const MUSIC: usize = 6;
pub const WATER: usize = 7;
pub const WEATHER: usize = 8;
pub const TIMER: usize = 9;

/// Timer template ring radius — sized to clear the status clock band at
/// the top (a full-rim ring would cross it; §1: the clock never moves).
const TIMER_R: i32 = 155;

/// Cross-frame app state owned by the run loop (one instance).
pub struct State {
    pub gal_page: usize,
    pub gal_settle: Option<Instant>,
    /// Settings: About panel open; live brightness level (reg 0x51).
    pub set_about: bool,
    pub set_level: u8,
    /// Time app seconds-arc: last RTC second + when it was observed (for
    /// sub-second sweep), last drawn pseudo-angle, last tip rect, and the
    /// minute the big digits show. t_anchor=None ⇒ arc not yet seeded.
    pub t_sec: u8,
    pub t_anchor: Option<Instant>,
    pub t_last_a: i32,
    pub t_tip: (i32, i32, i32, i32),
    pub t_min: u8,
    /// Music vinyl highlight angle last drawn (q10; -1 = unseeded).
    pub mu_theta: i32,
    /// Weather orbit-arc angle last drawn (q10; -1 = unseeded).
    pub we_arc: i32,
    /// Phone dial drift offset last drawn (pseudo units).
    pub ph_drift: i32,
    /// Water liquid simulation (particles/hash/calibration — internal SRAM).
    pub wa: Water,
}

impl State {
    pub const fn new() -> Self {
        Self {
            gal_page: 0,
            gal_settle: None,
            set_about: false,
            set_level: 0xFF,
            t_sec: 255,
            t_anchor: None,
            t_last_a: 0,
            t_tip: (0, 0, -1, -1),
            t_min: 255,
            mu_theta: -1,
            we_arc: -1,
            ph_drift: 0,
            wa: Water::new(),
        }
    }
}

/// Clip line shared with water.rs's CLOCK_Y1 — the reveal rect matches the
/// sim's clip so the status clock (band ~26..66) stays topmost.
const CLOCK_Y1_APPS: i32 = 70;

/// Brightness slider geometry (Settings row 0): a real drag control.
pub const SLIDER_X0: i32 = CX - 90;
pub const SLIDER_W: i32 = 180;
const SLIDER_Y: i32 = CY - 64 + 26;
/// Floor keeps the panel from going black under the finger.
const SLIDER_MIN: i32 = 0x14;

/// Map a (mirror-corrected) screen X onto a brightness level.
pub fn slider_level_from_x(sx: i32) -> u8 {
    (SLIDER_MIN + ((sx - SLIDER_X0).clamp(0, SLIDER_W) * (255 - SLIDER_MIN)) / SLIDER_W) as u8
}

/// Whether a press Y falls inside the slider row's touch band.
pub fn slider_zone(y: i32) -> bool {
    (CY - 64 - 34..=CY - 64 + 44).contains(&y)
}

/// Letter-spacing for spaced-caps titles (splash only now).
const TITLE_SPACING: i32 = 4;
/// Per-app accent color (§4). One restrained accent per app.
pub fn accent(idx: usize) -> (i32, i32, i32) {
    match idx {
        0 => (90, 160, 255),  // Time — azure
        1 => (255, 165, 80),  // Gallery — sunset amber
        2 => (70, 220, 200),  // Phone — teal
        3 => (170, 120, 255), // Messages — violet
        4 => (90, 160, 255),  // Activity — saber trio (azure lead)
        5 => (200, 215, 255), // Settings — ice
        6 => (255, 240, 220), // Music — warm white
        7 => (0, 190, 255),   // Water — neon blue
        8 => (255, 165, 80),  // Weather — amber
        _ => (90, 160, 255),  // Timer — azure
    }
}

/// Whether the app has real content behind its splash. Content apps
/// crossfade splash → content at the end of the open morph; template apps
/// REST on the splash (big centered logo + title below — the honest
/// placeholder until their W3.3–3.5 passes).
pub fn has_content(idx: usize) -> bool {
    // Every app has real content behind its splash. Water's is the liquid sim.
    let _ = idx;
    true
}

/// Water's per-frame entry: the run loop reads the IMU (it owns `self.i2c`)
/// and hands the raw triple in; `imu = None` on a bus fault. Keeps `apps`
/// I2C-free (docs/water/IMPL-SPEC.md §5–7). Water stays in the generic
/// `tick`'s `_ => {}` arm — it is driven from the run-loop call site because
/// it needs the accelerometer.
pub fn water_tick(wfb: &mut WatchFb, imu: Option<(i16, i16, i16)>, elapsed_ms: u32, st: &mut State) {
    st.wa.tick(wfb, imu, elapsed_ms);
}

/// Every app shows the shared status clock — the one fixed point (§1). The
/// Time app used to hide it, which produced a disappear artifact on open;
/// it now keeps the clock like everything else (user).
pub fn shows_status(_idx: usize) -> bool {
    true
}

/// Time app layout: big digits + date optically centered as a group.
const TIME_BASE_Y: i32 = 244;
const DATE_BASE_Y: i32 = 298;

/// Splash title: spaced caps in the app accent, under the centered logo.
/// (The only remaining title — the in-app titles were removed; the splash
/// still names the app on the way in.)
pub fn draw_splash_title(wfb: &mut WatchFb, fx: &mut wheel::WheelFx, idx: usize, alpha: i32) {
    let name = wheel::WHEEL_APPS[idx].name;
    let (tx, tw) = title_metrics(name);
    let by = wheel::SPLASH_TITLE_BASE_Y;
    {
        let fb = wfb.buf_mut();
        draw_title(fb, name, tx, by, (200 * alpha) >> 8, accent(idx));
    }
    fx.push(tx - 2, by - 32, tx + tw + 2, by + 10);
    wfb.mark_rect(tx - 2, by - 32, tx + tw + 2, by + 10);
}

/// Breathing amplitude on the wheel-ring tempo (~4 s triangle): the whole
/// OS breathes at one cadence.
fn breath(elapsed_ms: u32) -> i32 {
    let ph = (elapsed_ms / 8) % 512;
    let tri = if ph < 256 { ph } else { 511 - ph } as i32;
    150 + ((tri * 55) >> 8)
}

/// App content at reveal `q` (0..=256): rises 20 px while fading in with
/// `q` — the wheel-intro vocabulary. Drawn AFTER the wheel-side morph
/// frame, sharing its rect cache (rects pushed here are cleared by the
/// next morph frame like any content). The status line is NOT drawn here
/// (wheel-side owns it, topmost).
#[allow(clippy::too_many_arguments)]
pub fn draw_reveal(
    wfb: &mut WatchFb,
    fx: &mut wheel::WheelFx,
    now: &WallTime,
    batt: Option<u8>,
    idx: usize,
    q_q8: i32,
    elapsed_ms: u32,
    st: &mut State,
) {
    if q_q8 <= 0 {
        return;
    }
    // App titles removed (user): the splash logo names the app on the way
    // in; the content is the hero. Content now has the top of the screen.
    let rise = ((256 - q_q8) * 20) >> 8;

    if idx == TIME {
        // §4.1 — big centered time + date. Fades in PLACE (no rise) under
        // one generous static clear rect: while the reveal scrubbed, a
        // moving draw vs its trailing clear could strand 1-2 px digit
        // slivers for a beat (user-observed). Static draw + static rect
        // makes draw and clear identical by construction. The seconds arc
        // is seeded by the first rest tick (one clean full flush).
        let (tbuf, dbuf, dlen) = time_strings(now);
        let t_str = core::str::from_utf8(&tbuf).unwrap_or("00:00");
        let d_str = core::str::from_utf8(&dbuf[..dlen]).unwrap_or("");
        let tw = wheel::text_width(t_str, &lock::TIME_GLYPHS);
        let dw = wheel::text_width(d_str, &lock::TEXT_GLYPHS);
        {
            let fb = wfb.buf_mut();
            wheel::draw_text_at(fb, t_str, CX - tw / 2, TIME_BASE_Y, q_q8, &lock::TIME_GLYPHS);
            wheel::draw_text_at(fb, d_str, CX - dw / 2, DATE_BASE_Y, (150 * q_q8) >> 8, &lock::TEXT_GLYPHS);
        }
        let r = (CX - 180, 140, CX + 180, 322);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
        st.t_anchor = None; // reseed the arc on the first rest tick
        st.t_min = now.minute;
    } else if idx == ACTIVITY {
        // §4.5 — triple rings sweep in with the reveal, staggered.
        {
            let fb = wfb.buf_mut();
            for (i, (rc, closure, tint)) in ACT_RINGS.iter().enumerate() {
                let p = ((q_q8 - i as i32 * 36) * 256 / 184).clamp(0, 256);
                let sweep = (closure * 1024 / 100) * p >> 8;
                if sweep > 0 {
                    ring_with_caps(fb, *rc, sweep, *tint, 230);
                }
            }
            // Center readout fitted INSIDE the innermost ring (r=64): the
            // step count auto-scales to the hole width, "steps" beneath.
            let steps = "6412";
            let full = wheel::text_width(steps, &lock::LABELF_GLYPHS).max(1);
            let sc = ((90 << 8) / full).min(256);
            let sw2 = (full * sc) >> 8;
            wheel::draw_text_scaled(fb, steps, CX - sw2 / 2, CY + 2, q_q8, &lock::LABELF_GLYPHS, sc, false);
            let cap = "steps";
            let cw2 = wheel::text_width(cap, &lock::TEXT_GLYPHS);
            wheel::draw_text_at(fb, cap, CX - cw2 / 2, CY + 30, (102 * q_q8) >> 8, &lock::TEXT_GLYPHS);
        }
        let r = (CX - 132, CY - 132, CX + 132, CY + 132);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == SETTINGS {
        {
            let fb = wfb.buf_mut();
            draw_settings(fb, st, q_q8, rise, batt, elapsed_ms);
        }
        let r = (40, 118 + rise, W - 40, 340 + rise);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == MUSIC {
        {
            let fb = wfb.buf_mut();
            draw_music(fb, st, q_q8, rise, elapsed_ms);
        }
        let r = (CX - 130, 110 + rise, CX + 130, 450);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == WEATHER {
        {
            let fb = wfb.buf_mut();
            draw_weather(fb, st, q_q8, rise, elapsed_ms);
        }
        let r = (50, 96 + rise, W - 50, 344 + rise);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == PHONE {
        {
            let fb = wfb.buf_mut();
            draw_phone(fb, st, q_q8, rise, elapsed_ms);
        }
        let r = (CX - 145, 80 + rise, CX + 145, 404 + rise);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == MESSAGES {
        {
            let fb = wfb.buf_mut();
            draw_messages(fb, q_q8, rise, elapsed_ms);
        }
        let r = (56, 104 + rise, W - 56, 352 + rise);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == TIMER {
        // §4.10 — TEMPLATE ONLY: a full azure rim ring at 100%, "05:00"
        // centered, a ghosted start glyph beneath. The functional
        // countdown (tap start/pause, ring depleting azure→red, triple
        // pulse at zero) is specced and deferred to W4; rests perfectly
        // still (no tick) — an honest, beautiful placeholder.
        {
            let fb = wfb.buf_mut();
            draw_ring_arc(fb, CX, CY, TIMER_R - 5, TIMER_R + 5, 1024, AZURE, (210 * q_q8) >> 8);
            let t = "05:00";
            let tw = wheel::text_width(t, &lock::LABELF_GLYPHS);
            wheel::draw_text_at(fb, t, CX - tw / 2, CY + 16, q_q8, &lock::LABELF_GLYPHS);
            blit_icon_tint(fb, wheel::TR_PLAY, wheel::TR_PLAY_PX, CX, CY + 74, ICE, (70 * q_q8) >> 8);
        }
        let r = (CX - TIMER_R - 8, CY - TIMER_R - 8, CX + TIMER_R + 8, CY + TIMER_R + 8);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    } else if idx == WATER {
        // The pool fades up from black as the morph completes; the first
        // water_tick (run loop) calibrates and takes over the physics.
        st.wa.reveal(wfb, q_q8, elapsed_ms);
        let r = (CX - 226, CLOCK_Y1_APPS, CX + 226, H - 1);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    }
    // Any remaining app: the flown icon (wheel-side) IS the hero.
}

/// Rest-frame signature animation (§1: ONE breathing element per app,
/// partial flush, tick_ring doctrine — clear the rect, redraw, mark).
pub fn tick(wfb: &mut WatchFb, idx: usize, now: &WallTime, elapsed_ms: u32, st: &mut State) {
    match idx {
        TIME => time_tick(wfb, now, st),
        ACTIVITY => activity_tick(wfb, elapsed_ms),
        MUSIC => music_tick(wfb, st, elapsed_ms),
        WEATHER => weather_tick(wfb, st, elapsed_ms),
        PHONE => phone_tick(wfb, st, elapsed_ms),
        MESSAGES => messages_tick(wfb, elapsed_ms),
        // Water rests on its splash until the liquid sim lands; template
        // apps rest perfectly still.
        _ => {}
    }
}

// ---------------------------------------------------------------------
// Time (W3 §4.1) — big digits + date + a 2 px azure seconds arc sweeping
// the rim, its tip carrying a soft glow. Every write is idempotent (SET
// over black for arc pixels, per-channel MAX for glow), so partial
// redraws never self-stack.
// ---------------------------------------------------------------------

const ARC_R_IN: i32 = 224;
const ARC_R_OUT: i32 = 228;
const AZURE: (i32, i32, i32) = (70, 155, 255);

fn time_strings(now: &WallTime) -> ([u8; 5], [u8; 12], usize) {
    let mut t = [b'0'; 5];
    t[0] = b'0' + now.hour / 10;
    t[1] = b'0' + now.hour % 10;
    t[2] = b':';
    t[3] = b'0' + now.minute / 10;
    t[4] = b'0' + now.minute % 10;
    // "TUE 21 JUL"
    const WDAY: [&[u8; 3]; 7] = [b"SUN", b"MON", b"TUE", b"WED", b"THU", b"FRI", b"SAT"];
    const MON3: [&[u8; 3]; 12] = [
        b"JAN", b"FEB", b"MAR", b"APR", b"MAY", b"JUN", b"JUL", b"AUG", b"SEP", b"OCT", b"NOV",
        b"DEC",
    ];
    let mut d = [b' '; 12];
    let mut n = 0;
    d[..3].copy_from_slice(WDAY[weekday(now.year, now.month, now.day)]);
    n += 4;
    if now.day >= 10 {
        d[n] = b'0' + now.day / 10;
        n += 1;
    }
    d[n] = b'0' + now.day % 10;
    n += 2;
    let m = ((now.month.max(1) - 1) as usize).min(11);
    d[n..n + 3].copy_from_slice(MON3[m]);
    n += 3;
    (t, d, n)
}

/// Sakamoto's day-of-week, 0 = Sunday.
fn weekday(y: u16, m: u8, d: u8) -> usize {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = y as i32;
    if m < 3 {
        y -= 1;
    }
    ((y + y / 4 - y / 100 + y / 400 + T[((m.max(1) - 1) as usize).min(11)] + d as i32)
        .rem_euclid(7)) as usize
}

fn time_tick(wfb: &mut WatchFb, now: &WallTime, st: &mut State) {
    if st.t_anchor.is_none() {
        // Fresh open: seed the full face deterministically, but with
        // TARGETED clears (center rect + arc annulus) rather than a full
        // fill — a full fill wiped the status clock the morph had drawn at
        // the top, which is exactly what made it vanish. The status band
        // (y 26..66) is left untouched, so the clock stays live; the
        // run-loop keeps it on minute rollover.
        st.t_sec = now.second;
        st.t_min = now.minute;
        st.t_anchor = Some(Instant::now());
        let a = ((now.second as i32) * 1024 / 60).max(4);
        let (tbuf, dbuf, dlen) = time_strings(now);
        let t_str = core::str::from_utf8(&tbuf).unwrap_or("00:00");
        let d_str = core::str::from_utf8(&dbuf[..dlen]).unwrap_or("");
        let tw = wheel::text_width(t_str, &lock::TIME_GLYPHS);
        let dw = wheel::text_width(d_str, &lock::TEXT_GLYPHS);
        let fb = wfb.buf_mut();
        fill_rect_black(fb, CX - 180, 140, CX + 180, 322);
        clear_annulus(fb, ARC_R_IN - 2, ARC_R_OUT + 2);
        wheel::draw_text_at(fb, t_str, CX - tw / 2, TIME_BASE_Y, 256, &lock::TIME_GLYPHS);
        wheel::draw_text_at(fb, d_str, CX - dw / 2, DATE_BASE_Y, 150, &lock::TEXT_GLYPHS);
        draw_ring_arc(fb, CX, CY, ARC_R_IN, ARC_R_OUT, a, AZURE, 230);
        let (vx, vy) = pseudo_dir(a);
        let tx = CX + (226 * vx >> 8);
        let ty = CY + (226 * vy >> 8);
        soft_dot(fb, tx, ty, 9, AZURE, 210);
        st.t_last_a = a;
        st.t_tip = (tx - 22, ty - 22, tx + 22, ty + 22);
        wfb.mark_rect(0, 0, W - 1, H - 1);
        return;
    }
    // Sub-second sweep from the RTC second + a local anchor.
    if st.t_sec != now.second {
        st.t_sec = now.second;
        st.t_anchor = Some(Instant::now());
    }
    // Big digits + date on minute rollover.
    if st.t_min != now.minute {
        st.t_min = now.minute;
        let (tbuf, dbuf, dlen) = time_strings(now);
        let t_str = core::str::from_utf8(&tbuf).unwrap_or("00:00");
        let d_str = core::str::from_utf8(&dbuf[..dlen]).unwrap_or("");
        let tw = wheel::text_width(t_str, &lock::TIME_GLYPHS);
        let dw = wheel::text_width(d_str, &lock::TEXT_GLYPHS);
        let fb = wfb.buf_mut();
        fill_rect_black(fb, CX - 170, 150, CX + 170, 310);
        wheel::draw_text_at(fb, t_str, CX - tw / 2, TIME_BASE_Y, 256, &lock::TIME_GLYPHS);
        wheel::draw_text_at(fb, d_str, CX - dw / 2, DATE_BASE_Y, 150, &lock::TEXT_GLYPHS);
        wfb.mark_rect(CX - 170, 150, CX + 170, 310);
    }
    let ms = (st.t_sec as i32) * 1000
        + (st.t_anchor.map(|t| t.elapsed().as_millis() as i32).unwrap_or(0)).min(999);
    let a = (ms * 1024 / 60_000).clamp(0, 1023).max(4);
    if a == st.t_last_a {
        return;
    }
    let fb = wfb.buf_mut();
    if a < st.t_last_a {
        // New minute: the arc restarts from 12 o'clock.
        clear_annulus(fb, ARC_R_IN - 2, ARC_R_OUT + 2);
        wfb.mark_rect(0, 0, W - 1, H - 1);
        st.t_last_a = 0;
        st.t_tip = (CX - 22, CY - 226 - 22, CX + 22, CY - 226 + 22);
        return;
    }
    // Erase the old tip's glow, repaint arc beneath it, advance the tip.
    let old = st.t_tip;
    fill_rect_black(fb, old.0, old.1, old.2, old.3);
    arc_in_rect(fb, ARC_R_IN, ARC_R_OUT, a, AZURE, 230, old);
    let (vx, vy) = pseudo_dir(a);
    let tx = CX + (226 * vx >> 8);
    let ty = CY + (226 * vy >> 8);
    let nr = (tx - 22, ty - 22, tx + 22, ty + 22);
    arc_in_rect(fb, ARC_R_IN, ARC_R_OUT, a, AZURE, 230, nr);
    soft_dot(fb, tx, ty, 9, AZURE, 210);
    st.t_last_a = a;
    st.t_tip = nr;
    let u = (old.0.min(nr.0), old.1.min(nr.1), old.2.max(nr.2), old.3.max(nr.3));
    wfb.mark_rect(u.0, u.1, u.2, u.3);
}

// ---------------------------------------------------------------------
// Activity (W3 §4.5) — saber-palette triple rings; tips breathe at rest.
// ---------------------------------------------------------------------

/// (center radius, closure %, tint) — outer→inner: azure, violet, teal.
/// Outer ring tops out at y=105, safely below the title (user: rings must
/// not overlap it).
const ACT_RINGS: [(i32, i32, (i32, i32, i32)); 3] = [
    (120, 82, (70, 150, 255)),
    (92, 64, (170, 120, 255)),
    (64, 91, (70, 220, 200)),
];
const ACT_HALF_W: i32 = 8;

/// Rounded stroke caps at both arc ends (Apple-ring style): a solid AA
/// disc of half-stroke radius, MAX-blended — merges seamlessly with the
/// band and never self-stacks, so the sweeping tip can stamp its cap
/// every frame (the trail coincides with the band it just drew).
fn cap_dot(fb: &mut [u8], cx: i32, cy: i32, r: i32, tint: (i32, i32, i32), v: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r * r {
                continue;
            }
            let a = if d2 <= (r - 1) * (r - 1) { v } else { v / 2 };
            max_px(fb, cx + dx, cy + dy, tint, a);
        }
    }
}

/// One ring arc with rounded caps at 12 o'clock and at the sweep tip.
fn ring_with_caps(fb: &mut [u8], rc: i32, sweep: i32, tint: (i32, i32, i32), v: i32) {
    draw_ring_arc(fb, CX, CY, rc - ACT_HALF_W, rc + ACT_HALF_W, sweep, tint, v);
    cap_dot(fb, CX, CY - rc, ACT_HALF_W, tint, v);
    let (vx, vy) = pseudo_dir(sweep);
    cap_dot(fb, CX + (rc * vx >> 8), CY + (rc * vy >> 8), ACT_HALF_W, tint, v);
}

fn activity_tick(wfb: &mut WatchFb, elapsed_ms: u32) {
    // Tips breathe on the shared tempo: erase each tip rect, repaint any
    // ring pixels inside it (all three bands — rect corners can reach the
    // neighbor), re-glow at the breathing alpha (MAX — idempotent).
    let b = breath(elapsed_ms);
    let fb = wfb.buf_mut();
    let mut rects = [(0i32, 0i32, 0i32, 0i32); 3];
    for (i, (rc, closure, tint)) in ACT_RINGS.iter().enumerate() {
        let sweep = closure * 1024 / 100;
        let (vx, vy) = pseudo_dir(sweep);
        let tx = CX + (rc * vx >> 8);
        let ty = CY + (rc * vy >> 8);
        let r = (tx - 16, ty - 16, tx + 16, ty + 16);
        fill_rect_black(fb, r.0, r.1, r.2, r.3);
        for (rc2, closure2, tint2) in ACT_RINGS.iter() {
            let sweep2 = closure2 * 1024 / 100;
            arc_in_rect(fb, rc2 - ACT_HALF_W, rc2 + ACT_HALF_W, sweep2, *tint2, 230, r);
            // Restore any cap the erase rect clipped.
            let (vx2, vy2) = pseudo_dir(sweep2);
            cap_dot(fb, CX + (rc2 * vx2 >> 8), CY + (rc2 * vy2 >> 8), ACT_HALF_W, *tint2, 230);
        }
        soft_dot(fb, tx, ty, 10, *tint, (b * 240) >> 8);
        rects[i] = r;
    }
    for r in rects {
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    }
}

// ---------------------------------------------------------------------
// Settings (W3 §4.6) — mini list; Brightness is REAL (reg 0x51 presets),
// About shows real data only.
// ---------------------------------------------------------------------

const SET_ROWS: [&str; 3] = ["Brightness", "Display", "About"];
const SET_ROW_Y: [i32; 3] = [CY - 64, CY, CY + 64];

fn draw_settings(fb: &mut [u8], st: &State, q: i32, rise: i32, batt: Option<u8>, elapsed_ms: u32) {
    if st.set_about {
        let mut lines: [([u8; 20], usize); 5] = [([0; 20], 0); 5];
        let mk = |buf: &mut [u8; 20], s: &[u8]| -> usize {
            buf[..s.len()].copy_from_slice(s);
            s.len()
        };
        lines[0].1 = {
            let g = crate::time::FW_GIT.as_bytes();
            let n = mk(&mut lines[0].0, b"FW ");
            let m = g.len().min(17);
            lines[0].0[n..n + m].copy_from_slice(&g[..m]);
            n + m
        };
        lines[1].1 = mk(&mut lines[1].0, b"ESP32-S3");
        lines[2].1 = mk(&mut lines[2].0, b"466x466 CO5300");
        lines[3].1 = match batt {
            Some(p) => {
                let n = mk(&mut lines[3].0, b"BATT ");
                let mut n = n;
                if p >= 100 {
                    lines[3].0[n..n + 3].copy_from_slice(b"100");
                    n += 3;
                } else {
                    if p >= 10 {
                        lines[3].0[n] = b'0' + p / 10;
                        n += 1;
                    }
                    lines[3].0[n] = b'0' + p % 10;
                    n += 1;
                }
                lines[3].0[n] = b'%';
                n + 1
            }
            None => mk(&mut lines[3].0, b"USB POWER"),
        };
        lines[4].1 = {
            let mins = (elapsed_ms / 60_000) as usize;
            let n = mk(&mut lines[4].0, b"UP ");
            let mut n = n;
            let mut wrote = false;
            if mins >= 60 {
                let h = mins / 60;
                if h >= 10 {
                    lines[4].0[n] = b'0' + (h / 10) as u8;
                    n += 1;
                }
                lines[4].0[n] = b'0' + (h % 10) as u8;
                n += 1;
                lines[4].0[n] = b'H';
                lines[4].0[n + 1] = b' ';
                n += 2;
                wrote = true;
            }
            let m = mins % 60;
            if m >= 10 || !wrote {
                if m >= 10 {
                    lines[4].0[n] = b'0' + (m / 10) as u8;
                    n += 1;
                }
                lines[4].0[n] = b'0' + (m % 10) as u8;
                n += 1;
            }
            lines[4].0[n..n + 4].copy_from_slice(b" MIN");
            n + 4
        };
        for (i, (buf, len)) in lines.iter().enumerate() {
            let s = core::str::from_utf8(&buf[..*len]).unwrap_or("");
            let tw = wheel::text_width(s, &lock::TEXT_GLYPHS);
            let a = if i == 0 { q } else { (170 * q) >> 8 };
            wheel::draw_text_at(fb, s, CX - tw / 2, 172 + i as i32 * 42 + rise, a, &lock::TEXT_GLYPHS);
        }
        return;
    }
    // Row 0: label + the live brightness slider beneath it.
    draw_bright_row(fb, st, q, rise);
    for (i, name) in SET_ROWS.iter().enumerate().skip(1) {
        let y = SET_ROW_Y[i] + rise;
        let tw = wheel::text_width(name, &lock::TEXT_GLYPHS);
        wheel::draw_text_at(fb, name, CX - tw / 2, y + 11, (220 * q) >> 8, &lock::TEXT_GLYPHS);
    }
}

/// The Brightness row: label above a real slider (track, azure fill,
/// white knob at the live level).
fn draw_bright_row(fb: &mut [u8], st: &State, q: i32, rise: i32) {
    let name = SET_ROWS[0];
    let tw = wheel::text_width(name, &lock::TEXT_GLYPHS);
    wheel::draw_text_at(fb, name, CX - tw / 2, SET_ROW_Y[0] + rise - 4, (220 * q) >> 8, &lock::TEXT_GLYPHS);
    let y = SLIDER_Y + rise;
    let pos = SLIDER_X0
        + ((st.set_level as i32 - SLIDER_MIN).clamp(0, 255 - SLIDER_MIN) * SLIDER_W)
            / (255 - SLIDER_MIN);
    // Track (ghost) + filled portion (azure) as 4 px rounded bars.
    for x in SLIDER_X0..=SLIDER_X0 + SLIDER_W {
        let (tint, v) = if x <= pos { (AZURE, 230) } else { (ICE, 70) };
        for dy in -1..=2 {
            blend_px(fb, x, y + dy, tint, (v * q) >> 8);
        }
    }
    cap_dot(fb, SLIDER_X0 - 1, y + 1, 2, ICE, (70 * q) >> 8);
    cap_dot(fb, SLIDER_X0 + SLIDER_W + 1, y + 1, 2, ICE, (70 * q) >> 8);
    // Knob: solid white disc.
    cap_dot(fb, pos, y + 1, 8, TITLE_WHITE, (255 * q) >> 8);
}

/// Live slider update (drag session / tap): redraw only the row band.
pub fn settings_set_level(wfb: &mut WatchFb, st: &mut State, level: u8) {
    st.set_level = level;
    {
        let fb = wfb.buf_mut();
        fill_rect_black(fb, 60, SET_ROW_Y[0] - 34, W - 60, SET_ROW_Y[0] + 44);
        draw_bright_row(fb, st, 256, 0);
    }
    wfb.mark_rect(60, SET_ROW_Y[0] - 34, W - 60, SET_ROW_Y[0] + 44);
}

/// A tap inside the Settings content zone (the slider row is owned by the
/// drag session; here only About toggles).
pub fn settings_tap(wfb: &mut WatchFb, st: &mut State, y: i32, batt: Option<u8>, elapsed_ms: u32) {
    if st.set_about {
        st.set_about = false;
    } else if (110..=430).contains(&y) && y >= CY + 32 {
        st.set_about = true;
    } else {
        return;
    }
    {
        let fb = wfb.buf_mut();
        fill_rect_black(fb, 30, 110, W - 30, 360);
        draw_settings(fb, st, 256, 0, batt, elapsed_ms);
    }
    wfb.mark_rect(30, 110, W - 30, 360);
}

// ---------------------------------------------------------------------
// Music (W3 §4.7) — now-playing: procedural vinyl with a slowly rotating
// sheen, BLUE IN GREEN / MILES DAVIS, progress hairline, ghosted
// transport. Every disc pixel is a pure function of (r, angle, θ), so any
// region repaints idempotently at any time.
// ---------------------------------------------------------------------

const MU_C: (i32, i32) = (CX, 222);
const MU_R: i32 = 105;
const MU_LABEL_R: i32 = 30;
const WARM: (i32, i32, i32) = (255, 236, 210);
const VINYL_AMBER: (i32, i32, i32) = (196, 116, 48);

/// Vinyl rotation angle (q10) — ~4 rpm (one rev / 15 s).
fn mu_theta(elapsed_ms: u32) -> i32 {
    ((elapsed_ms as i64 * 1024 / 15_000) % 1024) as i32
}

/// One disc pixel: label / grooves / rotating sheen. SET write. `cy` is
/// the disc center's live Y (rise-shifted during the reveal).
fn music_px(fb: &mut [u8], x: i32, y: i32, cy: i32, theta: i32, q: i32) {
    let (dx, dy) = (x - MU_C.0, y - cy);
    let d2 = dx * dx + dy * dy;
    if d2 > MU_R * MU_R {
        return;
    }
    let r = isqrt(d2 as u32) as i32;
    if r < 3 {
        return; // spindle hole stays black
    }
    if r <= MU_LABEL_R {
        let v = if r >= MU_LABEL_R - 2 { 150 } else { 235 };
        set_px(fb, x, y, VINYL_AMBER, (v * q) >> 8);
        return;
    }
    let mut v = if r % 4 == 0 { 30 } else { 11 };
    let a = pseudo_angle(dx, dy);
    let da = adist(a, theta).min(adist(a, theta + 512));
    if da < 36 {
        v += (36 - da) * 15 / 36;
    }
    if r >= MU_R - 1 {
        v = v * (MU_R + 1 - r).clamp(0, 2) / 2; // soft rim
    }
    set_px(fb, x, y, ICE, (v * q) >> 8);
}

fn draw_music(fb: &mut [u8], st: &mut State, q: i32, rise: i32, elapsed_ms: u32) {
    let theta = mu_theta(elapsed_ms);
    let cy = MU_C.1 + rise;
    for y in (cy - MU_R).max(0)..=(cy + MU_R).min(H - 1) {
        for x in (MU_C.0 - MU_R).max(0)..=(MU_C.0 + MU_R).min(W - 1) {
            music_px(fb, x, y, cy, theta, q);
        }
    }
    st.mu_theta = theta;
    let t1 = "BLUE IN GREEN";
    let t2 = "MILES DAVIS";
    let w1 = wheel::text_width(t1, &lock::TEXT_GLYPHS);
    let w2 = wheel::text_width(t2, &lock::TEXT_GLYPHS);
    wheel::draw_text_at(fb, t1, CX - w1 / 2, 360 + rise, (235 * q) >> 8, &lock::TEXT_GLYPHS);
    wheel::draw_text_at(fb, t2, CX - w2 / 2, 388 + rise, (150 * q) >> 8, &lock::TEXT_GLYPHS);
    // Progress hairline, 40% played.
    let y = 404 + rise;
    for x in CX - 100..=CX + 100 {
        let played = x <= CX - 100 + 80;
        let (tint, v) = if played { (WARM, 170) } else { (ICE, 60) };
        blend_px(fb, x, y, tint, (v * q) >> 8);
    }
    // Ghosted transport.
    for (sprite, px, cx) in [
        (wheel::TR_PREV, wheel::TR_PREV_PX, CX - 64),
        (wheel::TR_PLAY, wheel::TR_PLAY_PX, CX),
        (wheel::TR_NEXT, wheel::TR_NEXT_PX, CX + 64),
    ] {
        blit_icon_tint(fb, sprite, px, cx, 430, ICE, (80 * q) >> 8);
    }
}

/// Rest tick: repaint only the sheen sectors (old + new, both mirrors).
fn music_tick(wfb: &mut WatchFb, st: &mut State, elapsed_ms: u32) {
    let theta = mu_theta(elapsed_ms);
    if theta == st.mu_theta {
        return;
    }
    let old = if st.mu_theta < 0 { theta } else { st.mu_theta };
    st.mu_theta = theta;
    let mut rects = [(0i32, 0i32, -1i32, -1i32); 4];
    {
        let fb = wfb.buf_mut();
        for (k, base) in [theta, theta + 512, old, old + 512].into_iter().enumerate() {
            let r = sector_bbox(MU_C.0, MU_C.1, MU_LABEL_R, MU_R, base, 40);
            for y in r.1.max(0)..=r.3.min(H - 1) {
                for x in r.0.max(0)..=r.2.min(W - 1) {
                    music_px(fb, x, y, MU_C.1, theta, 256);
                }
            }
            rects[k] = r;
        }
    }
    for r in rects {
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    }
}

// ---------------------------------------------------------------------
// Weather (W3 §4.9) — 22° with a glowing sun, CLEAR SKIES, hi/lo, and a
// faint highlight arc orbiting the sun over 20 s. The sun's glow is a
// deterministic radial function, so the orbit band repaints exactly.
// ---------------------------------------------------------------------

const SUN_C: (i32, i32) = (CX - 92, 158);
const AMBER_HOT: (i32, i32, i32) = (255, 190, 110);

/// Deterministic layered sun glow at distance d (0..=52).
fn sun_glow_v(d: i32) -> i32 {
    let l = |a: i32, reach: i32| {
        if d >= reach {
            0
        } else {
            a * (reach - d) * (reach - d) / (reach * reach)
        }
    };
    (l(95, 52) + l(150, 26)).min(210)
}

fn weather_orbit(elapsed_ms: u32) -> i32 {
    ((elapsed_ms as i64 * 1024 / 20_000) % 1024) as i32
}

/// One orbit-band pixel (r 42..=46 around the sun): glow base + arc
/// boost. `sy` = the sun center's live Y (rise-shifted during reveal).
fn orbit_px(fb: &mut [u8], x: i32, y: i32, sy: i32, phi: i32, q: i32) {
    let (dx, dy) = (x - SUN_C.0, y - sy);
    let d2 = dx * dx + dy * dy;
    if !(42 * 42..=46 * 46).contains(&d2) {
        return;
    }
    let d = isqrt(d2 as u32) as i32;
    let mut v = sun_glow_v(d);
    let da = adist(pseudo_angle(dx, dy), phi);
    if da < 30 {
        v += (30 - da) * 130 / 30;
    }
    set_px(fb, x, y, AMBER, (v.min(255) * q) >> 8);
}

fn draw_weather(fb: &mut [u8], st: &mut State, q: i32, rise: i32, elapsed_ms: u32) {
    let (sx, sy) = (SUN_C.0, SUN_C.1 + rise);
    // Glow disc (deterministic radial), then the sun glyph.
    for y in (sy - 52).max(0)..=(sy + 52).min(H - 1) {
        for x in (sx - 52).max(0)..=(sx + 52).min(W - 1) {
            let d2 = (x - sx) * (x - sx) + (y - sy) * (y - sy);
            if d2 > 52 * 52 {
                continue;
            }
            let v = sun_glow_v(isqrt(d2 as u32) as i32);
            if v > 4 {
                set_px(fb, x, y, AMBER, (v * q) >> 8);
            }
        }
    }
    blit_icon_tint(fb, wheel::SUN, wheel::SUN_PX, sx, sy, AMBER_HOT, (240 * q) >> 8);
    let phi = weather_orbit(elapsed_ms);
    for y in (sy - 47).max(0)..=(sy + 47).min(H - 1) {
        for x in (sx - 47).max(0)..=(sx + 47).min(W - 1) {
            orbit_px(fb, x, y, sy, phi, q);
        }
    }
    st.we_arc = phi;
    // 22° — TIME digits at 86% for the big reading; hand-drawn degree ring.
    let t = "22";
    let tw = (wheel::text_width(t, &lock::TIME_GLYPHS) * 220) >> 8;
    let tx = CX + 26 - tw / 2;
    wheel::draw_text_scaled(fb, t, tx, 236 + rise, q, &lock::TIME_GLYPHS, 220, false);
    draw_ring_arc_at(fb, tx + tw + 14, 236 + rise - 56, 5, 8, 1024, ICE, (220 * q) >> 8);
    let cs = "CLEAR SKIES";
    let cw = wheel::text_width(cs, &lock::TEXT_GLYPHS);
    wheel::draw_text_at(fb, cs, CX - cw / 2, 292 + rise, (128 * q) >> 8, &lock::TEXT_GLYPHS);
    let hl = "26 / 14";
    let hw = wheel::text_width(hl, &lock::TEXT_GLYPHS);
    let hx = CX - hw / 2;
    wheel::draw_text_at(fb, hl, hx, 330 + rise, (77 * q) >> 8, &lock::TEXT_GLYPHS);
    let w26 = wheel::text_width("26", &lock::TEXT_GLYPHS);
    draw_ring_arc_at(fb, hx + w26 + 6, 330 + rise - 20, 2, 4, 1024, ICE, (77 * q) >> 8);
    draw_ring_arc_at(fb, hx + hw + 6, 330 + rise - 20, 2, 4, 1024, ICE, (77 * q) >> 8);
}

/// Rest tick: repaint the orbit band only.
fn weather_tick(wfb: &mut WatchFb, st: &mut State, elapsed_ms: u32) {
    let phi = weather_orbit(elapsed_ms);
    if phi == st.we_arc {
        return;
    }
    st.we_arc = phi;
    let fb = wfb.buf_mut();
    for y in (SUN_C.1 - 47).max(0)..=(SUN_C.1 + 47).min(H - 1) {
        for x in (SUN_C.0 - 47).max(0)..=(SUN_C.0 + 47).min(W - 1) {
            orbit_px(fb, x, y, SUN_C.1, phi, 256);
        }
    }
    wfb.mark_rect(SUN_C.0 - 47, SUN_C.1 - 47, SUN_C.0 + 47, SUN_C.1 + 47);
}

// ---------------------------------------------------------------------
// Phone (W3 §4.3) — the rotary-dial object: digits on a ring around a
// teal contact circle (Amina), recents ghosted. The dial drifts ±6
// pseudo-units over 8 s.
// ---------------------------------------------------------------------

const PH_C: (i32, i32) = (CX, 205);
const TEAL: (i32, i32, i32) = (70, 220, 200);
const TEAL_DARK: (i32, i32, i32) = (16, 46, 42);

fn phone_digit_pos(i: usize, drift: i32) -> (i32, i32, char) {
    let a = 140 + i as i32 * 82 + drift;
    let (vx, vy) = pseudo_dir(a);
    let ch = if i < 9 { (b'1' + i as u8) as char } else { '0' };
    (PH_C.0 + (118 * vx >> 8), PH_C.1 + (118 * vy >> 8), ch)
}

fn draw_phone_digit(fb: &mut [u8], x: i32, y: i32, ch: char, alpha: i32) {
    let mut b = [0u8; 4];
    let s = ch.encode_utf8(&mut b);
    let w = wheel::text_width(s, &lock::TEXT_GLYPHS);
    wheel::draw_text_at(fb, s, x - w / 2, y + 9, alpha, &lock::TEXT_GLYPHS);
}

fn draw_phone(fb: &mut [u8], st: &mut State, q: i32, rise: i32, elapsed_ms: u32) {
    let (cx, cy) = (PH_C.0, PH_C.1 + rise);
    fill_disc(fb, cx, cy, 52, TEAL_DARK, q);
    draw_ring_arc_at(fb, cx, cy, 50, 53, 1024, TEAL, (200 * q) >> 8);
    let am = "AM";
    let aw = wheel::text_width(am, &lock::LABELF_GLYPHS);
    wheel::draw_text_at(fb, am, cx - aw / 2, cy + 16, (240 * q) >> 8, &lock::LABELF_GLYPHS);
    let drift = phone_drift(elapsed_ms);
    for i in 0..10 {
        let (x, y, ch) = phone_digit_pos(i, drift);
        draw_phone_digit(fb, x, y + rise, ch, (200 * q) >> 8);
    }
    st.ph_drift = drift;
    let (rx, _) = title_metrics("Recents");
    draw_title(fb, "Recents", rx, 366 + rise, (100 * q) >> 8, TEAL);
    let names = "Amina   Jim   Ross";
    let nw = wheel::text_width(names, &lock::TEXT_GLYPHS);
    let nx = CX - nw / 2;
    wheel::draw_text_at(fb, names, nx, 400 + rise, (110 * q) >> 8, &lock::TEXT_GLYPHS);
    let wa = wheel::text_width("Amina", &lock::TEXT_GLYPHS);
    let wg = wheel::text_width("   ", &lock::TEXT_GLYPHS);
    let wj = wheel::text_width("Jim", &lock::TEXT_GLYPHS);
    cap_dot(fb, nx + wa + wg / 2, 392 + rise, 2, ICE, (120 * q) >> 8);
    cap_dot(fb, nx + wa + wg + wj + wg / 2, 392 + rise, 2, ICE, (120 * q) >> 8);
}

/// Dial drift: ±6 pseudo-units, slow triangle over 8 s.
fn phone_drift(elapsed_ms: u32) -> i32 {
    let ph = ((elapsed_ms / 16) % 1024) as i32;
    let tri = if ph < 512 { ph } else { 1023 - ph };
    (tri - 256) * 6 / 256
}

fn phone_tick(wfb: &mut WatchFb, st: &mut State, elapsed_ms: u32) {
    let drift = phone_drift(elapsed_ms);
    if drift == st.ph_drift {
        return;
    }
    let old = st.ph_drift;
    st.ph_drift = drift;
    let mut rects = [(0i32, 0i32, -1i32, -1i32); 10];
    {
        let fb = wfb.buf_mut();
        for (i, r_out) in rects.iter_mut().enumerate() {
            let (ox, oy, _) = phone_digit_pos(i, old);
            let (nx, ny, ch) = phone_digit_pos(i, drift);
            let r = (ox.min(nx) - 16, oy.min(ny) - 20, ox.max(nx) + 16, oy.max(ny) + 14);
            fill_rect_black(fb, r.0, r.1, r.2, r.3);
            draw_phone_digit(fb, nx, ny, ch, 200);
            *r_out = r;
        }
    }
    for r in rects {
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    }
}

// ---------------------------------------------------------------------
// Messages (W3 §4.4) — three chord-fitted bubbles, professional tone;
// the typing indicator forever almost-continuing.
// ---------------------------------------------------------------------

const VIOLET: (i32, i32, i32) = (170, 120, 255);
const BUBBLE_GREY: (i32, i32, i32) = (32, 32, 40);
const BUBBLE_ICE: (i32, i32, i32) = (42, 46, 56);

fn draw_messages(fb: &mut [u8], q: i32, rise: i32, elapsed_ms: u32) {
    // AMINA: "slides look great ✨"
    draw_title(fb, "Amina", 96, 126 + rise, (90 * q) >> 8, VIOLET);
    let ts = "09:12";
    let tsw = (wheel::text_width(ts, &lock::TEXT_GLYPHS) * 150) >> 8;
    wheel::draw_text_scaled(fb, ts, CX + 160 - tsw, 126 + rise, (60 * q) >> 8, &lock::TEXT_GLYPHS, 150, false);
    let m1 = "slides look great";
    let w1 = wheel::text_width(m1, &lock::TEXT_GLYPHS);
    fill_round_rect(fb, 88, 134 + rise, 88 + w1 + 58, 178 + rise, 14, BUBBLE_GREY, q);
    wheel::draw_text_at(fb, m1, 104, 164 + rise, (225 * q) >> 8, &lock::TEXT_GLYPHS);
    blit_icon_tint(
        fb,
        wheel::SPARKLES,
        wheel::SPARKLES_PX,
        104 + w1 + 18,
        156 + rise,
        (255, 214, 120),
        (235 * q) >> 8,
    );
    // Habeeb (right, ice fill): "shipping tonight"
    let m2 = "shipping tonight";
    let w2 = wheel::text_width(m2, &lock::TEXT_GLYPHS);
    let r2 = CX + 168;
    fill_round_rect(fb, r2 - w2 - 34, 192 + rise, r2, 236 + rise, 14, BUBBLE_ICE, q);
    wheel::draw_text_at(fb, m2, r2 - w2 - 17, 222 + rise, (235 * q) >> 8, &lock::TEXT_GLYPHS);
    // JIM: "call when you're free"
    draw_title(fb, "Jim", 96, 262 + rise, (90 * q) >> 8, VIOLET);
    let ts3 = "09:15";
    let ts3w = (wheel::text_width(ts3, &lock::TEXT_GLYPHS) * 150) >> 8;
    wheel::draw_text_scaled(fb, ts3, CX + 160 - ts3w, 262 + rise, (60 * q) >> 8, &lock::TEXT_GLYPHS, 150, false);
    let m3 = "call when you're free";
    let w3 = wheel::text_width(m3, &lock::TEXT_GLYPHS);
    fill_round_rect(fb, 88, 270 + rise, 88 + w3 + 34, 314 + rise, 14, BUBBLE_GREY, q);
    wheel::draw_text_at(fb, m3, 104, 300 + rise, (225 * q) >> 8, &lock::TEXT_GLYPHS);
    draw_typing_dots(fb, rise, elapsed_ms, q);
}

/// Typing indicator: three dots, staggered 300 ms pulse.
fn draw_typing_dots(fb: &mut [u8], rise: i32, elapsed_ms: u32, q: i32) {
    for i in 0..3u32 {
        let p = ((elapsed_ms + 1200 - i * 300) % 1200) as i32;
        let a = if p < 600 { 70 + (300 - (p - 300).abs()) * 150 / 300 } else { 70 };
        cap_dot(fb, 104 + i as i32 * 16, 338 + rise, 3, VIOLET, (a * q) >> 8);
    }
}

fn messages_tick(wfb: &mut WatchFb, elapsed_ms: u32) {
    let fb = wfb.buf_mut();
    fill_rect_black(fb, 94, 328, 160, 348);
    draw_typing_dots(fb, 0, elapsed_ms, 256);
    wfb.mark_rect(94, 328, 160, 348);
}

// ---------------------------------------------------------------------
// Shared procedural-drawing helpers (idempotent writes only).
// ---------------------------------------------------------------------

/// Cyclic pseudo-angle distance (q10 space).
fn adist(a: i32, b: i32) -> i32 {
    let d = (a - b).rem_euclid(1024);
    d.min(1024 - d)
}

/// Axis-aligned bbox of an annular sector (angle ± half, radii lo..hi).
fn sector_bbox(cx: i32, cy: i32, r_lo: i32, r_hi: i32, a: i32, half: i32) -> (i32, i32, i32, i32) {
    let mut d = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for aa in [a - half, a, a + half] {
        let (vx, vy) = pseudo_dir(aa);
        for r in [r_lo, r_hi] {
            let x = cx + (r * vx >> 8);
            let y = cy + (r * vy >> 8);
            d = (d.0.min(x), d.1.min(y), d.2.max(x), d.3.max(y));
        }
    }
    (d.0 - 4, d.1 - 4, d.2 + 4, d.3 + 4)
}

/// Filled disc with a soft edge (source-over blend).
fn fill_disc(fb: &mut [u8], cx: i32, cy: i32, r: i32, tint: (i32, i32, i32), q: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r * r {
                continue;
            }
            let v = if d2 <= (r - 1) * (r - 1) { 255 } else { 128 };
            blend_px(fb, cx + dx, cy + dy, tint, (v * q) >> 8);
        }
    }
}

/// Rounded rect fill (corner radius < h/2), source-over.
fn fill_round_rect(
    fb: &mut [u8],
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    rad: i32,
    tint: (i32, i32, i32),
    q: i32,
) {
    for y in y0.max(0)..=y1.min(H - 1) {
        let dyc = if y < y0 + rad {
            y0 + rad - y
        } else if y > y1 - rad {
            y - (y1 - rad)
        } else {
            0
        };
        let ins = if dyc > 0 {
            rad - isqrt(((rad * rad - dyc * dyc).max(0)) as u32) as i32
        } else {
            0
        };
        for x in (x0 + ins).max(0)..=(x1 - ins).min(W - 1) {
            blend_px(fb, x, y, tint, q.min(255));
        }
    }
}

/// Icon alpha sprite through a custom tint (the accent twin of
/// wheel::blit_icon's fixed ice).
fn blit_icon_tint(
    fb: &mut [u8],
    sprite: &[u8],
    px: i32,
    cx: i32,
    cy: i32,
    tint: (i32, i32, i32),
    alpha: i32,
) {
    for iy in 0..px {
        let y = cy - px / 2 + iy;
        if y < 0 || y >= H {
            continue;
        }
        for ix in 0..px {
            let a = sprite[(iy * px + ix) as usize] as i32;
            if a < 8 {
                continue;
            }
            blend_px(fb, cx - px / 2 + ix, y, tint, (a * alpha) >> 8);
        }
    }
}

/// Diamond pseudo-angle: 0..1024 clockwise from 12 o'clock. Monotonic and
/// cheap; mildly non-uniform (±4°) — invisible for sweeps, and thresholds
/// and positions all map through the same function.
fn pseudo_angle(dx: i32, dy: i32) -> i32 {
    let ax = dx.abs();
    let ay = dy.abs();
    let d = (ax + ay).max(1);
    match (dx >= 0, dy >= 0) {
        (true, false) => (ax << 8) / d,
        (true, true) => 256 + ((ay << 8) / d),
        (false, true) => 512 + ((ax << 8) / d),
        (false, false) => 768 + ((ay << 8) / d),
    }
}

/// Unit direction (Q8) for a pseudo-angle — the tip position inverse.
fn pseudo_dir(a: i32) -> (i32, i32) {
    let a = a.rem_euclid(1024);
    let (q, f) = (a >> 8, a & 255);
    let (dx, dy) = match q {
        0 => (f, -(256 - f)),
        1 => (256 - f, f),
        2 => (-f, 256 - f),
        _ => (-(256 - f), -f),
    };
    let len = isqrt((dx * dx + dy * dy) as u32).max(1) as i32;
    ((dx << 8) / len, (dy << 8) / len)
}

/// Direct SET write of tint·v (over the black canvas — idempotent).
#[inline]
fn set_px(fb: &mut [u8], x: i32, y: i32, tint: (i32, i32, i32), v: i32) {
    if x < 0 || x >= W || y < 0 || y >= H {
        return;
    }
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 >= fb.len() {
        return;
    }
    let v = v.clamp(0, 255);
    let r5 = ((tint.0 * v / 255 * 31) / 255) as u16 & 0x1F;
    let g6 = ((tint.1 * v / 255 * 63) / 255) as u16 & 0x3F;
    let b5 = ((tint.2 * v / 255 * 31) / 255) as u16 & 0x1F;
    let px = (r5 << 11) | (g6 << 5) | b5;
    fb[idx] = (px >> 8) as u8;
    fb[idx + 1] = px as u8;
}

/// Per-channel MAX write of tint·v — glow layering that never self-stacks.
#[inline]
fn max_px(fb: &mut [u8], x: i32, y: i32, tint: (i32, i32, i32), v: i32) {
    if x < 0 || x >= W || y < 0 || y >= H {
        return;
    }
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 >= fb.len() {
        return;
    }
    let v = v.clamp(0, 255);
    let r5 = ((tint.0 * v / 255 * 31) / 255) as u16;
    let g6 = ((tint.1 * v / 255 * 63) / 255) as u16;
    let b5 = ((tint.2 * v / 255 * 31) / 255) as u16;
    let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
    let px = ((old >> 11).max(r5) << 11) | (((old >> 5) & 0x3F).max(g6) << 5) | (old & 0x1F).max(b5);
    fb[idx] = (px >> 8) as u8;
    fb[idx + 1] = px as u8;
}

/// Soft radial glow dot (quadratic falloff, MAX-blended).
fn soft_dot(fb: &mut [u8], cx: i32, cy: i32, r: i32, tint: (i32, i32, i32), vpeak: i32) {
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let v = vpeak * (r2 - d2) / r2;
            let v = (v * (r2 - d2)) / r2; // quadratic
            max_px(fb, cx + dx, cy + dy, tint, v);
        }
    }
}

/// Filled ring arc (radial AA, pseudo-angle sweep from 12 o'clock,
/// clockwise), scanning only the annulus row spans. SET writes.
fn draw_ring_arc(
    fb: &mut [u8],
    cx: i32,
    cy: i32,
    r_in: i32,
    r_out: i32,
    sweep: i32,
    tint: (i32, i32, i32),
    vmax: i32,
) {
    draw_ring_arc_at(fb, cx, cy, r_in, r_out, sweep, tint, vmax);
}

fn draw_ring_arc_at(
    fb: &mut [u8],
    cx: i32,
    cy: i32,
    r_in: i32,
    r_out: i32,
    sweep: i32,
    tint: (i32, i32, i32),
    vmax: i32,
) {
    let ri_q4 = r_in << 4;
    let ro_q4 = r_out << 4;
    for dy in -(r_out + 1)..=(r_out + 1) {
        let y = cy + dy;
        if y < 0 || y >= H {
            continue;
        }
        let xo = isqrt((((r_out + 1) * (r_out + 1) - dy * dy).max(0)) as u32) as i32;
        let (s1, s2) = if dy.abs() >= r_in - 1 {
            ((-xo, xo), (1, 0))
        } else {
            let xi = isqrt((((r_in - 1) * (r_in - 1) - dy * dy).max(0)) as u32) as i32;
            ((-xo, -xi), (xi, xo))
        };
        for (sx, ex) in [s1, s2] {
            for dx in sx..=ex {
                arc_px(fb, cx, cy, dx, dy, ri_q4, ro_q4, sweep, tint, vmax);
            }
        }
    }
}

/// Repaint ring-arc pixels intersecting a rect (tip-glow erase repair).
fn arc_in_rect(
    fb: &mut [u8],
    r_in: i32,
    r_out: i32,
    sweep: i32,
    tint: (i32, i32, i32),
    vmax: i32,
    rect: (i32, i32, i32, i32),
) {
    let ri_q4 = r_in << 4;
    let ro_q4 = r_out << 4;
    for y in rect.1.max(0)..=rect.3.min(H - 1) {
        for x in rect.0.max(0)..=rect.2.min(W - 1) {
            arc_px(fb, CX, CY, x - CX, y - CY, ri_q4, ro_q4, sweep, tint, vmax);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn arc_px(
    fb: &mut [u8],
    cx: i32,
    cy: i32,
    dx: i32,
    dy: i32,
    ri_q4: i32,
    ro_q4: i32,
    sweep: i32,
    tint: (i32, i32, i32),
    vmax: i32,
) {
    let d2 = dx * dx + dy * dy;
    let r_q4 = isqrt((d2 as u32) << 8) as i32;
    if r_q4 < ri_q4 - 16 || r_q4 > ro_q4 + 16 {
        return;
    }
    if pseudo_angle(dx, dy) >= sweep {
        return;
    }
    let vin = (r_q4 - (ri_q4 - 16)).clamp(0, 16);
    let vout = ((ro_q4 + 16) - r_q4).clamp(0, 16);
    set_px(fb, cx + dx, cy + dy, tint, vmax * vin.min(vout) / 16);
}

/// Black-fill an annulus centered on the panel (row spans).
fn clear_annulus(fb: &mut [u8], r_in: i32, r_out: i32) {
    for dy in -(r_out + 1)..=(r_out + 1) {
        let y = CY + dy;
        if y < 0 || y >= H {
            continue;
        }
        let xo = isqrt((((r_out + 1) * (r_out + 1) - dy * dy).max(0)) as u32) as i32;
        let (s1, s2) = if dy.abs() >= r_in - 1 {
            ((-xo, xo), (1, 0))
        } else {
            let xi = isqrt((((r_in - 1) * (r_in - 1) - dy * dy).max(0)) as u32) as i32;
            ((-xo, -xi), (xi, xo))
        };
        for (sx, ex) in [s1, s2] {
            if sx > ex {
                continue;
            }
            let a = ((y * W + (CX + sx).max(0)) * 2) as usize;
            let b = ((y * W + (CX + ex).min(W - 1)) * 2 + 2) as usize;
            fb[a..b].fill(0);
        }
    }
}

fn fill_rect_black(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    let (x0, x1) = (x0.max(0), x1.min(W - 1));
    if x1 < x0 {
        return;
    }
    for y in y0.max(0)..=y1.min(H - 1) {
        let a = ((y * W + x0) * 2) as usize;
        let b = ((y * W + x1) * 2 + 2) as usize;
        fb[a..b].fill(0);
    }
}

#[inline]
fn isqrt(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = v;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

// ---------------------------------------------------------------------
// Gallery (W3 §4.2) — full-bleed art, page strip, amber dots, finale
// caption. Physics live in app.rs::gallery_interact; these are the
// composers.
// ---------------------------------------------------------------------

const CAPTION: &str = "SEE YOU SPACE COWBOY...";
const CAP_SCALE_Q8: i32 = 160;
const CAP_BASE_Y: i32 = 396;
const DOTS_Y: i32 = 424;
const AMBER: (i32, i32, i32) = (255, 165, 80);
const ICE: (i32, i32, i32) = (200, 215, 255);

/// One full-bleed strip frame at horizontal offset `s_px` (page i rests
/// at i·466): two flash-page spans per row (black past the rubber ends),
/// page dots, status on top. Full-frame damage — the scroll IS the frame.
pub fn draw_gallery_frame(wfb: &mut WatchFb, s_px: i32, now: &WallTime, batt: Option<u8>) {
    let n = GALLERY_PAGES as i32;
    {
        let fb = wfb.buf_mut();
        let pg = s_px.div_euclid(W);
        let off = s_px.rem_euclid(W);
        for y in 0..H {
            let dst = ((y * W) * 2) as usize;
            let left_w = (W - off) as usize;
            copy_page_row(fb, dst, pg, y, off as usize, left_w, n);
            if off > 0 {
                copy_page_row(fb, dst + left_w * 2, pg + 1, y, 0, off as usize, n);
            }
        }
        draw_dots(fb, (s_px + W / 2).div_euclid(W).clamp(0, n - 1), n);
        wheel::draw_status(fb, now, batt);
    }
    wfb.mark_rect(0, 0, W - 1, H - 1);
}

/// Morph load/unload frame over the art. Opening: the page shows at once
/// under the fading splash logo/title (bands re-blit each frame —
/// source-over must never self-stack). Closing (`art_a` < 256): the whole
/// page fades out beneath the persistent status bar, blitted faded
/// straight from flash (~full-frame cost, but the fade is only a few
/// frames and reads beautifully).
#[allow(clippy::too_many_arguments)]
pub fn draw_gallery_load(
    wfb: &mut WatchFb,
    fx: &mut wheel::WheelFx,
    now: &WallTime,
    batt: Option<u8>,
    page: usize,
    icon_a: i32,
    art_a: i32,
) {
    let n = GALLERY_PAGES as i32;
    if art_a < 250 {
        // Close fade: full-frame faded art + status; chrome fades with it.
        fx.take_seed();
        {
            let fb = wfb.buf_mut();
            fade_page(fb, page, art_a.max(0));
            wheel::draw_status(fb, now, batt);
        }
        wfb.mark_rect(0, 0, W - 1, H - 1);
        return;
    }
    let seed = fx.take_seed();
    let ia = icon_a.clamp(0, 256);
    let ir = wheel::SPLASH_PX / 2 + 2;
    let (iy0, iy1) = (wheel::SPLASH_ICON_Y - ir, wheel::SPLASH_ICON_Y + ir);
    let tb = wheel::SPLASH_TITLE_BASE_Y;
    {
        let fb = wfb.buf_mut();
        if seed {
            for y in 0..H {
                let dst = ((y * W) * 2) as usize;
                copy_page_row(fb, dst, page as i32, y, 0, W as usize, n);
            }
            draw_dots(fb, page as i32, n);
        } else {
            blit_band(fb, page, iy0, iy1, CX - ir, CX + ir);
            blit_band(fb, page, tb - 32, tb + 10, CX - 120, CX + 120);
            blit_band(fb, page, 26, 66, CX - 110, CX + 110);
        }
        if ia > 8 {
            let app = &wheel::WHEEL_APPS[GALLERY];
            wheel::blit_icon(fb, app.icon_x, wheel::ICON_X_PX, CX, wheel::SPLASH_ICON_Y, ia);
            let (tx, _) = title_metrics(app.name);
            draw_title(fb, app.name, tx, tb, (200 * ia) >> 8, accent(GALLERY));
        }
        wheel::draw_status(fb, now, batt);
    }
    if seed {
        wfb.mark_rect(0, 0, W - 1, H - 1);
    } else {
        wfb.mark_rect(CX - ir, iy0, CX + ir, iy1);
        wfb.mark_rect(CX - 120, tb - 32, CX + 120, tb + 10);
        wfb.mark_rect(CX - 110, 26, CX + 110, 66);
    }
}

/// Gallery rest upkeep: minute-rollover status (band re-blit from the art
/// first) and the finale caption fading in 300 ms after the last page
/// settles — quiet, simply the last thing in the gallery.
pub fn gallery_tick(
    wfb: &mut WatchFb,
    now: &WallTime,
    batt: Option<u8>,
    page: usize,
    settle_ms: Option<u32>,
    status_minute: &mut u8,
) {
    if *status_minute != now.minute {
        *status_minute = now.minute;
        {
            let fb = wfb.buf_mut();
            blit_band(fb, page, 26, 66, CX - 110, CX + 110);
            wheel::draw_status(fb, now, batt);
        }
        wfb.mark_rect(CX - 110, 26, CX + 110, 66);
    }
    if page + 1 == GALLERY_PAGES {
        if let Some(ms) = settle_ms {
            if (300..700).contains(&ms) {
                let a = (((ms as i32 - 300) * 256) / 300).clamp(0, 256);
                let cw = (wheel::text_width(CAPTION, &lock::TEXT_GLYPHS) * CAP_SCALE_Q8) >> 8;
                {
                    let fb = wfb.buf_mut();
                    blit_band(fb, page, CAP_BASE_Y - 32, CAP_BASE_Y + 10, CX - cw / 2 - 4, CX + cw / 2 + 4);
                    wheel::draw_text_scaled(
                        fb,
                        CAPTION,
                        CX - cw / 2,
                        CAP_BASE_Y,
                        (170 * a) >> 8,
                        &lock::TEXT_GLYPHS,
                        CAP_SCALE_Q8,
                        false,
                    );
                }
                wfb.mark_rect(CX - cw / 2 - 4, CAP_BASE_Y - 32, CX + cw / 2 + 4, CAP_BASE_Y + 10);
            }
        }
    }
}

/// Copy one row span from a gallery page (black outside the strip).
fn copy_page_row(fb: &mut [u8], dst: usize, pg: i32, y: i32, src_x: usize, w: usize, n: i32) {
    if w == 0 {
        return;
    }
    if pg < 0 || pg >= n {
        fb[dst..dst + w * 2].fill(0);
        return;
    }
    let src = GALLERY_ART[pg as usize];
    let s = ((y as usize) * W as usize + src_x) * 2;
    fb[dst..dst + w * 2].copy_from_slice(&src[s..s + w * 2]);
}

/// Full-frame faded blit of a page (RGB565 channel scale, source read
/// from flash so PSRAM traffic stays write-only).
fn fade_page(fb: &mut [u8], page: usize, a: i32) {
    let src = GALLERY_ART[page.min(GALLERY_PAGES - 1)];
    let n = fb.len().min(src.len());
    for (d, s) in fb[..n].chunks_exact_mut(2).zip(src[..n].chunks_exact(2)) {
        let px = ((s[0] as i32) << 8) | s[1] as i32;
        let r = ((px >> 11) * a) >> 8;
        let g = (((px >> 5) & 0x3F) * a) >> 8;
        let b = ((px & 0x1F) * a) >> 8;
        let o = ((r as u16) << 11) | ((g as u16) << 5) | b as u16;
        d[0] = (o >> 8) as u8;
        d[1] = o as u8;
    }
}

/// Re-blit a rect of the resting page's art (rest-state band restore).
fn blit_band(fb: &mut [u8], page: usize, y0: i32, y1: i32, x0: i32, x1: i32) {
    let (x0, x1) = (x0.max(0), x1.min(W - 1));
    if x1 < x0 {
        return;
    }
    let src = GALLERY_ART[page.min(GALLERY_PAGES - 1)];
    for y in y0.max(0)..=y1.min(H - 1) {
        let o = ((y * W + x0) * 2) as usize;
        let len = ((x1 - x0 + 1) * 2) as usize;
        fb[o..o + len].copy_from_slice(&src[o..o + len]);
    }
}

/// Footer page dots: lit dot amber (soft edge), others ghosted ice.
fn draw_dots(fb: &mut [u8], lit: i32, n: i32) {
    let x_start = CX - (n - 1) * 11;
    for i in 0..n {
        let cx = x_start + i * 22;
        if i == lit {
            disc(fb, cx, DOTS_Y, 5, AMBER, 255);
        } else {
            disc(fb, cx, DOTS_Y, 3, ICE, 90);
        }
    }
}

/// Small filled disc with a one-pixel softened edge, blended source-over.
fn disc(fb: &mut [u8], cx: i32, cy: i32, r: i32, tint: (i32, i32, i32), vmax: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r * r {
                continue;
            }
            let v = if d2 <= (r - 1) * (r - 1) { vmax } else { vmax / 2 };
            blend_px(fb, cx + dx, cy + dy, tint, v);
        }
    }
}

/// Title styling (user-tuned): a little smaller than TEXT (78%), white.
const TITLE_SCALE_Q8: i32 = 200;
const TITLE_WHITE: (i32, i32, i32) = (245, 246, 250);

/// Uppercase + letter-spaced title metrics at TITLE_SCALE: (left_x, width).
fn title_metrics(name: &str) -> (i32, i32) {
    let mut w = 0;
    let mut n = 0;
    for ch in name.chars() {
        if let Some(g) = lock::get_glyph(&lock::TEXT_GLYPHS, ch.to_ascii_uppercase()) {
            w += ((g.advance as i32 * TITLE_SCALE_Q8) >> 8) + TITLE_SPACING;
            n += 1;
        }
    }
    if n > 0 {
        w -= TITLE_SPACING;
    }
    (CX - w / 2, w)
}

fn draw_title(fb: &mut [u8], name: &str, left_x: i32, base_y: i32, alpha: i32, tint: (i32, i32, i32)) {
    let _ = tint; // titles are white by design (user); accents stay on art
    let mut x = left_x;
    for ch in name.chars() {
        if let Some(g) = lock::get_glyph(&lock::TEXT_GLYPHS, ch.to_ascii_uppercase()) {
            if g.width > 0 && g.height > 0 {
                let dst_w = ((g.width as i32 * TITLE_SCALE_Q8) >> 8).max(1);
                let dst_h = ((g.height as i32 * TITLE_SCALE_Q8) >> 8).max(1);
                let gy = base_y - (((g.height as i32 + g.ymin as i32) * TITLE_SCALE_Q8) >> 8);
                draw_glyph_scaled_tint(fb, x, gy, g, alpha, dst_w, dst_h, TITLE_WHITE);
            }
            x += ((g.advance as i32 * TITLE_SCALE_Q8) >> 8) + TITLE_SPACING;
        }
    }
}

/// Bilinear-scaled 4-bit glyph through a custom tint (the scaled twin of
/// draw_glyph_tint; titles render at 78%).
#[allow(clippy::too_many_arguments)]
fn draw_glyph_scaled_tint(
    fb: &mut [u8],
    ox: i32,
    oy: i32,
    g: &Glyph,
    alpha: i32,
    dst_w: i32,
    dst_h: i32,
    tint: (i32, i32, i32),
) {
    let src_w = g.width as i32;
    let src_h = g.height as i32;
    let stride = (g.width as usize + 1) / 2;
    let sample = |x: i32, y: i32| -> i32 {
        let x = x.clamp(0, src_w - 1);
        let y = y.clamp(0, src_h - 1);
        let byte = g.data[y as usize * stride + (x as usize) / 2];
        let a4 = if x % 2 == 0 { byte >> 4 } else { byte & 0x0F };
        (a4 as i32) * 17
    };
    let step_x = (src_w << 8) / dst_w;
    let step_y = (src_h << 8) / dst_h;
    for dy in 0..dst_h {
        let y = oy + dy;
        if y < 0 || y >= H {
            continue;
        }
        let sy_q8 = dy * step_y;
        let (sy, fy) = (sy_q8 >> 8, sy_q8 & 255);
        for dx in 0..dst_w {
            let x = ox + dx;
            if x < 0 || x >= W {
                continue;
            }
            let sx_q8 = dx * step_x;
            let (sx, fx) = (sx_q8 >> 8, sx_q8 & 255);
            let a = (sample(sx, sy) * (256 - fx) * (256 - fy)
                + sample(sx + 1, sy) * fx * (256 - fy)
                + sample(sx, sy + 1) * (256 - fx) * fy
                + sample(sx + 1, sy + 1) * fx * fy)
                >> 16;
            if a < 8 {
                continue;
            }
            blend_px(fb, x, y, tint, (a * alpha) >> 8);
        }
    }
}

/// Tinted source-over write: out = tint·v + dst·(1−v). The accent twin of
/// the wheel's fixed-ice write_tinted.
#[inline]
fn blend_px(fb: &mut [u8], x: i32, y: i32, tint: (i32, i32, i32), v: i32) {
    if x < 0 || x >= W || y < 0 || y >= H {
        return;
    }
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 >= fb.len() {
        return;
    }
    let v = v.clamp(0, 255);
    let tr5 = tint.0 * 31 / 255;
    let tg6 = tint.1 * 63 / 255;
    let tb5 = tint.2 * 31 / 255;
    let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
    let (or5, og6, ob5) = (
        (old >> 11) as i32,
        ((old >> 5) & 0x3F) as i32,
        (old & 0x1F) as i32,
    );
    let r5 = (or5 + (tr5 - or5) * v / 255) as u16 & 0x1F;
    let g6 = (og6 + (tg6 - og6) * v / 255) as u16 & 0x3F;
    let b5 = (ob5 + (tb5 - ob5) * v / 255) as u16 & 0x1F;
    let px = (r5 << 11) | (g6 << 5) | b5;
    fb[idx] = (px >> 8) as u8;
    fb[idx + 1] = px as u8;
}
