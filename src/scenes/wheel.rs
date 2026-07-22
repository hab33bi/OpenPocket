//! M6 W1 — the App Wheel (docs/M6-APPWHEEL-PLAN.md).
//!
//! Left-aligned icons hugging the circle's chord (with padding so an icon
//! can never clip the round boundary), labels screen-centered on each row,
//! the focused row large and bright with the saber-glow focus ring (an
//! iteration of the lightsaber flourish's bloom frame) whose gradient
//! animates through azure ↔ violet. Rows enter with a staggered slide-in
//! reveal. Static layout otherwise (scroll physics land in W2).

use crate::display::watch_fb::WatchFb;
use crate::scenes::lock::{self, Glyph};
use crate::time::WallTime;
use esp_hal::time::{Duration, Instant};

include!(concat!(env!("OUT_DIR"), "/wheel_assets.rs"));

const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = W / 2;
const CY: i32 = H / 2;
const R: i32 = W / 2;
/// Row pitch in list space (public: the app's scroll state + hit-testing).
pub const PITCH_PX: i32 = 68;
const PITCH: i32 = PITCH_PX;

/// Number of app rows.
pub fn rows() -> usize {
    WHEEL_APPS.len()
}
/// Padding between an icon's bounding box and the circular boundary — the
/// box is fitted against the chord at its WORST row (top/bottom corner), so
/// icons can never leave the screen.
const EDGE_PAD: i32 = 10;
/// Ice-blue tint shared with the lock scene's text.
const TINT: (i32, i32, i32) = (200, 215, 255);

/// Staggered reveal: per-row start offset and rise time, in 25 ms frames.
/// Rows rise from below with a cubic ease-out (fast lift, feather-soft
/// landing) while fading in.
const INTRO_STAG_F: i32 = 2;
const INTRO_RISE_F: i32 = 10;
/// How far below its resting place a row starts (px).
const INTRO_RISE_PX: i32 = 28;
pub const INTRO_FRAMES: u32 = (INTRO_STAG_F as u32 * 9) + INTRO_RISE_F as u32 + 1;

/// Max content rects tracked per frame for targeted clearing.
const FX_RECTS: usize = 40;

/// Persistent scroll-renderer state: last frame's content rects (targeted
/// clear instead of a 424 KiB full-canvas fill), the entrance-reveal clock
/// (rows rise/fade in WHILE the wheel already scrolls — the intro never
/// blocks input), and the status line's identity (redrawn only on change).
pub struct WheelFx {
    rects: [(i16, i16, i16, i16); FX_RECTS],
    n: usize,
    seeded: bool,
    intro: Option<u32>,
}

impl WheelFx {
    pub const fn new() -> Self {
        Self {
            rects: [(0, 0, 0, 0); FX_RECTS],
            n: 0,
            seeded: false,
            intro: None,
        }
    }
    /// Restart the entrance reveal; the next frame reseeds the full canvas.
    pub fn begin_intro(&mut self) {
        self.seeded = false;
        self.intro = Some(0);
    }
    /// The canvas was painted over by another composer (sheet drag, wake
    /// repaint) — the next wheel frame must reseed from a full clear.
    pub fn invalidate(&mut self) {
        self.seeded = false;
    }
    /// External composer hand-off (unlock morph): take ownership of the
    /// canvas WITHOUT clearing it — the black base + lock ring stay put.
    /// Rects pushed before the next draw_scroll queue foreign content
    /// (the resting lock text) for its targeted erase.
    pub fn seed_silent(&mut self) {
        self.seeded = true;
        self.n = 0;
        self.intro = None;
    }
    pub fn intro_active(&self) -> bool {
        self.intro.is_some()
    }
    /// Consume the reseed flag: true exactly once after invalidate() /
    /// construction — for external full-canvas composers (gallery) that
    /// manage their own damage.
    pub fn take_seed(&mut self) -> bool {
        !core::mem::replace(&mut self.seeded, true)
    }
    pub fn push(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        if self.n < FX_RECTS {
            self.rects[self.n] = (x0 as i16, y0 as i16, x1 as i16, y1 as i16);
            self.n += 1;
        }
    }
}

/// Grow an (x0,y0,x1,y1) union bbox.
fn grow(d: &mut (i32, i32, i32, i32), x0: i32, y0: i32, x1: i32, y1: i32) {
    d.0 = d.0.min(x0);
    d.1 = d.1.min(y0);
    d.2 = d.2.max(x1);
    d.3 = d.3.max(y1);
}

/// Status line baseline / alpha (public: the unlock morph lands the lock
/// digits exactly on this slot — the ONE fixed point of the whole UI).
pub const STATUS_BASE_Y: i32 = 52;
pub const STATUS_ALPHA: i32 = 200;

/// Splash logo geometry (the open/close morph's loading beat): the focused
/// icon flies to screen center at this size, the app title sits below it.
/// The icon/title group is optically centered on the panel.
pub const SPLASH_PX: i32 = 128;
pub const SPLASH_ICON_Y: i32 = 210;
pub const SPLASH_TITLE_BASE_Y: i32 = 322;

