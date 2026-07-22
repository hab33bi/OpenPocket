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

const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = W / 2;

/// Wheel row index of the Photos app (the W3.2 proving screen).
pub const PHOTOS: usize = 7;

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
    idx == PHOTOS
}

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
pub fn draw_reveal(
    wfb: &mut WatchFb,
    fx: &mut wheel::WheelFx,
    now: &WallTime,
    idx: usize,
    q_q8: i32,
    elapsed_ms: u32,
) {
    let _ = now;
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
    }
    // Template apps: the flown icon (wheel-side) IS the hero; nothing more.
}

/// Rest-frame signature animation (§1: ONE breathing element, partial
/// flush, tick_ring doctrine — clear the rect, redraw, mark).
pub fn tick(wfb: &mut WatchFb, idx: usize, elapsed_ms: u32) {
    if idx != PHOTOS {
        return; // template apps rest perfectly still (on their splash)
    }
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

/// Uppercase + letter-spaced title metrics: (left_x, width).
fn title_metrics(name: &str) -> (i32, i32) {
    let mut w = 0;
    let mut n = 0;
    for ch in name.chars() {
        if let Some(g) = lock::get_glyph(&lock::TEXT_GLYPHS, ch.to_ascii_uppercase()) {
            w += g.advance as i32 + TITLE_SPACING;
            n += 1;
        }
    }
    if n > 0 {
        w -= TITLE_SPACING;
    }
    (CX - w / 2, w)
}

fn draw_title(fb: &mut [u8], name: &str, left_x: i32, base_y: i32, alpha: i32, tint: (i32, i32, i32)) {
    let mut x = left_x;
    for ch in name.chars() {
        if let Some(g) = lock::get_glyph(&lock::TEXT_GLYPHS, ch.to_ascii_uppercase()) {
            let gy = base_y - (g.height as i32 + g.ymin as i32);
            draw_glyph_tint(fb, x, gy, g, alpha, tint);
            x += g.advance as i32 + TITLE_SPACING;
        }
    }
}

/// 4-bit-alpha glyph through a custom tint (accent titles) — the wheel's
/// draw_glyph is hard-tinted ice; this is its accent twin.
fn draw_glyph_tint(fb: &mut [u8], ox: i32, oy: i32, g: &Glyph, alpha: i32, tint: (i32, i32, i32)) {
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
            let v = (((a4 as i32) * 17 * alpha) >> 8).clamp(0, 255);
            let idx = ((y * W + x) * 2) as usize;
            if idx + 1 >= fb.len() {
                continue;
            }
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
    }
}
