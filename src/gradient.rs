//! Simple Blue Gradient - premium diagonal gentle sweeping animation (dark blue to royal blue).
//!
//! Beautiful implementation:
//! - Diagonal sweep from top-leftish to bottom-right, gentle flow.
//! - Subtle perpendicular wave using LUT for organic premium feel.
//! - Smooth Q14 lerp for no banding.
//! - Centered for round display, soft vignette.
//! - Black/dark edges for contrast.
//!
//! Strategy (following main branch learnings + cloud/light-rays ports, web research):
//! - Live only (no prebake, minimal flash).
//! - Full res for max smoothness (gradient math trivial; ~negligible compute vs flush).
//!   Or low-res + bilinear for consistency (div=2).
//! - Q14 + lut_sin_cos_q14 (best internal "lib") for wave.
//! - Direct sync flush.
//! - Extremely efficient: few ops + 1 LUT/pixel.
//! - Target 30+ FPS (easy; flush limited ~25-40ms).
//! - Production: dither optional (research: dithereens), but simple noise here.
//! - No new deps for minimal cost (Animato/dithereens researched but deferred; internal sufficient for simple).
//!
//! Research notes:
//! - Best internal: Q14 LUT + direct pipeline (from raidal, proven 30FPS+).
//! - External: embedded-graphics (optional primitives), Animato (tween for future), dithereens (dither).
//!   For this, custom pixel is lowest cost/highest FPS.
//! - Diagonal gentle: phase along (x+y), slow speed, low amp wave.
//! - To 25+FPS: full res ok; optimize DMA if needed (8k chunks already good).
//! - Premium: high prec lerp, LUT wave, round center, calm params.
//!
//! Usage: similar to Cloud/LightRays.

use alloc::vec::Vec;

use crate::raidal::{lut_sin_cos_q14, q14_rgb_to_rgb565};

// Use low res for consistency with ports (can switch to full for even smoother).
pub const GRAD_DIV: u16 = 2;

pub const LOW_W: u16 = 466 / GRAD_DIV;
pub const LOW_H: u16 = 466 / GRAD_DIV;
pub const LOW_PIXELS: usize = (LOW_W as usize) * (LOW_H as usize);

/// Dark blue (deep navy) to Royal blue. Tuned for good 565 contrast.
const DARK_BLUE: u16 = 0x000B;  // deep dark blue
const ROYAL_BLUE: u16 = 0x45DF; // rich royal blue

#[derive(Clone, Copy, Debug)]
pub struct GradientConfig {
    pub time_scale: f32,
    /// Slow sweep speed for gentle diagonal flow (higher = faster sweep).
    pub sweep_speed: f32,
    /// Amplitude of subtle perpendicular wave (0.05-0.15 for gentle premium).
    pub wave_amp: f32,
}

impl Default for GradientConfig {
    fn default() -> Self {
        Self {
            time_scale: 1.0,
            sweep_speed: 0.2,  // very slow elegant sweep
            wave_amp: 0.08,    // subtle organic wave
        }
    }
}

pub struct BlueGradient {
    config: GradientConfig,
    low_w: u16,
    low_h: u16,
    out_w: u16,
    out_h: u16,
    ux: Vec<UpSample1D>,
    uy: Vec<UpSample1D>,
    t_q: i32,
}

#[derive(Clone, Copy)]
struct UpSample1D {
    i0: u16,
    i1: u16,
    w1: u8,
}

impl BlueGradient {
    pub fn new(config: GradientConfig, width: u16, height: u16) -> Self {
        let low_w = LOW_W;
        let low_h = LOW_H;

        let ux = build_upscale_table(width, low_w, GRAD_DIV);
        let uy = build_upscale_table(height, low_h, GRAD_DIV);

        Self {
            config,
            low_w,
            low_h,
            out_w: width,
            out_h: height,
            ux,
            uy,
            t_q: 0,
        }
    }

    pub fn update_time(&mut self, time_ms: u32) {
        let t_f = (time_ms as f32 / 1000.0) * self.config.time_scale;
        self.t_q = (t_f * 16384.0) as i32;
    }