/// Format the status line ("HH:MM" + " | 87%" when battery is known).
fn status_str(now: &WallTime, battery: Option<u8>, s: &mut [u8; 12]) -> usize {
    let mut n = 0;
    s[n] = b'0' + now.hour / 10;
    s[n + 1] = b'0' + now.hour % 10;
    s[n + 2] = b':';
    s[n + 3] = b'0' + now.minute / 10;
    s[n + 4] = b'0' + now.minute % 10;
    n += 5;
    if let Some(pct) = battery {
        s[n] = b' ';
        s[n + 1] = b'|';
        s[n + 2] = b' ';
        n += 3;
        if pct >= 100 {
            s[n] = b'1';
            s[n + 1] = b'0';
            s[n + 2] = b'0';
            n += 3;
        } else {
            if pct >= 10 {
                s[n] = b'0' + pct / 10;
                n += 1;
            }
            s[n] = b'0' + pct % 10;
            n += 1;
        }
        s[n] = b'%';
        n += 1;
    }
    n
}

/// Status line, top-center: HH:MM, plus battery when present. Source-over
/// draw — callers over an image must re-blit the band first (repeated
/// self-compositing brightens AA edges).
pub fn draw_status(fb: &mut [u8], now: &WallTime, battery: Option<u8>) {
    let mut s = [0u8; 12];
    let n = status_str(now, battery, &mut s);
    let text = core::str::from_utf8(&s[..n]).unwrap_or("");
    let w = text_width(text, &lock::TEXT_GLYPHS);
    draw_text_at(fb, text, CX - w / 2, STATUS_BASE_Y, STATUS_ALPHA, &lock::TEXT_GLYPHS);
}

/// Where the status line will rest for this time/battery: (left x of the
/// full string, width of its "HH:MM" head, full width). The unlock morph
/// flies the lock digits to exactly the head's slot so the hand-off to
/// draw_status is position-perfect.
pub fn status_metrics(now: &WallTime, battery: Option<u8>) -> (i32, i32, i32) {
    let mut s = [0u8; 12];
    let n = status_str(now, battery, &mut s);
    let text = core::str::from_utf8(&s[..n]).unwrap_or("");
    let w = text_width(text, &lock::TEXT_GLYPHS);
    let head = text_width(&text[..5.min(text.len())], &lock::TEXT_GLYPHS);
    (CX - w / 2, head, w)
}

/// The status line MINUS its "HH:MM" head (the " | 87%" tail), drawn at
/// its exact resting position — faded in over the last stretch of the
/// unlock morph while the digits are still flying in.
pub fn draw_status_tail(fb: &mut [u8], now: &WallTime, battery: Option<u8>, alpha: i32) {
    let mut s = [0u8; 12];
    let n = status_str(now, battery, &mut s);
    if n <= 5 {
        return;
    }
    let text = core::str::from_utf8(&s[..n]).unwrap_or("");
    let w = text_width(text, &lock::TEXT_GLYPHS);
    let head = text_width(&text[..5], &lock::TEXT_GLYPHS);
    draw_text_at(fb, &text[5..], CX - w / 2 + head, STATUS_BASE_Y, alpha, &lock::TEXT_GLYPHS);
}

