//! Animated Cloud - live procedural wave/cloud shader ported from the provided canvas component.
//! 
//! Design goals for this branch (animation-cloud):
//! - Live rendering only (no pre-bake, saves flash - prebake ate ~80% in main experiment).
//! - High framerate using lessons from main branch:
//!   - Fixed-point Q14 everywhere for trig and math (reuse raidal LUTs).
//!   - Low-resolution eval (divisor 2-3) + fast integer upscale.
//!   - Dual-core row split for eval (reuse/adapt worker).
//!   - Direct sync flush (proven for live updates).
//!   - LUT-based sin/cos (no libm in hot path).
//!   - Minimal code/tables: just math + small upscale tables. Target <200KB flash for shader.
//! - Simpler than Raidal-2 (4 iters vs 9 layers + polar), should hit 25-40+ FPS live.
//!
//! Research notes (planning):
//! - Original JS: downscale SCALE=2, 1024-entry sin/cos tables, per-pixel 4-iter feedback a/d with sin/cos(time/pos).
//!   Color: base + time/pos modulated accents for blue/purple cloud feel, intensity from wave.
//! - Port challenges: float feedback -> Q14 accum (a,d in 1.0=Q units representing original float).
//!   Angle args treated as radians -> use lut_sin_cos_q14( arg_q ) where arg_q = arg_f * Q (matches lut_cos_angle convention).
//!   Time mod for periodicity if wanted for "seam" but here live continuous.
//! - Expected perf: low 233x233 (div=2) ~54k pixels * 4 iters * ~6 LUTs ~1.3M ops/frame. Dual core + 240MHz + optimized = high FPS possible.
//!   Main bottleneck will still be 466x466 *2 flush (~25-40ms), so target 20-30 FPS realistic without prebake.
//! - No large assets: only code + reused SIN_LUT + small ux/uy tables (~ few KB).
//! - Future opts: more aggressive div, unroll, more LUT precomp for common coeffs, fixed div no table, SIMD if avail.
//! - Compared to prebake: this uses ~0 extra flash vs 13MB+, and is reactive (time continuous).
//!
//! Usage similar to Raidal2 for integration.

use alloc::vec::Vec;

use crate::raidal::{lut_sin_cos_q14, q14_rgb_to_rgb565};

// We use similar low res. For cloud start with div=2 for more detail/speed balance.
pub const CLOUD_DIV: u16 = 2;

pub const LOW_W: u16 = 466 / CLOUD_DIV;  // ~233
pub const LOW_H: u16 = 466 / CLOUD_DIV;
pub const LOW_PIXELS: usize = (LOW_W as usize) * (LOW_H as usize);

#[derive(Clone, Copy, Debug)]
pub struct CloudConfig {
    pub time_scale: f32,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self { time_scale: 1.0 }
    }
}

pub struct Cloud {
    config: CloudConfig,
    low_w: u16,
    low_h: u16,
    out_w: u16,
    out_h: u16,
    // On-fly upscale tables (small, like main branch learned)
    ux: Vec<UpSample1D>,
    uy: Vec<UpSample1D>,
    t_q: i32, // current time in Q14 (seconds * scale * Q)
}

#[derive(Clone, Copy)]
struct UpSample1D {
    i0: u16,
    i1: u16,
    w1: u8,
}

impl Cloud {
    pub fn new(config: CloudConfig, width: u16, height: u16) -> Self {
        // For simplicity match 466 exactly with div
        let low_w = 466 / CLOUD_DIV;
        let low_h = 466 / CLOUD_DIV;

        let ux = build_upscale_table(width, low_w, CLOUD_DIV);
        let uy = build_upscale_table(height, low_h, CLOUD_DIV);

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
        // t in "seconds" scaled, as Q14
        let t_f = (time_ms as f32 / 1000.0) * self.config.time_scale;
        self.t_q = (t_f * 16384.0) as i32;
    }

