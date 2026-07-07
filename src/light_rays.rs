//! Light Rays - live WebGL-style light rays effect ported for ESP32.
//!
//! Matches https://reactbits.dev/backgrounds/light-rays : black background, light rays coming from the TOP (small/narrow beams),
//! slightly scaled down effect (concentrated sources in upper/center-top, narrow beams, fade not full screen).
//! 
//! Strategy (following main branch learnings + cloud port):
//! - Live first (no prebake to save flash).
//! - Q14 fixed point + LUT sin/cos (reuse from raidal).
//! - Low res eval (div=2) + on-fly upscale tables.
//! - Direct sync flush.
//! - Dual core ready (eval_rows).
//! - Minimal flash.
//!
//! Rays: vertical-ish beams from top edge, sources concentrated for "small" look (0.25-0.75 width), narrow width=0.06,
//! fade downward, subtle sway + per-ray pulse animation. Black base, bright neutral rays.

use alloc::vec::Vec;

use crate::raidal::{lut_sin_cos_q14, q14_rgb_to_rgb565};

pub const LIGHT_RAYS_DIV: u16 = 2;

pub const LOW_W: u16 = 466 / LIGHT_RAYS_DIV;
pub const LOW_H: u16 = 466 / LIGHT_RAYS_DIV;

#[derive(Clone, Copy, Debug)]
pub struct LightRaysConfig {
    pub time_scale: f32,
    pub num_rays: u8,
    pub ray_length: f32,  // in normalized units
    pub beam_width: f32,
}

impl Default for LightRaysConfig {
    fn default() -> Self {
        Self {
            time_scale: 1.0,
            num_rays: 6,
            ray_length: 1.8,   // 3x larger for bigger effect
            beam_width: 0.18,  // 3x wider beams
        }
    }
}

pub struct LightRays {
    config: LightRaysConfig,
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

impl LightRays {
    pub fn new(config: LightRaysConfig, width: u16, height: u16) -> Self {
        let low_w = LOW_W;
        let low_h = LOW_H;

        let ux = build_upscale_table(width, low_w, LIGHT_RAYS_DIV);
        let uy = build_upscale_table(height, low_h, LIGHT_RAYS_DIV);

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

    pub fn eval_rows(&self, low_buf: &mut [u16], row_start: usize, row_end: usize) {
        let lw = self.low_w as usize;
        let lh = self.low_h as usize;
        let rs = row_start.min(lh);
        let re = row_end.min(lh);

        let t = self.t_q;
        let num = self.config.num_rays as i32;  // e.g. 5
        let beam = (self.config.beam_width * 16384.0) as i32;  // small e.g. 0.06*Q ~ 1000
        let len_q = (self.config.ray_length * 16384.0) as i32;  // e.g. 0.5*Q

        let cx = (lw / 2) as i32;

        // For top-center: origin slightly above top, dir down
        let ray_y0 = -((lh / 5) as i32);  // above top for "from top"

        for ly in rs..re {
            if ly > (lh * 85 / 100) as usize {  // larger coverage, ~85% height for 3x bigger effect, still some black at bottom
                for lx in 0..lw {
                    low_buf[ly * lw + lx] = 0;  // pure black at very bottom
                }
                continue;
            }

            let sy = (ly as i32 - ray_y0) ;  // positive down

            for lx in 0..lw {
                let sx = (lx as i32 - cx) ;

                // approx length
                let dist = isqrt_approx( (sx as i64 * sx as i64 + sy as i64 * sy as i64) as i32 >> 0 );  // rough

                // horizontal closeness to center line from top (for spread)
                let dx_norm = if sy == 0 { 0 } else { (sx as i64 * 16384 / sy as i64) as i32 };

                let abs_dx = if dx_norm < 0 { -dx_norm } else { dx_norm };

                let mut intens = 0i32;

                // simulate multiple rays with offsets for width - wider spread now
                for r in 0..num {
                    let offset = (r - num/2) * (beam / 4) ;  // adjusted for larger

                    let d = abs_dx + offset;

                    if d < beam && d >= 0 {
                        let cross = beam - d;

                        // length falloff - longer now
                        let len_f = if dist > len_q { 0 } else { len_q - dist };

                        // fade from top - gentler for bigger effect
                        let fade = 16384 - ( (ly as i32 * 5000) / lh as i32 );

                        // pulse and anim
                        let pulse = (10000 + lut_sin_cos_q14( (t * 5) + (r * 1000) )) >> 1 ;

                        let mut contrib = (cross * len_f / beam ) * fade / 16384 ;
                        contrib = (contrib * pulse / 16384 ) ;

                        intens += contrib ;
                    }
                }

                intens = (intens / num).min(16384);

                // black bg + rays (white/cyan tint for light rays)
                let base = 0;  // pure black
                let v = (intens * 220 / 16384) as i32;  // brightness, slightly higher for visibility

                // slight cyan tint like example #00ffff but on device white-ish
                let r = (base + v) .min(255);
                let g = (base + v * 1) .min(255);
                let b = (base + v * 1) .min(255);

                let rq = (r as i32 * 16384 / 255) as i32;
                let gq = (g as i32 * 16384 / 255) as i32;
                let bq = (b as i32 * 16384 / 255) as i32;

                let px = q14_rgb_to_rgb565(rq, gq, bq);
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

// Approx sqrt for dist (for falloff)
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