/// Scroll-tracked frame: rows positioned by the continuous offset `s_q8`
/// (Q8 px; row i rests centered when s = i·PITCH·256). Focus scale/alpha/
/// glow interpolate with the offset. Damage-minimized: clears only last
/// frame's content rects (not the 424 KiB canvas), draws, and marks one
/// union bbox — flush_spans folds it into a single window burst. Glow is
/// drawn in a separate first pass (bottom layer by construction), icons and
/// labels composite over it. `fast` = motion LOD: the glow halo is dropped
/// and size scaling switches to nearest-neighbor — both restored
/// automatically in the slow landing frames. While `fx.intro` runs, rows
/// still rise/fade in as the wheel scrolls underneath them.
///
/// `reveal` = the unlock morph's scrub (0..=256 sheet progress): rows
/// reveal as the (invisible, black-on-black) sheet boundary rises past
/// them — bottom rows first, the focused center row last — tracked 1:1 by
/// the finger. The status line is suppressed while scrubbed: the morphing
/// lock digits ARE the status-line-to-be.
/// `pill` = row index whose label gets the held-press container: a dark
/// grey full-radius pill drawn under the label while a still finger rests
/// on the row (tap-select / tap-open affordance).
pub fn draw_scroll(
    wfb: &mut WatchFb,
    now: &WallTime,
    battery: Option<u8>,
    s_q8: i32,
    fx: &mut WheelFx,
    fast: bool,
    reveal: Option<i32>,
    pill: Option<usize>,
) {
    // Entrance-reveal clock: advances once per rendered frame (idle while
    // the reveal is scrubbed externally by the unlock morph).
    if reveal.is_some() {
        fx.intro = None;
    }
    let intro_f = fx.intro;
    if let Some(f) = intro_f {
        fx.intro = if f + 1 >= INTRO_FRAMES { None } else { Some(f + 1) };
    }
    let s_px = s_q8 >> 8;
    let mut d = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);

    let seed = !fx.seeded;
    if seed {
        // Seed frame (scene entry / unknown canvas): full clear + full mark.
        fx.seeded = true;
        wfb.buf_mut().fill(0);
    } else {
        // Targeted clear: only where content actually was last frame.
        let fb = wfb.buf_mut();
        for k in 0..fx.n {
            let (x0, y0, x1, y1) = fx.rects[k];
            let (x0, y0) = ((x0 as i32).max(0), (y0 as i32).max(0));
            let (x1, y1) = ((x1 as i32).min(W - 1), (y1 as i32).min(H - 1));
            if x1 < x0 || y1 < y0 {
                continue;
            }
            for y in y0..=y1 {
                let a = ((y * W + x0) * 2) as usize;
                let b = ((y * W + x1) * 2 + 2) as usize;
                fb[a..b].fill(0);
            }
            grow(&mut d, x0, y0, x1, y1);
        }
    }
    fx.n = 0;

    let fb = wfb.buf_mut();
    let saber = if fast { None } else { Some(saber_lut(0)) };

    // Per-row layout, incl. the entrance rise/fade riding the live scroll:
    // focus (alpha/t) follows the list position y_c; geometry draws at the
    // risen y_d. A row whose reveal hasn't started is skipped entirely.
    const MAX_ROWS: usize = 16;
    let mut on = [false; MAX_ROWS];
    let mut ly = [0i32; MAX_ROWS];
    let mut la = [0i32; MAX_ROWS];
    let mut lt = [0i32; MAX_ROWS];
    let mut lw = [0i32; MAX_ROWS];
    let mut lc = [0i32; MAX_ROWS];
    let mut lp = [0i32; MAX_ROWS];
    for i in 0..rows().min(MAX_ROWS) {
        let y_c = CY + i as i32 * PITCH - s_px;
        if y_c < -PITCH || y_c > H + PITCH {
            continue;
        }
        let (alpha, t) = row_alpha(y_c);
        if alpha == 0 {
            continue;
        }
        let (mut y_d, mut a, mut p) = (y_c, alpha, 256);
        if let Some(pv) = reveal {
            // Sheet-boundary reveal: a row starts rising when the boundary
            // reaches its bottom edge and lands over ~110 px of further
            // boundary travel — scrub back down and it sinks away again.
            let b_vis = H - ((pv * H) >> 8);
            let pr = (((y_c + PITCH / 2 - b_vis) * 256) / 110).clamp(0, 256);
            if pr == 0 {
                continue;
            }
            let inv = 256 - pr;
            let pe = 256 - ((((inv * inv) >> 8) * inv) >> 8);
            y_d = y_c + ((256 - pe) * INTRO_RISE_PX >> 8);
            a = (alpha * pe) >> 8;
            p = pe;
        } else if let Some(f) = intro_f {
            let p_lin =
                (((f as i32 - INTRO_STAG_F * i as i32) * 256) / INTRO_RISE_F).clamp(0, 256);
            if p_lin == 0 {
                continue;
            }
            let inv = 256 - p_lin;
            let pe = 256 - (((inv * inv) >> 8) * inv >> 8);
            y_d = y_c + ((256 - pe) * INTRO_RISE_PX >> 8);
            a = (alpha * pe) >> 8;
            p = pe;
        }
        // Continuous size interpolation through a NARROW focus band
        // (t 160..224) that resting rows never occupy.
        let wl = ((t - 160) * 4).clamp(0, 256);
        let cx_s = icon_center_x(y_d, ICON_S_PX);
        let cx_l = icon_center_x(y_d, ICON_L_PX);
        on[i] = true;
        ly[i] = y_d;
        la[i] = a;
        lt[i] = t;
        lw[i] = wl;
        lc[i] = cx_s + (((cx_l - cx_s) * wl) >> 8);
        lp[i] = p;
    }

    // Pass 1 — glow halos (bottom layer everywhere; skipped in fast LOD).
    if let Some(sl) = &saber {
        for i in 0..rows().min(MAX_ROWS) {
            if !on[i] || lt[i] <= 128 {
                continue;
            }
            blit_glow(fb, sl, lc[i], ly[i], ((lt[i] - 128) * 2 * lp[i]) >> 8);
            let s = GLOW_RING_PX / 2;
            fx.push(lc[i] - s, ly[i] - s, lc[i] + s, ly[i] + s);
        }
    }

    // Pass 2 — icons + labels, composited OVER whatever glow lies beneath.
    for (i, app) in WHEEL_APPS.iter().enumerate() {
        if i >= MAX_ROWS || !on[i] {
            continue;
        }
        let (y_d, alpha, wl, cx) = (ly[i], la[i], lw[i], lc[i]);
        // TRUE size interpolation: mid-transition, render the large sprite
        // scaled to the exact intermediate size (bilinear at rest speeds,
        // nearest-neighbor in fast LOD). At wl 0/256 the pixel-perfect
        // pre-rendered sizes are used, so crisp AA holds where the eye
        // lingers.
        let px_eff = ICON_S_PX + (((ICON_L_PX - ICON_S_PX) * wl) >> 8);
        if wl == 0 {
            blit_icon(fb, app.icon_s, ICON_S_PX, cx, y_d, alpha);
        } else if wl == 256 {
            blit_icon(fb, app.icon_l, ICON_L_PX, cx, y_d, alpha);
        } else {
            blit_icon_scaled(fb, app.icon_l, ICON_L_PX, px_eff, cx, y_d, alpha, fast);
        }
        if fast || lt[i] <= 128 {
            // No glow rect covers this icon — track it for the next clear.
            let s = px_eff / 2 + 1;
            fx.push(cx - s, y_d - s, cx + s, y_d + s);
        }
        let min_left = cx + px_eff / 2 + 10;
        if pill == Some(i) {
            // Held-press container: full-radius dark grey pill under the
            // label (drawn first; the label composites over it).
            let tw = if wl == 0 {
                text_width(app.name, &lock::TEXT_GLYPHS)
            } else if wl == 256 {
                text_width(app.name, &lock::LABELF_GLYPHS)
            } else {
                (text_width(app.name, &lock::LABELF_GLYPHS) * (196 + ((60 * wl) >> 8))) >> 8
            };
            let x = (CX - tw / 2).max(min_left);
            let (up, dn) = if wl >= 128 { (28, 26) } else { (20, 22) };
            fill_pill(fb, x - 16, y_d - up, x + tw + 16, y_d + dn);
            fx.push(x - 18, y_d - up - 1, x + tw + 18, y_d + dn + 1);
        }
        let (x, twd) = if wl == 0 {
            let gs = &lock::TEXT_GLYPHS;
            let tw = text_width(app.name, gs);
            let x = (CX - tw / 2).max(min_left);
            draw_text_at(fb, app.name, x, y_d + 11, alpha, gs);
            (x, tw)
        } else if wl == 256 {
            let gl = &lock::LABELF_GLYPHS;
            let tw = text_width(app.name, gl);
            let x = (CX - tw / 2).max(min_left);
            draw_text_at(fb, app.name, x, y_d + 11, alpha, gl);
            (x, tw)
        } else {
            // Scale from small-size ratio (~196/256) up to full (256/256).
            let gl = &lock::LABELF_GLYPHS;
            let scale_q8 = 196 + ((60 * wl) >> 8);
            let tw = (text_width(app.name, gl) * scale_q8) >> 8;
            let x = (CX - tw / 2).max(min_left);
            draw_text_scaled(fb, app.name, x, y_d + 11, alpha, gl, scale_q8, fast);
            (x, tw)
        };
        fx.push(x - 1, y_d - 36, x + twd + 1, y_d + 36);
    }

    // Status line: topmost layer, redrawn EVERY frame and tracked in the
    // rect cache — scrolling rows overlapping its band get cleared like
    // any content, so the clock can never be left erased. Suppressed while
    // the unlock morph scrubs (its flying digits own that slot).
    if reveal.is_none() {
        draw_status(fb, now, battery);
        fx.push(CX - 110, 26, CX + 110, 66);
    }

    // Damage: one union bbox (old content + new content). A single rect
    // keeps the DMI at one equal span per row, which flush_spans folds
    // into ONE window burst — the fast partial path.
    for k in 0..fx.n {
        let (x0, y0, x1, y1) = fx.rects[k];
        grow(&mut d, x0 as i32, y0 as i32, x1 as i32, y1 as i32);
    }
    if seed {
        wfb.mark_rect(0, 0, W - 1, H - 1);
    } else if d.0 <= d.2 && d.1 <= d.3 {
        wfb.mark_rect(d.0, d.1, d.2, d.3);
    }
}

