//! Raidal-2 — WebGL-faithful aurora, optimised for ESP32-S3 + 8 MB PSRAM.
//!
//! Two-pass pipeline:
//!   Pass A: fixed-point (Q14) shader eval → `low_rgb565` (div=3 grid)
//!   Pass B: precomputed integer bilinear upscale → RGB565 BE byte framebuffer

include!(concat!(env!("OUT_DIR"), "/sin_lut.rs"));

use alloc::vec;
use alloc::vec::Vec;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicPtr, Ordering};
use libm::atan2f;

#[cfg(feature = "esp")]
use esp_hal::ram;

// Fixed-size for RENDER_DIVISOR=3 (156×156). Enables #[ram(reclaimed)] SRAM placement
// instead of PSRAM heap allocation. See docs/05-MEMORY-LINKER.md Strategy A.
pub const LOW_W: u16 = 156;
pub const LOW_H: u16 = 156;
pub const LOW_PIXELS: usize = (LOW_W as usize) * (LOW_H as usize);

/// `low_rgb565` placed in internal reclaimed SRAM (fast random access for bilinear gathers).
/// 48 KiB — must not overflow dram2_seg.
/// Board spec: 512 KiB internal SRAM + stacked 8 MB PSRAM (ESP32-S3R8, per Waveshare docs).
/// Uses MaybeUninit because reclaimed section requires Uninit marker.
#[cfg(feature = "esp")]
#[ram(reclaimed)]
pub static mut LOW_RGB565: [MaybeUninit<u16>; LOW_PIXELS] =
    [const { MaybeUninit::uninit() }; LOW_PIXELS];

#[cfg(not(feature = "esp"))]
pub static mut LOW_RGB565: [MaybeUninit<u16>; LOW_PIXELS] =
    [const { MaybeUninit::uninit() }; LOW_PIXELS];

/// Return address for runtime diagnostic (PSRAM vs internal SRAM).
#[inline]
pub fn low_rgb565_ptr() -> *const u16 {
    // raw ref, no unsafe needed for &raw
    &raw const LOW_RGB565 as *const MaybeUninit<u16> as *const u16
}

/// SAFETY helpers: caller must ensure full init before read (eval writes every pixel).
#[inline]
unsafe fn low_rgb565_mut() -> &'static mut [u16; LOW_PIXELS] {
    unsafe { &mut *(&raw mut LOW_RGB565 as *mut _ as *mut [u16; LOW_PIXELS]) }
}

#[inline]
unsafe fn low_rgb565() -> &'static [u16; LOW_PIXELS] {
    unsafe { &*(&raw const LOW_RGB565 as *const _ as *const [u16; LOW_PIXELS]) }
}

// Dedicated reclaimed static row buffers for Scratch (avoids Vec in heap + overlap with LOW).
// Size for div=3: 156 * 10 = 1560 i32s (~6.2 KiB each). Two for dual-core.
// Use MaybeUninit to satisfy esp_hal Uninit trait for reclaimed section.
pub const ROW_PACK_LEN: usize = (LOW_W as usize) * (LAYERS + 1);

#[cfg(feature = "esp")]
#[ram(reclaimed)]
pub static mut SCRATCH_ROW0: [MaybeUninit<i32>; ROW_PACK_LEN] =
    [const { MaybeUninit::uninit() }; ROW_PACK_LEN];

#[cfg(not(feature = "esp"))]
pub static mut SCRATCH_ROW0: [MaybeUninit<i32>; ROW_PACK_LEN] =
    [const { MaybeUninit::uninit() }; ROW_PACK_LEN];

#[cfg(feature = "esp")]
#[ram(reclaimed)]
pub static mut SCRATCH_ROW1: [MaybeUninit<i32>; ROW_PACK_LEN] =
    [const { MaybeUninit::uninit() }; ROW_PACK_LEN];