    /// Full low-res eval for the diagonal gradient.
    /// Diagonal sweep: phase along (x + y), gentle wave.
    pub fn eval_rows(&self, low_buf: &mut [u16], row_start: usize, row_end: usize) {
        let lw = self.low_w as usize;
        let lh = self.low_h as usize;
        let rs = row_start.min(lh);
        let re = row_end.min(lh);

        let t = self.t_q;
        let sweep = (self.config.sweep_speed * 16384.0) as i32;
        let amp = (self.config.wave_amp * 16384.0) as i32;

        let cx = (lw / 2) as i32;
        let cy = (lh / 2) as i32;

        for ly in rs..re {
            let y_off = (ly as i32 - cy) * 16384 / (lh as i32);

            for lx in 0..lw {
                let x_off = (lx as i32 - cx) * 16384 / (lw as i32);

                // Diagonal coord (x + y normalized) - full coverage for larger sweep
                let diag = x_off + y_off;  // larger range for 3x bigger effect

                // Gentle sweep phase (slow diagonal advance)
                let phase = t.wrapping_mul(sweep / 800);  // adjusted for larger visible sweep

                // Base blend factor with sweep - covers more of the diagonal
                let mut factor = (diag.wrapping_add(phase) >> 1) & 0x7FFF;
                factor = factor.clamp(0, 16384);

                // Subtle perpendicular wave for premium flowing feel (larger for 3x effect)
                let perp = (x_off - y_off) / 2;
                let wave = lut_sin_cos_q14(perp.wrapping_add(phase / 2));
                let wave_contrib = (wave * amp * 2) >> 14;  // boosted
                factor = (factor + wave_contrib).clamp(0, 16384);

                // Lerp colors (Q14 style for smoothness)
                // Simple lerp between fixed 565 (promote to Q)
                let dark = (DARK_BLUE as i32) << 9;  // rough scale
                let royal = (ROYAL_BLUE as i32) << 9;
                let color = dark + (((royal - dark) * factor) >> 14);

                // Soft vignette for round premium
                let dist = isqrt_approx((x_off as i64 * x_off as i64 + y_off as i64 * y_off as i64) as i32 >> 8);
                let vig = (16384 - (dist * 4000 / 16384)).max(8000);  // soft
                let color = (color * vig) >> 14;

                // To 565 (simplified from q14 helper)
                let c = (color >> 9) as u16;  // back
                let r = ((c >> 11) & 0x1F) as u16;
                let g = ((c >> 5) & 0x3F) as u16;
                let b = (c & 0x1F) as u16;
                // slight desat for dark to royal
                let px = (r << 11) | (g << 5) | b;

                low_buf[ly * lw + lx] = px;
            }
        }
    }

    pub fn eval_pass(&self, low_buf: &mut [u16]) {
        self.eval_rows(low_buf, 0, self.low_h as usize);
    }

    pub fn upscale_rows(&self, low: &[u16], out: &mut [u8], row_start: usize, row_end: usize) {
        let out_w = self.out_w as usize;
        let rs = row_start.min(self.out_h as usize);
        let re = row_end.min(self.out_h as usize);
        let lw = self.low_w as usize;

        for oy in rs..re {
            let vy = &self.uy[oy];
            let row0 = vy.i0 as usize * lw;
            let row1 = vy.i1 as usize * lw;
            let wy1 = vy.w1 as u16;
            let wy0 = 255 - wy1;

            for ox in 0..out_w {
                let hx = &self.ux[ox];
                let wx1 = hx.w1 as u16;
                let wx0 = 255 - wx1;

                let w00 = ((wx0 * wy0) / 255) as u16;
                let w10 = ((wx1 * wy0) / 255) as u16;
                let w01 = ((wx0 * wy1) / 255) as u16;
                let w11 = ((wx1 * wy1) / 255) as u16;

                let c00 = low[row0 + hx.i0 as usize];
                let c10 = low[row0 + hx.i1 as usize];
                let c01 = low[row1 + hx.i0 as usize];
                let c11 = low[row1 + hx.i1 as usize];

                let px = bilinear_rgb565(c00, c10, c01, c11, [w00 as u8, w10 as u8, w01 as u8, w11 as u8]);
                let idx = (oy * out_w + ox) * 2;
                out[idx] = (px >> 8) as u8;
                out[idx + 1] = px as u8;
            }
        }
    }
}

fn build_upscale_table(out_size: u16, low_size: u16, div: u16) -> Vec<UpSample1D> {
    let scale = div as f32;
    let max_i = (low_size - 1) as f32;
    let mut table = Vec::with_capacity(out_size as usize);
    for o in 0..out_size {
        let s = ((o as f32 + 0.5) / scale) - 0.5;
        let s = s.clamp(0.0, max_i);
        let i0 = libm::floorf(s) as u16;
        let i1 = (i0 + 1).min(low_size - 1);
        let frac = s - i0 as f32;
        table.push(UpSample1D { i0, i1, w1: (frac * 255.0) as u8 });
    }
    table
}

#[inline]
fn bilinear_rgb565(c00: u16, c10: u16, c01: u16, c11: u16, w: [u8; 4]) -> u16 {
    let ch = |shift: u16, mask: u16| -> u32 {
        let f = |c: u16| ((c >> shift) & mask) as u32;
        let sum = f(c00) * w[0] as u32 + f(c10) * w[1] as u32 + f(c01) * w[2] as u32 + f(c11) * w[3] as u32;
        (sum + 127) / 255
    };
    let r = ch(11, 31) as u16;
    let g = ch(5, 63) as u16;
    let b = ch(0, 31) as u16;
    (r << 11) | (g << 5) | b
}

// Approx sqrt for vignette
#[inline]
fn isqrt_approx(x: i32) -> i32 {
    if x <= 0 { return 0; }
    let mut y = x;
    let mut z = (x + 1) / 2;
    while z < y {
        y = z;
        z = (z + x / z) / 2;
    }
    y
}