/// Per-frame focus-ring tick while the wheel is resting: redraw the glow
/// through a phase-shifted gradient (azure ↔ violet breathing cycle) and
/// re-blit the focused icon on top. Damage = the glow sprite's rect only.
pub fn tick_ring(wfb: &mut WatchFb, elapsed_ms: u32, focused: usize) {
    // Triangle wave over ~4 s.
    let ph = (elapsed_ms / 8) % 512;
    let phase = if ph < 256 { ph } else { 511 - ph } as i32;
    let saber = saber_lut(phase);

    let y_c = CY; // focused row sits at center (static W1)
    let app = &WHEEL_APPS[focused];
    let px = ICON_L_PX;
    let icon_cx = icon_center_x(y_c, px);

    let fb = wfb.buf_mut();
    let s = GLOW_RING_PX / 2;
    // Clear the tick rect first: the icon composites source-over, and
    // re-compositing over its own previous output would brighten AA edges
    // frame over frame. From black, every tick is bit-identical.
    for y in (y_c - s).max(0)..(y_c + s).min(H) {
        let a = ((y * W + (icon_cx - s).max(0)) * 2) as usize;
        let b = ((y * W + (icon_cx + s).min(W)) * 2) as usize;
        fb[a..b].fill(0);
    }
    blit_glow(fb, &saber, icon_cx, y_c, 256);
    blit_icon(fb, app.icon_l, px, icon_cx, y_c, 256);
    wfb.mark_rect(icon_cx - s, y_c - s, icon_cx + s, y_c + s);
}

/// Redraw the status line in place (minute rollover at rest — the wheel's
/// tick_ring and app rest frames don't otherwise touch it). Clears the
/// band first (tick_ring doctrine: from black, every draw is identical).
pub fn tick_status(wfb: &mut WatchFb, now: &WallTime, battery: Option<u8>) {
    let fb = wfb.buf_mut();
    for y in 26..66 {
        let a = ((y * W + (CX - 110)) * 2) as usize;
        let b = ((y * W + (CX + 110)) * 2) as usize;
        fb[a..b].fill(0);
    }
    draw_status(fb, now, battery);
    wfb.mark_rect(CX - 110, 26, CX + 110, 66);
}

