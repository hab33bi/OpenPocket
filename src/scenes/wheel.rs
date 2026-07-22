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
/// Row pitch in list space.
const PITCH: i32 = 68;
/// Padding between an icon's bounding box and the circular boundary — the
/// box is fitted against the chord at its WORST row (top/bottom corner), so
/// icons can never leave the screen.
const EDGE_PAD: i32 = 10;
/// Ice-blue tint shared with the lock scene's text.
const TINT: (i32, i32, i32) = (200, 215, 255);

/// Staggered reveal: per-row start offset and rise time, in 25 ms frames.
const INTRO_STAG_F: i32 = 2;
const INTRO_RISE_F: i32 = 8;
pub const INTRO_FRAMES: u32 = (INTRO_STAG_F as u32 * 9) + INTRO_RISE_F as u32 + 1;

/// One frame of the staggered reveal (`frame` ≥ INTRO_FRAMES = final state).
/// Full-canvas redraw.
pub fn draw(wfb: &mut WatchFb, now: &WallTime, battery: Option<u8>, focused: usize, frame: u32) {
    let fb = wfb.buf_mut();
    fb.fill(0);
    let saber = saber_lut(0);

    for (i, app) in WHEEL_APPS.iter().enumerate() {
        let y_c = CY + (i as i32 - focused as i32) * PITCH;
        if y_c < -PITCH || y_c > H + PITCH {
            continue;
        }
        // Reveal progress for this row (slide in from the left + fade).
        let p = (((frame as i32 - INTRO_STAG_F * i as i32) * 256) / INTRO_RISE_F).clamp(0, 256);
        if p == 0 {
            continue;
        }
        let slide = -((256 - p) * 48) >> 8;

        let (alpha, t) = row_alpha(y_c);
        if alpha == 0 {
            continue;
        }
        let alpha = (alpha * p) >> 8;

        let (sprite, px) = if t > 128 {
            (app.icon_l, ICON_L_PX)
        } else {
            (app.icon_s, ICON_S_PX)
        };
        let icon_cx = icon_center_x(y_c, px) + slide;

        if t > 128 {
            blit_glow(fb, &saber, icon_cx, y_c, ((t - 128) * 2 * p) >> 8);
        }
        blit_icon(fb, sprite, px, icon_cx, y_c, alpha);

        let glyphs: &[Option<Glyph>; 128] = if t > 128 {
            &lock::LABELF_GLYPHS
        } else {
            &lock::TEXT_GLYPHS
        };
        // Label centered on the screen, vertically in line with the icon,
        // never overlapping it.
        let tw = text_width(app.name, glyphs);
        let min_left = icon_cx + px / 2 + 10;
        let x = (CX - tw / 2).max(min_left) + slide / 2;
        draw_text_at(fb, app.name, x, y_c + 11, alpha, glyphs);
    }

    // Status line, top-center: HH:MM, plus battery when present.
    let mut s = [0u8; 12];
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
    let text = core::str::from_utf8(&s[..n]).unwrap_or("");
    let w = text_width(text, &lock::TEXT_GLYPHS);
    draw_text_at(fb, text, CX - w / 2, 52, 200, &lock::TEXT_GLYPHS);

    wfb.mark_rect(0, 0, W - 1, H - 1);
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
    blit_glow(fb, &saber, icon_cx, y_c, 256);
    blit_icon(fb, app.icon_l, px, icon_cx, y_c, 256);
    let s = GLOW_RING_PX / 2;
    wfb.mark_rect(icon_cx - s, y_c - s, icon_cx + s, y_c + s);
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

/// Blit the pre-rendered glow-ring intensity sprite through the saber LUT.
fn blit_glow(fb: &mut [u8], lut: &[(u8, u8); 256], cx: i32, cy: i32, alpha: i32) {
    let px = GLOW_RING_PX;
    for iy in 0..px {
        let y = cy - px / 2 + iy;
        if y < 0 || y >= H {
            continue;
        }
        for ix in 0..px {
            let a = GLOW_RING[(iy * px + ix) as usize] as i32;
            if a == 0 {
                continue;
            }
            let x = cx - px / 2 + ix;
            if x < 0 || x >= W {
                continue;
            }
            let (hi, lo) = lut[((a * alpha) >> 8).clamp(0, 255) as usize];
            let idx = ((y * W + x) * 2) as usize;
            if idx + 1 < fb.len() {
                fb[idx] = hi;
                fb[idx + 1] = lo;
            }
        }
    }
}

/// Blit an icon alpha sprite, ice-blue tinted, scaled by row alpha (0..=256).
fn blit_icon(fb: &mut [u8], sprite: &[u8], px: i32, cx: i32, cy: i32, alpha: i32) {
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

fn draw_text_at(
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

fn text_width(text: &str, glyphs: &[Option<Glyph>; 128]) -> i32 {
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

#[inline]
fn write_tinted(fb: &mut [u8], x: i32, y: i32, v: i32) {
    let r = (TINT.0 * v / 255).clamp(0, 255);
    let g = (TINT.1 * v / 255).clamp(0, 255);
    let b = (TINT.2 * v / 255).clamp(0, 255);
    let r5 = ((r as u16) * 31 / 255) & 0x1F;
    let g6 = ((g as u16) * 63 / 255) & 0x3F;
    let b5 = ((b as u16) * 31 / 255) & 0x1F;
    let px = (r5 << 11) | (g6 << 5) | b5;
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 < fb.len() {
        fb[idx] = (px >> 8) as u8;
        fb[idx + 1] = px as u8;
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