#[cfg(not(feature = "esp"))]
pub static mut SCRATCH_ROW1: [MaybeUninit<i32>; ROW_PACK_LEN] =
    [const { MaybeUninit::uninit() }; ROW_PACK_LEN];

/// SAFETY: callers must treat as fully written before use (eval does full row copies).
#[inline]
unsafe fn scratch_row0() -> &'static mut [i32; ROW_PACK_LEN] {
    unsafe { &mut *(&raw mut SCRATCH_ROW0 as *mut _ as *mut [i32; ROW_PACK_LEN]) }
}

#[inline]
unsafe fn scratch_row1() -> &'static mut [i32; ROW_PACK_LEN] {
    unsafe { &mut *(&raw mut SCRATCH_ROW1 as *mut _ as *mut [i32; ROW_PACK_LEN]) }
}

const LAYERS: usize = 9;
/// Q14 fixed-point: 1.0 = 16384.
const Q: i32 = 16384;
const ONE2_Q14: i32 = 19661; // 1.2 in Q14
const FRAC_PI_2_Q14: i32 = 25736;
const TAU_Q14: i32 = 103246;
const LUT_SHIFT: i32 = 9;

const LAYER_IDX_Q14: [i32; LAYERS] = [
    16384, 32768, 49152, 65536, 81920, 98304, 114688, 131072, 147456,
];

// Globals for dual-core access (set once after init). row_packed & frame_cos are read-only in eval.
static mut ROW_PACKED_PTR: AtomicPtr<i32> = AtomicPtr::new(core::ptr::null_mut());
static mut FRAME_COS_PTR: AtomicPtr<[i32; LAYERS]> = AtomicPtr::new(core::ptr::null_mut());

// Global pointer to the Raidal2 instance so app core can call methods (set in main before worker use).
pub static mut RAIDAL_PTR: AtomicPtr<Raidal2> = AtomicPtr::new(core::ptr::null_mut());

// Global for fb to allow parallel upscale writes (disjoint row ranges).
pub static mut FB_PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
const LAYER_LL_Q14: [i32; LAYERS] = [
    16384, 65536, 147456, 262144, 409600, 589824, 802816, 1048576, 1327104,
];

#[derive(Clone, Copy, Debug)]
pub struct Raidal2Config {
    pub render_divisor: u8,
    pub time_scale: f32,
}

impl Default for Raidal2Config {
    fn default() -> Self {
        Self {
            render_divisor: 3,
            time_scale: 1.0,
        }
    }
}

/// Per-frame scratch backed by reclaimed static (no heap allocation).
/// Use SCRATCH0 for core0, SCRATCH1 for app core (dual).
pub struct Scratch {
    row_pack: &'static mut [i32],
}

impl Scratch {
    /// Returns a Scratch using the primary static buffer (core 0).
    pub fn new(_low_w: u16) -> Self {
        // SAFETY: exclusive use per core; written before reads in eval.
        Self {
            row_pack: unsafe { &mut *scratch_row0() },
        }
    }

    /// Returns a Scratch using the secondary static buffer (for app core in dual-core).
    pub fn new_secondary(_low_w: u16) -> Self {
        Self {
            row_pack: unsafe { &mut *scratch_row1() },
        }
    }
}

#[derive(Clone, Copy)]
struct UpSample1D {
    i0: u16,
    i1: u16,
    w1: u8,
}



pub struct Raidal2 {
    config: Raidal2Config,
    low_w: u16,
    low_h: u16,
    out_w: u16,
    out_h: u16,
    row_stride: usize,
    row_packed: Vec<i32>,
    // ux/uy 1D tables (tiny) for on-the-fly bilinear indices. Replaces large upmap Vec to save PSRAM traffic.
    ux: Vec<UpSample1D>,
    uy: Vec<UpSample1D>,
    // low_rgb565 is now a global static in reclaimed SRAM (see above)
    frame_cos_q14: [i32; LAYERS],
    // Precomputed sin/cos of the per-layer phase offsets (for LUT call reduction via angle addition).
    // sin_d[layer][k] = sin( LAYER_IDX[layer] + k*2 * Q ) in Q14, for k=0,1,2 (offsets 0,2,4)
    sin_d: [[i32; 3]; LAYERS],
    cos_d: [[i32; 3]; LAYERS],
}