/// Wheel-side frame of the open/close morph (W3 §2), driven by two scrub
/// values: `f_q8` = the focused icon's flight (56→128 px, row slot → the
/// centered splash slot) while its glow dissolves, its label fades fast,
/// and every other row fades + slides 12 px away from center; `icon_a` =
/// the splash logo's alpha (fades out during a content app's load
/// crossfade). The splash title and the app's content are drawn AFTER
/// this by apps::draw_splash_title / apps::draw_reveal, sharing the same
/// rect cache. Status stays topmost and never blinks.
#[allow(clippy::too_many_arguments)]
pub fn draw_open_morph(
    wfb: &mut WatchFb,
    now: &WallTime,
    battery: Option<u8>,
    s_q8: i32,
    fx: &mut WheelFx,
    focused: usize,
    f_q8: i32,
    icon_a: i32,
    show_status: bool,
) {
    fx.intro = None;
    let s_px = s_q8 >> 8;
    let mut d = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);

    let seed = !fx.seeded;
    if seed {
        fx.seeded = true;
        wfb.buf_mut().fill(0);
    } else {
        let fb = wfb.buf_mut();
        for k in 0..fx.n {
            let (x0, y0, x1, y1) = fx.rects[k];
            let (x0, y0) = ((x0 as i32).max(0), (y0 as i32).max(0));
            let (x1, y1) = ((x1 as i32).min(W - 1), (y1 as i32).min(H - 1));
            if x1 < x0 || y1 < y0 {
                continue;
            }
            for y in y0..=y1 {
                let a = ((y * W + x0) * 2) as usize;
                let b = ((y * W + x1) * 2 + 2) as usize;
                fb[a..b].fill(0);
            }
            grow(&mut d, x0, y0, x1, y1);
        }
    }
    fx.n = 0;

    let fb = wfb.buf_mut();

    // Non-focused rows: fade out while sliding away from center.
    for (i, app) in WHEEL_APPS.iter().enumerate() {
        if i == focused {
            continue;
        }
        let y_c = CY + i as i32 * PITCH - s_px;
        if y_c < -PITCH || y_c > H + PITCH {
            continue;
        }
        let (alpha, _) = row_alpha(y_c);
        if alpha == 0 {
            continue;
        }
        let away = if y_c >= CY { 12 } else { -12 };
        let y_d = y_c + ((away * f_q8) >> 8);
        let a = (alpha * (256 - f_q8)) >> 8;
        if a < 8 {
            continue;
        }
        let cx_i = icon_center_x(y_d, ICON_S_PX);
        blit_icon(fb, app.icon_s, ICON_S_PX, cx_i, y_d, a);
        let s2 = ICON_S_PX / 2 + 1;
        fx.push(cx_i - s2, y_d - s2, cx_i + s2, y_d + s2);
        let gs = &lock::TEXT_GLYPHS;
        let tw = text_width(app.name, gs);
        let x = (CX - tw / 2).max(cx_i + ICON_S_PX / 2 + 10);
        draw_text_at(fb, app.name, x, y_d + 11, a, gs);
        fx.push(x - 1, y_d - 36, x + tw + 1, y_d + 36);
    }

    // Focused row: glow dissolves early, label fades fast, icon flies.
    let app = &WHEEL_APPS[focused];
    let y_c = CY + focused as i32 * PITCH - s_px;
    let icon_from = icon_center_x(y_c, ICON_L_PX);
    let ga = (256 - f_q8 * 5 / 2).clamp(0, 256);
    if ga > 8 {
        let sl = saber_lut(0);
        blit_glow(fb, &sl, icon_from, y_c, ga);
        let s2 = GLOW_RING_PX / 2;
        fx.push(icon_from - s2, y_c - s2, icon_from + s2, y_c + s2);
    }
    let la = (256 - f_q8 * 2).clamp(0, 256);
    if la > 8 {
        let gl = &lock::LABELF_GLYPHS;
        let tw = text_width(app.name, gl);
        let x = (CX - tw / 2).max(icon_from + ICON_L_PX / 2 + 10);
        draw_text_at(fb, app.name, x, y_c + 11, la, gl);
        fx.push(x - 1, y_c - 36, x + tw + 1, y_c + 36);
    }
    let ia = icon_a.clamp(0, 256);
    if ia > 8 {
        let px_eff = ICON_L_PX + (((SPLASH_PX - ICON_L_PX) * f_q8) >> 8);
        let ix = icon_from + (((CX - icon_from) * f_q8) >> 8);
        let iy = y_c + (((SPLASH_ICON_Y - y_c) * f_q8) >> 8);
        blit_icon_scaled(fb, app.icon_x, ICON_X_PX, px_eff, ix, iy, ia, false);
        let s2 = px_eff / 2 + 1;
        fx.push(ix - s2, iy - s2, ix + s2, iy + s2);
    }

    // Status: topmost, every frame — the one fixed point (suppressed for
    // apps that hide it, e.g. Time: the frame-start targeted clear erased
    // the wheel's copy, so it simply never reappears during the morph).
    if show_status {
        draw_status(fb, now, battery);
        fx.push(CX - 110, 26, CX + 110, 66);
    }

    for k in 0..fx.n {
        let (x0, y0, x1, y1) = fx.rects[k];
        grow(&mut d, x0 as i32, y0 as i32, x1 as i32, y1 as i32);
    }
    if seed {
        wfb.mark_rect(0, 0, W - 1, H - 1);
    } else if d.0 <= d.2 && d.1 <= d.3 {
        wfb.mark_rect(d.0, d.1, d.2, d.3);
    }
}

