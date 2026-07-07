//! Simple Blue Gradient - premium diagonal gentle sweeping animation (dark blue to royal blue).
//!
//! Beautiful implementation:
//! - Diagonal sweep from top-leftish to bottom-right, gentle flow.
//! - Subtle perpendicular wave using LUT for organic premium feel.
//! - Smooth Q14 lerp for no banding.
//! - Centered for round display, soft vignette.
//! - Black/dark edges for contrast.
//!
//! Simple Blue Gradient - premium diagonal gentle sweeping animation (dark blue to royal blue).
//!
//! Current implementation (addresses "bands too visible, need to almost blur, 15-20fps smooth slow"):
//! - Low-res (div=2) eval + bilinear upscale for natural soft blending ("almost blur into each other").
//! - Cheap per-pixel dither in low eval to further hide RGB565 quantization bands.
//! - Monotonic Q14 diagonal + slow rot + dual subtle waves.
//! - Double PSRAM framebuffers + correct ping-pong (render back, flush front).
//! - Frame rate capped ~15-20 FPS with slow phase advance for smooth slow motion.
//! - Darker low-sat navy with dark purple hint, vignette.
//!
//! All math uses Q14 + lut_sin_cos_q14. Reuses project's bilinear + upscale tables.
//! Live only. Call render_to_fb(fb, t_q) each frame from the main loop.

use alloc::vec;
use alloc::vec::Vec;

use crate::raidal::{lut_sin_cos_q14, q14_rgb_to_rgb565};

// Use low res for consistency with ports (can switch to full for even smoother).
pub const GRAD_DIV: u16 = 2;

pub const LOW_W: u16 = 466 / GRAD_DIV;
pub const LOW_H: u16 = 466 / GRAD_DIV;
pub const LOW_PIXELS: usize = (LOW_W as usize) * (LOW_H as usize);