impl Raidal2 {
    pub fn new(config: Raidal2Config, width: u16, height: u16) -> Self {
        let div = config.render_divisor.max(1) as u16;
        let low_w = (width + div - 1) / div;
        let low_h = (height + div - 1) / div;
        let lw = low_w as usize;
        let lh = low_h as usize;
        let row_stride = lw * (LAYERS + 1);
        let res_y = height as f32;
        let inv_ry = 1.0 / res_y;
        let step = div as f32;
        let res_x = width as f32;

        let mut row_packed = vec![0_i32; lh * row_stride];

        for ly in 0..low_h {
            let row_base = (ly as usize) * row_stride;
            for lx in 0..low_w {
                let idx = row_base + (lx as usize);
                let frag_x = lx as f32 * step + step * 0.5;
                let frag_y = ly as f32 * step + step * 0.5;
                let px = frag_x - res_x * 0.5;
                let py = frag_y - res_y * 0.5;
                let plen_norm = libm::sqrtf(px * px + py * py) * inv_ry;
                row_packed[idx] = libm::roundf(atan2f(py, px) * Q as f32) as i32;

                for layer in 0..LAYERS {
                    let l = (layer + 1) as f32;
                    let a = l * l / 80.0 - plen_norm;
                    let denom = maxf(a, -a * 3.0) + 2.0 * inv_ry;
                    let inv = 0.03 / denom;
                    let dst = row_base + lw + layer * lw + (lx as usize);
                    row_packed[dst] = libm::roundf(inv * 1_048_576.0) as i32;
                }
            }
        }

        let ux = build_upscale_table(width, low_w, div);
        let uy = build_upscale_table(height, low_h, div);

        let mut frame_cos_q14 = [0i32; LAYERS];

        // Precompute sin/cos of offsets for LUT reduction (2 calls per layer instead of 4).
        let mut sin_d = [[0i32; 3]; LAYERS];
        let mut cos_d = [[0i32; 3]; LAYERS];
        for layer in 0..LAYERS {
            let li = LAYER_IDX_Q14[layer];
            for k in 0..3 {
                let off = li + k as i32 * 2 * Q;
                sin_d[layer][k] = lut_sin_cos_q14(off);
                cos_d[layer][k] = lut_sin_cos_q14(off + FRAC_PI_2_Q14);
            }
        }

        // Publish read-only buffers for app core (dual eval). Use raw to satisfy 2024 static_mut rules.
        unsafe {
            let p0 = &raw mut ROW_PACKED_PTR;
            (*p0).store(row_packed.as_mut_ptr(), Ordering::SeqCst);
            let p1 = &raw mut FRAME_COS_PTR;
            (*p1).store(&raw mut frame_cos_q14 as *mut _, Ordering::SeqCst);
        }

        Self {
            config,
            low_w,
            low_h,
            out_w: width,
            out_h: height,
            row_stride,
            row_packed,
            ux,
            uy,
            frame_cos_q14,
            sin_d,
            cos_d,
        }
    }

    pub fn init_time(&mut self, time_ms: u32) {
        self.update_frame_cos(time_ms);
    }

    pub fn update_time(&mut self, time_ms: u32) {
        self.update_frame_cos(time_ms);
    }

    pub fn eval_pass(&self, scratch: &mut Scratch) {
        let lw = self.low_w as usize;
        let lh = self.low_h as usize;

        let low = unsafe { low_rgb565_mut() };

        // Efficient row-by-row: copy the packed row data (from PSRAM) into internal scratch **once per row**.
        for ly in 0..lh {
            let src = ly * self.row_stride;
            scratch.row_pack[..self.row_stride]
                .copy_from_slice(&self.row_packed[src..src + self.row_stride]);

            let row_base = ly * lw;
            for lx in 0..lw {
                let px = eval_pixel_q14(&scratch.row_pack, lw, lx, &self.frame_cos_q14, &self.sin_d, &self.cos_d);
                low[row_base + lx] = px;
            }
        }
    }

