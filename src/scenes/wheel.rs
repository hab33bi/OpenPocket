//! M6 W1 — the App Wheel, static frame (docs/M6-APPWHEEL-PLAN.md).
//!
//! Right-aligned vertical carousel hugging the round bezel: each row's right
//! edge follows the circle's chord at that row's y, the focused row renders
//! large and bright with the saber-glow focus ring (an iteration of the
//! lightsaber flourish's bloom frame), neighbors dim and indent along the
//! curve. W1 renders a static frame (scroll physics land in W2).

use crate::display::watch_fb::WatchFb;
use crate::scenes::lock::{self, Glyph};
use crate::time::WallTime;

include!(concat!(env!("OUT_DIR"), "/wheel_assets.rs"));

const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = W / 2;
const CY: i32 = H / 2;
const R: i32 = W / 2;
/// Row pitch in list space.
const PITCH: i32 = 68;
/// Gap between a row's icon and the circle's chord at that row's y.
const EDGE_MARGIN: i32 = 14;
/// Gap between a label's right edge and its icon's left edge.
const LABEL_GAP: i32 = 12;
/// Ice-blue tint shared with the lock scene's text.
const TINT: (i32, i32, i32) = (200, 215, 255);

/// Draw the full wheel: rows around `focused`, status line top-center.
/// Full-canvas redraw (static scene; W2 makes it incremental).
pub fn draw(wfb: &mut WatchFb, now: &WallTime, battery: Option<u8>, focused: usize) {
    let fb = wfb.buf_mut();
    fb.fill(0);
    let saber = saber_lut();

    for (i, app) in WHEEL_APPS.iter().enumerate() {
        let y_c = CY + (i as i32 - focused as i32) * PITCH;
        if y_c < -PITCH || y_c > H + PITCH {
            continue;
        }
        // Focus proximity 0..=256 and row alpha 0.45..=1.0.
        let t = (256 - ((y_c - CY).abs() * 256 / (2 * PITCH)).min(256)).max(0);
        let alpha = 115 + (141 * t) / 256;

        // Chord-following right edge at this row's y.
        let dy = (y_c - CY).abs().min(R - 1);
        let half_w = isqrt(((R * R - dy * dy) as u32) << 8) as i32 >> 4;
        let x_r = CX + half_w - EDGE_MARGIN;

        let (sprite, px) = if t > 128 {
            (app.icon_l, ICON_L_PX)
        } else {
            (app.icon_s, ICON_S_PX)
        };
        let icon_cx = x_r - px / 2;

        if t > 128 {
            // Saber-glow focus ring beneath the focused icon.
            blit_glow(fb, &saber, icon_cx, y_c, (t - 128) * 2);
        }
        blit_icon(fb, sprite, px, icon_cx, y_c, alpha);

        let glyphs: &[Option<Glyph>; 128] = if t > 128 {
            &lock::LABELF_GLYPHS
        } else {
            &lock::TEXT_GLYPHS
        };
        // Baseline ≈ optical center of the row.
        draw_text_right(fb, app.name, x_r - px - LABEL_GAP, y_c + 11, alpha, glyphs);
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
    draw_text_right(fb, text, CX + w / 2, 52, 200, &lock::TEXT_GLYPHS);

    wfb.mark_rect(0, 0, W - 1, H - 1);
}

/// The electric-azure saber gradient (same stops as the lock flourish) —
/// intensity 0..=255 → RGB565-BE.
fn saber_lut() -> [(u8, u8); 256] {
    let mut lut = [(0u8, 0u8); 256];
    for (i, e) in lut.iter_mut().enumerate() {
        let v = i as i32;
        let (r, g, b) = if v < 72 {
            (v * 10 / 72, v * 40 / 72, v * 140 / 72)
        } else if v < 160 {
            let t = v - 72;
            (10 - t * 10 / 88, 40 + t * 70 / 88, 140 + t * 90 / 88)
        } else if v < 216 {
            let t = v - 160;
            (0, 110 + t * 60 / 56, 230 + t * 25 / 56)
        } else {
            let t = v - 216;
            (t * 190 / 39, 170 + t * 85 / 39, 255)
        };
        let r5 = ((r as u16) * 31 / 255) & 0x1F;
        let g6 = ((g as u16) * 63 / 255) & 0x3F;
        let b5 = ((b as u16) * 31 / 255) & 0x1F;
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

/// Right-aligned text: the string ends at `right_x`.
fn draw_text_right(
    fb: &mut [u8],
    text: &str,
    right_x: i32,
    base_y: i32,
    alpha: i32,
    glyphs: &[Option<Glyph>; 128],
) {
    let mut x = right_x - text_width(text, glyphs);
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