/// Focus proximity + row alpha with a fade tail past 2 rows from center.
fn row_alpha(y_c: i32) -> (i32, i32) {
    let ady = (y_c - CY).abs();
    let t = (256 - (ady * 256 / (2 * PITCH)).min(256)).max(0);
    let mut alpha = 115 + (141 * t) / 256;
    if ady > 2 * PITCH {
        let fade = (256 - ((ady - 2 * PITCH) * 256) / PITCH).max(0);
        alpha = (alpha * fade) >> 8;
    }
    (alpha, t)
}

/// Icon center x: fitted against the chord at the icon's WORST row (its
/// top/bottom corner), plus EDGE_PAD — the box never clips the circle.
fn icon_center_x(y_c: i32, px: i32) -> i32 {
    let s = px / 2;
    let dyw = ((y_c - CY).abs() + s).min(R - 1);
    let half_w = (isqrt(((R * R - dyw * dyw) as u32) << 8) >> 4) as i32;
    CX - half_w + EDGE_PAD + s
}

/// The electric-azure saber gradient, phase-blended toward violet
/// (phase 0..=256). Same stop structure as the lock flourish.
fn saber_lut(phase: i32) -> [(u8, u8); 256] {
    // (azure, violet) endpoints per stop channel.
    let mix = |a: i32, b: i32| a + ((b - a) * phase) / 256;
    let mut lut = [(0u8, 0u8); 256];
    for (i, e) in lut.iter_mut().enumerate() {
        let v = i as i32;
        let (r, g, b) = if v < 72 {
            let (r1, g1, b1) = (mix(10, 60), mix(40, 20), mix(140, 160));
            (v * r1 / 72, v * g1 / 72, v * b1 / 72)
        } else if v < 160 {
            let t = v - 72;
            let (r0, g0, b0) = (mix(10, 60), mix(40, 20), mix(140, 160));
            let (r1, g1, b1) = (mix(0, 90), mix(110, 70), mix(230, 255));
            (r0 + t * (r1 - r0) / 88, g0 + t * (g1 - g0) / 88, b0 + t * (b1 - b0) / 88)
        } else if v < 216 {
            let t = v - 160;
            let (r0, g0, b0) = (mix(0, 90), mix(110, 70), mix(230, 255));
            let (r1, g1, b1) = (mix(0, 140), mix(170, 130), 255);
            (r0 + t * (r1 - r0) / 56, g0 + t * (g1 - g0) / 56, b0 + t * (b1 - b0) / 56)
        } else {
            let t = v - 216;
            let (r0, g0, b0) = (mix(0, 140), mix(170, 130), 255);
            (r0 + t * (mix(190, 220) - r0) / 39, g0 + t * (255 - g0) / 39, b0 + t * (255 - b0) / 39)
        };
        let r5 = ((r.clamp(0, 255) as u16) * 31 / 255) & 0x1F;
        let g6 = ((g.clamp(0, 255) as u16) * 63 / 255) & 0x3F;
        let b5 = ((b.clamp(0, 255) as u16) * 31 / 255) & 0x1F;
        let px = (r5 << 11) | (g6 << 5) | b5;
        *e = ((px >> 8) as u8, px as u8);
    }
    lut
}

/// The saber gradient frozen at its azure end — the shared accent LUT for
/// app-screen glows (Photos disc, W3+).
pub fn azure_lut() -> [(u8, u8); 256] {
    saber_lut(0)
}

/// Blit the pre-rendered glow-ring intensity sprite through the saber LUT.
fn blit_glow(fb: &mut [u8], lut: &[(u8, u8); 256], cx: i32, cy: i32, alpha: i32) {
    blit_lut_sprite(fb, GLOW_RING, GLOW_RING_PX, lut, cx, cy, alpha);
}

/// Blit any intensity sprite through a color LUT (per-channel MAX blend —
/// overlapping glows merge instead of punching square bounds).
pub fn blit_lut_sprite(
    fb: &mut [u8],
    sprite: &[u8],
    px: i32,
    lut: &[(u8, u8); 256],
    cx: i32,
    cy: i32,
    alpha: i32,
) {
    for iy in 0..px {
        let y = cy - px / 2 + iy;
        if y < 0 || y >= H {
            continue;
        }
        for ix in 0..px {
            let a = sprite[(iy * px + ix) as usize] as i32;
            if a < 6 {
                continue;
            }
            let x = cx - px / 2 + ix;
            if x < 0 || x >= W {
                continue;
            }
            let (hi, lo) = lut[((a * alpha) >> 8).clamp(0, 255) as usize];
            let idx = ((y * W + x) * 2) as usize;
            if idx + 1 < fb.len() {
                // MAX-blend: overlapping halos (two adjacent glowing rows
                // mid-scroll) merge seamlessly instead of the later one
                // punching its square bounds into the earlier.
                let new = ((hi as u16) << 8) | lo as u16;
                let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
                let px = ((new >> 11).max(old >> 11) << 11)
                    | (((new >> 5) & 0x3F).max((old >> 5) & 0x3F) << 5)
                    | (new & 0x1F).max(old & 0x1F);
                fb[idx] = (px >> 8) as u8;
                fb[idx + 1] = px as u8;
            }
        }
    }
}