    /// Eval pixels in flat index range [start, end).
    /// Safe for dual-core pixel-index split (no row seams).
    /// Copies row data only when the logical row changes.
    pub fn eval_pass_range(&self, scratch: &mut Scratch, start: usize, end: usize) {
        let lw = self.low_w as usize;
        let total = lw * self.low_h as usize;
        let s = start.min(total);
        let e = end.min(total);

        let low = unsafe { low_rgb565_mut() };

        let mut current_ly: Option<usize> = None;

        for p in s..e {
            let ly = p / lw;
            let lx = p % lw;

            if current_ly != Some(ly) {
                let src = ly * self.row_stride;
                scratch.row_pack[..self.row_stride]
                    .copy_from_slice(&self.row_packed[src..src + self.row_stride]);
                current_ly = Some(ly);
            }

            let px = eval_pixel_q14(&scratch.row_pack, lw, lx, &self.frame_cos_q14, &self.sin_d, &self.cos_d);
            low[p] = px;
        }
    }

    /// Eval a contiguous row range [row_start, row_end) — preferred for dual-core (natural copies, no per-pixel div).
    /// Writes only to its rows in the global LOW_RGB565.
    #[inline]
    #[unsafe(link_section = ".iram0.text")]
    pub fn eval_rows(&self, scratch: &mut Scratch, row_start: usize, row_end: usize) {
        let lw = self.low_w as usize;
        let lh = self.low_h as usize;
        let rs = row_start.min(lh);
        let re = row_end.min(lh);

        let low = unsafe { low_rgb565_mut() };

        for ly in rs..re {
            let src = ly * self.row_stride;
            // Offloaded bulk copy (GDMA mem2mem in production — see dma_memcpy stub in main + plan).
            // Currently falls back to memcpy; real GDMA frees CPU for math and reduces PSRAM contention on dual.
            let src_bytes = unsafe {
                core::slice::from_raw_parts(
                    self.row_packed.as_ptr().add(src) as *const u8,
                    self.row_stride * 4,
                )
            };
            let dst_bytes = unsafe {
                core::slice::from_raw_parts_mut(
                    scratch.row_pack.as_mut_ptr() as *mut u8,
                    self.row_stride * 4,
                )
            };
            // In real: dma_memcpy(src_bytes, dst_bytes);
            dst_bytes.copy_from_slice(src_bytes);

            let row_base = ly * lw;
            for lx in 0..lw {
                let px = eval_pixel_q14(&scratch.row_pack, lw, lx, &self.frame_cos_q14, &self.sin_d, &self.cos_d);
                low[row_base + lx] = px;
            }
        }
    }

    #[inline]
    #[unsafe(link_section = ".iram0.text")]
    pub fn upscale_pass(&self, out: &mut [u8]) {
        self.upscale_rows(out, 0, self.out_h as usize);
    }