// (Legacy 565 consts kept for reference; current path uses Q14 component lerp + dither.)

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
            sweep_speed: 0.7,  // slow for "smooth slow" visible gentle cycling
            wave_amp: 0.09,    // subtle for premium blur blend
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
    low_buf: Vec<u16>,  // internal for low-res eval (blur via upscale)
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

        let low_pixels = (low_w as usize) * (low_h as usize);
        Self {
            config,
            low_w,
            low_h,
            out_w: width,
            out_h: height,
            ux,
            uy,
            low_buf: vec![0u16; low_pixels],
            t_q: 0,
        }
    }

    pub fn update_time(&mut self, time_ms: u32) {
        let t_f = (time_ms as f32 / 1000.0) * self.config.time_scale;
        self.t_q = (t_f * 16384.0) as i32;
    }

    /// Low-res eval (div=2) for diagonal gradient + blur via upscale.
    /// Uses monotonic arithmetic factor (no & fold), rot + dual waves, dither for 565 smooth blend.
    pub fn eval_rows(&self, low_buf: &mut [u16], row_start: usize, row_end: usize) {
        Self::eval_gradient_rows(
            self.low_w,
            self.low_h,
            self.t_q,
            self.config.sweep_speed,
            self.config.wave_amp,
            low_buf,
            row_start,
            row_end,
        );
    }

    /// Core low-res gradient math. Extracted so render_to_fb can call it without borrow conflicts
    /// while using the struct's low_buf.
    fn eval_gradient_rows(
        low_w: u16,
        low_h: u16,
        t: i32,
        sweep: f32,
        amp: f32,
        low_buf: &mut [u16],
        row_start: usize,
        row_end: usize,
    ) {
        let lw = low_w as usize;
        let lh = low_h as usize;
        let rs = row_start.min(lh);
        let re = row_end.min(lh);

        let sweep_q = (sweep * 16384.0) as i32;
        let amp_q = (amp * 16384.0) as i32;

        // Darker navy + hint dark purple, low sat (scaled to Q14)
        let dark_r: i32 = 0 * 16384 / 255;
        let dark_g: i32 = 5 * 16384 / 255;
        let dark_b: i32 = 28 * 16384 / 255;
        let purp_r: i32 = 6 * 16384 / 255;
        let purp_g: i32 = 3 * 16384 / 255;
        let purp_b: i32 = 50 * 16384 / 255;

        let cx = (lw / 2) as i32;
        let cy = (lh / 2) as i32;

        for ly in rs..re {
            let y_off = (ly as i32 - cy) * 16384 / (lh as i32);
            for lx in 0..lw {
                let x_off = (lx as i32 - cx) * 16384 / (lw as i32);

                // Slow rotation of diagonal for diversity
                let rot = t >> 9;
                let c = lut_sin_cos_q14(rot + 4096);
                let s = lut_sin_cos_q14(rot);
                let diag = (x_off * c + y_off * s) >> 13;

                // Slow phase for smooth slow motion
                let phase = t.wrapping_mul(sweep_q >> 9);

                // Monotonic ramp (arithmetic, no fold & causing bands/strips)
                let pos = diag + phase;
                let mut factor = ((pos + 32768) >> 1).clamp(0, 16384);

                // Dual subtle waves (organic premium)
                let perp = (x_off - y_off) / 2;
                let w1 = lut_sin_cos_q14(perp + (phase >> 1));
                let w2 = lut_sin_cos_q14((perp >> 1) + (phase >> 3));
                let w_contrib = ((w1 + (w2 >> 1)) * amp_q) >> 14;
                factor = (factor + w_contrib).clamp(0, 16384);

                // Q14 lerp navy <-> purple
                let mut r = dark_r + (((purp_r - dark_r) * factor) >> 14);
                let mut g = dark_g + (((purp_g - dark_g) * factor) >> 14);
                let mut b = dark_b + (((purp_b - dark_b) * factor) >> 14);

                // Vignette
                let dist2 = (x_off as i64 * x_off as i64 + y_off as i64 * y_off as i64) >> 10;
                let dist = isqrt_approx(dist2 as i32);
                let vig = (16384 - (dist * 2200 / 16384)).max(7500);
                r = (r * vig) >> 14;
                g = (g * vig) >> 14;
                b = (b * vig) >> 14;

                // Keep dark overall
                r = (r * 15200) >> 14;
                g = (g * 15200) >> 14;
                b = (b * 15200) >> 14;

                // Cheap dither (x^y + t hash) to blur 565 bands further
                let d = (((lx as i32 * 19) ^ (ly as i32 * 31) + (t >> 8)) & 7) - 3;
                let dith = d * 210;
                r = (r + dith).clamp(0, 16384);
                g = (g + dith).clamp(0, 16384);
                b = (b + (dith / 2)).clamp(0, 16384);

                let px = q14_rgb_to_rgb565(r, g, b);
                low_buf[ly * lw + lx] = px;
            }
        }
    }

    pub fn eval_pass(&self, low_buf: &mut [u16]) {
        self.eval_rows(low_buf, 0, self.low_h as usize);
    }

    /// Render to BE RGB565 fb (in PSRAM). Uses low-res eval + bilinear upscale for soft blur blending
    /// that makes colors "almost blur into eachother" and hides 565 bands. Full-res direct replaced to achieve
    /// Render using low-res + bilinear upscale (provides the soft "almost blur" blending to hide
    /// RGB565 bands). Delegates to eval_rows (source of truth for dither + factor + waves) then upscale.
    /// Call with fresh t_q each frame.
    pub fn render_to_fb(&mut self, fb: &mut [u8], t_q: i32) {
        self.t_q = t_q;
        let low_h = self.low_h as usize;
        let out_h = self.out_h as usize;
        Self::eval_gradient_rows(
            self.low_w,
            self.low_h,
            t_q,
            self.config.sweep_speed,
            self.config.wave_amp,
            &mut self.low_buf,
            0,
            low_h,
        );
        self.upscale_rows(&self.low_buf, fb, 0, out_h);
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