/// Blit an icon alpha sprite, ice-blue tinted, scaled by row alpha (0..=256).
pub fn blit_icon(fb: &mut [u8], sprite: &[u8], px: i32, cx: i32, cy: i32, alpha: i32) {
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
            let x = cx - px / 2 + ix;
            if x < 0 || x >= W {
                continue;
            }
            let v = (a * alpha) >> 8;
            write_tinted(fb, x, y, v);
        }
    }
}

/// Scaled icon blit (downscale from the large sprite to any intermediate
/// size) — used only mid-transition. `fast` swaps bilinear for
/// nearest-neighbor sampling (~5x fewer ops/px; invisible at flick speed).
pub fn blit_icon_scaled(
    fb: &mut [u8],
    sprite: &[u8],
    src_px: i32,
    dst_px: i32,
    cx: i32,
    cy: i32,
    alpha: i32,
    fast: bool,
) {
    let step_q8 = (src_px << 8) / dst_px;
    let s = |x: i32, y: i32| -> i32 {
        sprite[(y.min(src_px - 1) * src_px + x.min(src_px - 1)) as usize] as i32
    };
    for dy in 0..dst_px {
        let y = cy - dst_px / 2 + dy;
        if y < 0 || y >= H {
            continue;
        }
        let sy_q8 = dy * step_q8;
        let (sy, fy) = (sy_q8 >> 8, sy_q8 & 255);
        for dx in 0..dst_px {
            let x = cx - dst_px / 2 + dx;
            if x < 0 || x >= W {
                continue;
            }
            let sx_q8 = dx * step_q8;
            let (sx, fx) = (sx_q8 >> 8, sx_q8 & 255);
            let a = if fast {
                s((sx_q8 + 128) >> 8, (sy_q8 + 128) >> 8)
            } else {
                (s(sx, sy) * (256 - fx) * (256 - fy)
                    + s(sx + 1, sy) * fx * (256 - fy)
                    + s(sx, sy + 1) * (256 - fx) * fy
                    + s(sx + 1, sy + 1) * fx * fy)
                    >> 16
            };
            if a < 8 {
                continue;
            }
            write_tinted(fb, x, y, (a * alpha) >> 8);
        }
    }
}

/// Per-glyph scaled text from the large atlas (scale_q8 ≤ 256) — used only
/// mid-transition. `fast` = nearest-neighbor sampling (motion LOD).
pub fn draw_text_scaled(
    fb: &mut [u8],
    text: &str,
    left_x: i32,
    base_y: i32,
    alpha: i32,
    glyphs: &[Option<Glyph>; 128],
    scale_q8: i32,
    fast: bool,
) {
    let mut x_q8 = left_x << 8;
    for ch in text.chars() {
        if let Some(g) = lock::get_glyph(glyphs, ch) {
            // Zero-size glyphs (space) carry only an advance — the .max(1)
            // destination clamp on an empty bitmap indexed out of bounds
            // (sample x.min(-1) wrapped through usize; hardware panic via
            // the Gallery caption's spaces).
            if g.width > 0 && g.height > 0 {
                let dst_w = ((g.width as i32 * scale_q8) >> 8).max(1);
                let dst_h = ((g.height as i32 * scale_q8) >> 8).max(1);
                let glyph_y = base_y - (((g.height as i32 + g.ymin as i32) * scale_q8) >> 8);
                draw_glyph_scaled(fb, x_q8 >> 8, glyph_y, g, alpha, dst_w, dst_h, fast);
            }
            x_q8 += (g.advance as i32) * scale_q8;
        }
    }
}

fn draw_glyph_scaled(
    fb: &mut [u8],
    ox: i32,
    oy: i32,
    g: &Glyph,
    alpha: i32,
    dst_w: i32,
    dst_h: i32,
    fast: bool,
) {
    let src_w = g.width as i32;
    let src_h = g.height as i32;
    let stride = (g.width as usize + 1) / 2;
    let sample = |x: i32, y: i32| -> i32 {
        let x = x.min(src_w - 1);
        let y = y.min(src_h - 1);
        let byte = g.data[y as usize * stride + (x as usize) / 2];
        let a4 = if x % 2 == 0 { byte >> 4 } else { byte & 0x0F };
        (a4 as i32) * 17
    };
    let step_x_q8 = (src_w << 8) / dst_w;
    let step_y_q8 = (src_h << 8) / dst_h;
    for dy in 0..dst_h {
        let y = oy + dy;
        if y < 0 || y >= H {
            continue;
        }
        let sy_q8 = dy * step_y_q8;
        let (sy, fy) = (sy_q8 >> 8, sy_q8 & 255);
        for dx in 0..dst_w {
            let x = ox + dx;
            if x < 0 || x >= W {
                continue;
            }
            let sx_q8 = dx * step_x_q8;
            let (sx, fx) = (sx_q8 >> 8, sx_q8 & 255);
            let a = if fast {
                sample((sx_q8 + 128) >> 8, (sy_q8 + 128) >> 8)
            } else {
                (sample(sx, sy) * (256 - fx) * (256 - fy)
                    + sample(sx + 1, sy) * fx * (256 - fy)
                    + sample(sx, sy + 1) * (256 - fx) * fy
                    + sample(sx + 1, sy + 1) * fx * fy)
                    >> 16
            };
            if a < 8 {
                continue;
            }
            write_tinted(fb, x, y, (a * alpha) >> 8);
        }
    }
}