    /// Eval low-res cloud colors directly into the provided low buffer (u16 RGB565).
    /// Designed for dual-core split by rows.
    pub fn eval_rows(&self, low_buf: &mut [u16], row_start: usize, row_end: usize) {
        let lw = self.low_w as usize;
        let lh = self.low_h as usize;
        let rs = row_start.min(lh);
        let re = row_end.min(lh);

        let t = self.t_q;

        for ly in rs..re {
            let u_y = self.calc_u_q(ly as i32, self.low_h as i32);  // Q14

            for lx in 0..lw {
                let u_x = self.calc_u_q(lx as i32, self.low_w as i32);

                let mut a: i32 = 0;
                let mut d: i32 = 0;

                // 4 iterations - feedback, keep in Q14 representing original float units
                for i in 0..4 {
                    let i_q = (i * 16384) as i32;  // i as Q14 "radians" unit

                    // arg for cos: i - d + (t*0.5) - a * u_x
                    let t05 = (t / 2) as i32;  // t * 0.5 in Q
                    let term1 = i_q - d + t05;
                    let term2 = mul_q14(a, u_x);
                    let arg = term1 - term2;

                    // cos(arg)  -- arg as "radian value" *Q
                    let c = lut_sin_cos_q14(arg + 4096);  // + pi/2 Q14 approx for cos via sin( +pi/2) , 16384/4=4096

                    // arg for sin: i * u_y + a
                    let arg_sin = mul_q14(i_q, u_y) + a;
                    let s = lut_sin_cos_q14(arg_sin);

                    a += c;   // accumulate (scale may need tuning, but matches bounded)
                    d += s;
                }

                // wave = (sin(a) + cos(d)) * 0.5
                let sa = lut_sin_cos_q14(a);
                let cd = lut_sin_cos_q14(d + 4096);
                let wave = (sa + cd) / 2;  // Q14 *0.5 approx

                // intensity = 0.3 + 0.4 * wave
                let intensity = (4915 + mul_q14(6554, wave)) as i32; // 0.3*Q + 0.4*Q * wave

                // baseVal = 0.1 + 0.15 * cos(u_x + u_y + time*0.3)
                let t03 = mul_q14(t, 4915); // t*0.3
                let arg_base = u_x + u_y + t03;
                let cb = lut_sin_cos_q14(arg_base + 4096);
                let base_val = 1638 + mul_q14(2458, cb); // 0.1Q + 0.15Q * cos

                // accents
                let a15 = mul_q14(a, 24576); // a*1.5
                let t02 = mul_q14(t, 3277); // t*0.2
                let blue_accent = mul_q14(3277, lut_sin_cos_q14(a15 + t02)); // 0.2 * sin

                let d2 = mul_q14(d, 32768); // d*2
                let t01 = mul_q14(t, 1638); // t *0.1
                let purple_accent = mul_q14(2458, lut_sin_cos_q14(d2 + t01)); // 0.15*cos

                // r = (base + purple*0.8) * intensity   clamp 0-1 *Q
                let mut r = base_val + mul_q14(purple_accent, 13107); // 0.8Q
                r = mul_q14(r, intensity);
                r = r.clamp(0, 16384);

                let mut g = base_val + mul_q14(blue_accent, 9830); // 0.6
                g = mul_q14(g, intensity);
                g = g.clamp(0, 16384);

                let mut b = base_val + mul_q14(blue_accent, 19661) + mul_q14(purple_accent, 6554);
                b = mul_q14(b, intensity);
                b = b.clamp(0, 16384);

                let px = q14_rgb_to_rgb565(r, g, b);
                low_buf[ly * lw + lx] = px;
            }
        }
    }

    /// Full eval for single core / fallback. Fills low_buf[LOW_PIXELS]
    pub fn eval_pass(&self, low_buf: &mut [u16]) {
        self.eval_rows(low_buf, 0, self.low_h as usize);
    }

    pub fn upscale_rows(&self, low: &[u16], out: &mut [u8], row_start: usize, row_end: usize) {
        // Reuse similar bilinear upscale logic from raidal (ported for RGB565)
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

                let out_idx = (oy * out_w + ox) * 2;
                let px = bilinear_rgb565(c00, c10, c01, c11, [w00 as u8, w10 as u8, w01 as u8, w11 as u8]);
                // BE for display
                out[out_idx] = (px >> 8) as u8;
                out[out_idx + 1] = px as u8;
            }
        }
    }

    #[inline]
    fn calc_u_q(&self, coord: i32, size: i32) -> i32 {
        // u = (2*coord - size) / size   in Q14
        let two_coord = 2 * coord;
        let num = (two_coord - size) * 16384;
        num / size
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
        table.push(UpSample1D {
            i0,
            i1,
            w1: (frac * 255.0) as u8,
        });
    }
    table
}

#[inline(always)]
fn mul_q14(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 14) as i32
}

#[inline(always)]
fn bilinear_rgb565(c00: u16, c10: u16, c01: u16, c11: u16, w: [u8; 4]) -> u16 {
    // Simplified from raidal (port of the channel mix)
    let ch = |shift: u16, mask: u16| -> u32 {
        let f = |c: u16| -> u32 { ((c >> shift) & mask) as u32 };
        let sum = f(c00) * w[0] as u32 + f(c10) * w[1] as u32 + f(c01) * w[2] as u32 + f(c11) * w[3] as u32;
        (sum + 127) / 255
    };
    let r = ch(11, 31) as u16;
    let g = ch(5, 63) as u16;
    let b = ch(0, 31) as u16;
    (r << 11) | (g << 5) | b
}