    /// Upscale a row range of output [row_start, row_end). Safe for dual-core split (disjoint fb writes).
    /// Uses the (now on-the-fly) index calc + low in SRAM.
    #[unsafe(link_section = ".iram0.text")]
    pub fn upscale_rows(&self, out: &mut [u8], row_start: usize, row_end: usize) {
        let out_w = self.out_w as usize;
        let row_bytes = out_w * 2;
        let rs = row_start.min(self.out_h as usize);
        let re = row_end.min(self.out_h as usize);

        let low = unsafe { low_rgb565() };
        let lw = self.low_w as usize;

        for oy in rs..re {
            // SRAM row buffer for fast writes during computation, then one bulk copy to PSRAM fb per row.
            // Avoids many slow per-pixel PSRAM stores.
            let mut row_buf = [0u8; 932]; // 466*2 max

            let vy = &self.uy[oy];
            let row0 = vy.i0 as usize * lw;
            let row1 = vy.i1 as usize * lw;
            let wy1 = vy.w1 as u16;
            let wy0 = 255 - wy1;

            for ox in 0..out_w {
                let hx = &self.ux[ox];
                let wx1 = hx.w1 as u16;
                let wx0 = 255 - wx1;

                let w00 = ((wx0 * wy0) / 255) as u8;
                let w10 = ((wx1 * wy0) / 255) as u8;
                let w01 = ((wx0 * wy1) / 255) as u8;
                let w11 = ((wx1 * wy1) / 255) as u8;

                let c00 = low[row0 + hx.i0 as usize];
                let c10 = low[row0 + hx.i1 as usize];
                let c01 = low[row1 + hx.i0 as usize];
                let c11 = low[row1 + hx.i1 as usize];

                let px = bilinear_rgb565(c00, c10, c01, c11, [w00, w10, w01, w11]);
                let o = ox * 2;
                row_buf[o] = (px >> 8) as u8;
                row_buf[o + 1] = px as u8;
            }

            let fb_start = oy * row_bytes;
            // Bulk copy row to PSRAM fb — offloaded to GDMA mem2mem in production (see dma_memcpy stub in main).
            // Real GDMA frees CPU and can use wider bursts on the PSRAM interface.
            // In real: dma_memcpy(&row_buf[0..row_bytes], &mut out[fb_start..fb_start + row_bytes]);
            out[fb_start..fb_start + row_bytes].copy_from_slice(&row_buf[0..row_bytes]);
        }
    }

    fn update_frame_cos(&mut self, time_ms: u32) {
        let t = (time_ms as f32 / 1000.0) * self.config.time_scale;
        for layer in 0..LAYERS {
            let angle = (layer + 1) as f32 - t;
            self.frame_cos_q14[layer] = lut_cos_angle_q14(angle);
        }
    }
}