pub fn draw_text_at(
    fb: &mut [u8],
    text: &str,
    left_x: i32,
    base_y: i32,
    alpha: i32,
    glyphs: &[Option<Glyph>; 128],
) {
    let mut x = left_x;
    for ch in text.chars() {
        if let Some(g) = lock::get_glyph(glyphs, ch) {
            let glyph_y = base_y - (g.height as i32 + g.ymin as i32);
            draw_glyph(fb, x, glyph_y, g, alpha);
            x += g.advance as i32;
        }
    }
}

pub fn text_width(text: &str, glyphs: &[Option<Glyph>; 128]) -> i32 {
    let mut w = 0;
    for ch in text.chars() {
        if let Some(g) = lock::get_glyph(glyphs, ch) {
            w += g.advance as i32;
        }
    }
    w
}

/// 4-bit-alpha atlas glyph, ice-blue tinted, scaled by `alpha` (0..=256).
fn draw_glyph(fb: &mut [u8], ox: i32, oy: i32, g: &Glyph, alpha: i32) {
    let w = g.width as i32;
    let h = g.height as i32;
    let stride = (g.width as usize + 1) / 2;
    for gy in 0..h {
        let y = oy + gy;
        if y < 0 || y >= H {
            continue;
        }
        let row = &g.data[gy as usize * stride..(gy as usize + 1) * stride];
        for gx in 0..w {
            let x = ox + gx;
            if x < 0 || x >= W {
                continue;
            }
            let byte = row[gx as usize / 2];
            let a4 = if gx % 2 == 0 { byte >> 4 } else { byte & 0x0F };
            if a4 == 0 {
                continue;
            }
            let v = ((a4 as i32) * 17 * alpha) >> 8;
            write_tinted(fb, x, y, v);
        }
    }
}

/// Tinted SOURCE-OVER write: v is coverage×row-alpha; out = tint·v +
/// dst·(1−v). On the black canvas this renders identically to before, but
/// over the glow the icon/text now genuinely COVERS it — a dim or scaled
/// icon no longer loses per-channel to bright ring pixels (the old
/// MAX-blend's split-second "icon sinks into the ring" on hard flicks).
#[inline]
fn write_tinted(fb: &mut [u8], x: i32, y: i32, v: i32) {
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 >= fb.len() {
        return;
    }
    let v = v.clamp(0, 255);
    // Tint pre-quantized to 565 channel depth.
    const TR5: i32 = TINT.0 * 31 / 255;
    const TG6: i32 = TINT.1 * 63 / 255;
    const TB5: i32 = TINT.2 * 31 / 255;
    let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
    let (or5, og6, ob5) = (
        (old >> 11) as i32,
        ((old >> 5) & 0x3F) as i32,
        (old & 0x1F) as i32,
    );
    let r5 = (or5 + (TR5 - or5) * v / 255) as u16 & 0x1F;
    let g6 = (og6 + (TG6 - og6) * v / 255) as u16 & 0x3F;
    let b5 = (ob5 + (TB5 - ob5) * v / 255) as u16 & 0x1F;
    let px = (r5 << 11) | (g6 << 5) | b5;
    fb[idx] = (px >> 8) as u8;
    fb[idx + 1] = px as u8;
}

/// Full-radius pill: the held-label container. Dark charcoal at ~78%
/// opacity, blended source-over so the glow beneath ghosts through — the
/// premium AMOLED look (a flat SET fill read as a sticker).
fn fill_pill(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    const T: (i32, i32, i32) = (34, 36, 42);
    const A: i32 = 200; // ~78% opacity
    const TR5: i32 = T.0 * 31 / 255;
    const TG6: i32 = T.1 * 63 / 255;
    const TB5: i32 = T.2 * 31 / 255;
    let r = (y1 - y0) / 2;
    let cy_ = (y0 + y1) / 2;
    for y in y0.max(0)..=y1.min(H - 1) {
        let dy = y - cy_;
        let ins = r - isqrt(((r * r - dy * dy).max(0)) as u32) as i32;
        let (a, b) = ((x0 + ins).max(0), (x1 - ins).min(W - 1));
        for x in a..=b {
            let idx = ((y * W + x) * 2) as usize;
            if idx + 1 >= fb.len() {
                continue;
            }
            let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
            let (or5, og6, ob5) = (
                (old >> 11) as i32,
                ((old >> 5) & 0x3F) as i32,
                (old & 0x1F) as i32,
            );
            let r5 = (or5 + (TR5 - or5) * A / 255) as u16 & 0x1F;
            let g6 = (og6 + (TG6 - og6) * A / 255) as u16 & 0x3F;
            let b5 = (ob5 + (TB5 - ob5) * A / 255) as u16 & 0x1F;
            let px = (r5 << 11) | (g6 << 5) | b5;
            fb[idx] = (px >> 8) as u8;
            fb[idx + 1] = px as u8;
        }
    }
}

/// Pace helper for the intro (blocking, 25 ms frames like the settle).
pub fn pace(frame_start: Instant) {
    while frame_start.elapsed() < Duration::from_micros(25_000) {
        core::hint::spin_loop();
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
