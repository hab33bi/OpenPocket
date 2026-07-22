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
pub const ACTIVITY: usize = 4;
pub const SETTINGS: usize = 5;
pub const PHOTOS: usize = 7;

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
        }
    }
}

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

/// Title baseline (§1) and content geometry.
const TITLE_BASE_Y: i32 = 92;
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
        7 => (90, 160, 255),  // Photos — azure
        8 => (255, 165, 80),  // Weather — amber
        _ => (90, 160, 255),  // Timer — azure
    }
}

/// Whether the app has real content behind its splash. Content apps
/// crossfade splash → content at the end of the open morph; template apps
/// REST on the splash (big centered logo + title below — the honest
/// placeholder until their W3.3–3.5 passes).
pub fn has_content(idx: usize) -> bool {
    matches!(idx, TIME | GALLERY | ACTIVITY | SETTINGS | PHOTOS)
}

/// Whether the app shows the shared status clock. The Time app hides it
/// (its big digits ARE the time — user decision) and centers its group.
pub fn shows_status(idx: usize) -> bool {
    idx != TIME
}

/// Time app layout: big digits + date optically centered as a group.
const TIME_BASE_Y: i32 = 244;
const DATE_BASE_Y: i32 = 298;

/// Photos hero (glow disc) center.
const PHOTOS_HERO: (i32, i32) = (CX, 230);

/// Splash title: spaced caps in the app accent, under the centered logo.
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
    let rise = ((256 - q_q8) * 20) >> 8;

    // Title: spaced caps, accent at 60%.
    let name = wheel::WHEEL_APPS[idx].name;
    let (tx, tw) = title_metrics(name);
    {
        let fb = wfb.buf_mut();
        draw_title(fb, name, tx, TITLE_BASE_Y + rise, (153 * q_q8) >> 8, accent(idx));
    }
    fx.push(tx - 2, TITLE_BASE_Y + rise - 32, tx + tw + 2, TITLE_BASE_Y + rise + 10);
    wfb.mark_rect(tx - 2, TITLE_BASE_Y + rise - 32, tx + tw + 2, TITLE_BASE_Y + rise + 10);

    if idx == PHOTOS {
        // §4.8 — the elegant empty state: breathing azure inner-glow disc,
        // aperture at 50%, honest caption.
        let (hx, hy0) = PHOTOS_HERO;
        let hy = hy0 + rise;
        let lut = wheel::azure_lut();
        let r = wheel::PHOTOS_DISC_PX / 2;
        {
            let fb = wfb.buf_mut();
            wheel::blit_lut_sprite(
                fb,
                wheel::PHOTOS_DISC,
                wheel::PHOTOS_DISC_PX,
                &lut,
                hx,
                hy,
                (breath(elapsed_ms) * q_q8) >> 8,
            );
            wheel::blit_icon(fb, wheel::APERTURE, wheel::APERTURE_PX, hx, hy, (128 * q_q8) >> 8);
        }
        fx.push(hx - r, hy - r, hx + r, hy + r);
        wfb.mark_rect(hx - r, hy - r, hx + r, hy + r);

        let cap = "nothing captured yet";
        let cw = wheel::text_width(cap, &lock::TEXT_GLYPHS);
        let cy = 356 + rise;
        {
            let fb = wfb.buf_mut();
            wheel::draw_text_at(fb, cap, CX - cw / 2, cy, (102 * q_q8) >> 8, &lock::TEXT_GLYPHS);
        }
        fx.push(CX - cw / 2 - 2, cy - 32, CX + cw / 2 + 2, cy + 10);
        wfb.mark_rect(CX - cw / 2 - 2, cy - 32, CX + cw / 2 + 2, cy + 10);
    } else if idx == TIME {
        // §4.1 — big centered time + date; the seconds arc is seeded by
        // the first rest tick (its rim damage wants one clean full flush).
        let (tbuf, dbuf, dlen) = time_strings(now);
        let t_str = core::str::from_utf8(&tbuf).unwrap_or("00:00");
        let d_str = core::str::from_utf8(&dbuf[..dlen]).unwrap_or("");
        let tw = wheel::text_width(t_str, &lock::TIME_GLYPHS);
        let dw = wheel::text_width(d_str, &lock::TEXT_GLYPHS);
        {
            let fb = wfb.buf_mut();
            wheel::draw_text_at(fb, t_str, CX - tw / 2, TIME_BASE_Y + rise, q_q8, &lock::TIME_GLYPHS);
            wheel::draw_text_at(
                fb,
                d_str,
                CX - dw / 2,
                DATE_BASE_Y + rise,
                (150 * q_q8) >> 8,
                &lock::TEXT_GLYPHS,
            );
        }
        let r = (CX - tw / 2 - 2, 150 + rise, CX + tw / 2 + 2, 310 + rise);
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
            let steps = "6 412";
            let sw2 = wheel::text_width(steps, &lock::LABELF_GLYPHS);
            wheel::draw_text_at(fb, steps, CX - sw2 / 2, 248 + rise, q_q8, &lock::LABELF_GLYPHS);
            let cap = "steps";
            let cw2 = wheel::text_width(cap, &lock::TEXT_GLYPHS);
            wheel::draw_text_at(fb, cap, CX - cw2 / 2, 284 + rise, (102 * q_q8) >> 8, &lock::TEXT_GLYPHS);
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
    }
    // Template apps: the flown icon (wheel-side) IS the hero; nothing more.
}

/// Rest-frame signature animation (§1: ONE breathing element per app,
/// partial flush, tick_ring doctrine — clear the rect, redraw, mark).
pub fn tick(wfb: &mut WatchFb, idx: usize, now: &WallTime, elapsed_ms: u32, st: &mut State) {
    match idx {
        PHOTOS => photos_tick(wfb, elapsed_ms),
        TIME => time_tick(wfb, now, st),
        ACTIVITY => activity_tick(wfb, elapsed_ms),
        _ => {} // template apps (and Settings) rest perfectly still
    }
}

fn photos_tick(wfb: &mut WatchFb, elapsed_ms: u32) {
    let (hx, hy) = PHOTOS_HERO;
    let r = wheel::PHOTOS_DISC_PX / 2;
    let lut = wheel::azure_lut();
    let fb = wfb.buf_mut();
    for y in (hy - r).max(0)..(hy + r).min(H) {
        let a = ((y * W + (hx - r).max(0)) * 2) as usize;
        let b = ((y * W + (hx + r).min(W)) * 2) as usize;
        fb[a..b].fill(0);
    }
    wheel::blit_lut_sprite(
        fb,
        wheel::PHOTOS_DISC,
        wheel::PHOTOS_DISC_PX,
        &lut,
        hx,
        hy,
        breath(elapsed_ms),
    );
    wheel::blit_icon(fb, wheel::APERTURE, wheel::APERTURE_PX, hx, hy, 128);
    wfb.mark_rect(hx - r, hy - r, hx + r, hy + r);
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
        // Fresh open: seed the whole arc in one clean frame.
        st.t_sec = now.second;
        st.t_anchor = Some(Instant::now());
        let a = ((now.second as i32) * 1024 / 60).max(4);
        let fb = wfb.buf_mut();
        // The Time app owns the whole face: the status clock is hidden
        // here (the big digits ARE the time).
        fill_rect_black(fb, CX - 110, 26, CX + 110, 66);
        clear_annulus(fb, ARC_R_IN - 2, ARC_R_OUT + 2);
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
// Shared procedural-drawing helpers (idempotent writes only).
// ---------------------------------------------------------------------

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