#[inline(always)]
#[unsafe(link_section = ".iram0.text")]
fn eval_pixel_q14(
    pack: &[i32],
    lw: usize,
    lx: usize,
    frame_cos: &[i32; LAYERS],
    sin_d: &[[i32; 3]; LAYERS],
    cos_d: &[[i32; 3]; LAYERS],
) -> u16 {
    let atan_p = pack[lx];
    // Use i64 accumulators for full fidelity and headroom during summation (previous i32+saturating caused lines and color artifacts).
    // The LUT reduction (2 calls/layer) and other opts provide speed without breaking the math.
    let mut o_r: i64 = 0;
    let mut o_g: i64 = 0;
    let mut o_b: i64 = 0;

    for layer in 0..LAYERS {
        let edge0 = frame_cos[layer];
        let a = atan_p + edge0 + LAYER_LL_Q14[layer];
        // Only 2 LUT calls per layer (was 4)
        let sin_a = lut_sin_cos_q14(a);
        let cos_a = lut_sin_cos_q14(a + FRAC_PI_2_Q14);
        let sm = smoothstep_q14(edge0, Q * 2, cos_a);
        let inv_q20 = pack[lw + layer * lw + lx];
        let factor = ((inv_q20 as i64 * sm as i64) >> 20) as i64;

        // angle addition, result in Q14
        let s0 = ONE2_Q14 + (((sin_a as i64 * cos_d[layer][0] as i64 + cos_a as i64 * sin_d[layer][0] as i64) >> 14) as i32);
        let s1 = ONE2_Q14 + (((sin_a as i64 * cos_d[layer][1] as i64 + cos_a as i64 * sin_d[layer][1] as i64) >> 14) as i32);
        let s2 = ONE2_Q14 + (((sin_a as i64 * cos_d[layer][2] as i64 + cos_a as i64 * sin_d[layer][2] as i64) >> 14) as i32);

        o_r += factor * s0 as i64;
        o_g += factor * s1 as i64;
        o_b += factor * s2 as i64;
    }

    let r = fast_tanh_q14((o_r >> 14) as i32);
    let g = fast_tanh_q14((o_g >> 14) as i32);
    let b = fast_tanh_q14((o_b >> 14) as i32);
    q14_rgb_to_rgb565(r, g, b)
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
#[unsafe(link_section = ".iram0.text")]
pub fn lut_sin_cos_q14(phase: i32) -> i32 {
    let mut x = phase % TAU_Q14;
    if x < 0 {
        x += TAU_Q14;
    }
    let idx_f = ((x as i64) << LUT_SHIFT) / TAU_Q14 as i64;
    let i0 = (idx_f as usize) & 511;
    let frac = (idx_f - i0 as i64) as i32;
    let i1 = (i0 + 1) & 511;
    let v0 = SIN_LUT_I16[i0] as i32;
    let v1 = SIN_LUT_I16[i1] as i32;
    v0 + ((v1 - v0) * frac >> LUT_SHIFT)
}

#[inline(always)]
pub fn lut_cos_angle_q14(angle_rad: f32) -> i32 {
    let tau = core::f32::consts::TAU;
    let mut a = angle_rad % tau;
    if a < 0.0 {
        a += tau;
    }
    let phase = libm::roundf(a * Q as f32) as i32;
    lut_sin_cos_q14(phase + FRAC_PI_2_Q14)
}

#[inline(always)]
fn smoothstep_q14(edge0: i32, edge1: i32, x: i32) -> i32 {
    let denom = edge1 - edge0;
    if denom <= 0 {
        return 0;
    }
    // Use 64 only for the division safety; result clamped to Q14
    let mut t = ((x - edge0) as i64 * Q as i64) / denom as i64;
    if t < 0 {
        t = 0;
    }
    if t > Q as i64 {
        t = Q as i64;
    }
    let t = t as i32;
    // t2 and hermite with i32 where possible (less i64 pressure)
    let t2 = t.saturating_mul(t) / Q;
    let num = t2 as i64 * (3 * Q - 2 * t) as i64;
    (num / Q as i64) as i32
}

#[inline(always)]
fn fast_tanh_q14(x: i32) -> i32 {
    let lim = (3.5 * Q as f32) as i32;
    if x > lim {
        return Q;
    }
    if x < -lim {
        return -Q;
    }
    // i32 path where possible
    let x2 = (x as i64 * x as i64) / Q as i64;
    let num = (x as i64) * (27 * Q as i64 + x2);
    let den = 27 * Q as i64 + 9 * x2;
    (num / den) as i32
}

#[inline(always)]
pub fn q14_rgb_to_rgb565(r: i32, g: i32, b: i32) -> u16 {
    let clamp = |v: i32| v.clamp(0, Q) as u32;
    let r5 = (clamp(r) * 31 / Q as u32) as u16;
    let g6 = (clamp(g) * 63 / Q as u32) as u16;
    let b5 = (clamp(b) * 31 / Q as u32) as u16;
    (r5 << 11) | (g6 << 5) | b5
}

#[inline(always)]
fn bilinear_rgb565(c00: u16, c10: u16, c01: u16, c11: u16, w: [u8; 4]) -> u16 {
    let ch = |shift: u16, mask: u16| -> u32 {
        let f = |c: u16| -> u32 { ((c >> shift) & mask) as u32 };
        let sum = f(c00) * w[0] as u32
            + f(c10) * w[1] as u32
            + f(c01) * w[2] as u32
            + f(c11) * w[3] as u32;
        (sum + 127) / 255
    };
    let r = ch(11, 31) as u16;
    let g = ch(5, 63) as u16;
    let b = ch(0, 31) as u16;
    (r << 11) | (g << 5) | b
}

#[inline(always)]
fn maxf(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}