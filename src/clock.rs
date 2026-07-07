//! Digital time display on black background with startup fade + bezel circle animation.
//!
//! - Proper Inter font via "scale to gray" AA (build 72pt 1bpp + runtime 2x2 bitcount -> 5-level alpha).
//! - Time 3x, date 1x, both centered.
//! - Bezel: 10px pad solid thick ring, drawn/undrawn with eased curve on startup + every minute change.
//!   Dual precomputed lists: center-pixel angular-order for anim (~1.5 MiB), deduped row-major for static full.
//!   Incremental delta ring updates (not full prefix replay) + copy-from-prev ping-pong for consistent FPS.
//! - Reuses: double PSRAM fbs + ping-pong, Q14 + LUT, DMA QSPI flush (direct from PSRAM).

use alloc::vec;
use alloc::vec::Vec;

use crate::raidal::lut_sin_cos_q14;

include!(concat!(env!("OUT_DIR"), "/inter_font.rs"));

/// Correct Q14 value for +90° (pi/2).
const FRAC_PI_2_Q14: i32 = 25736;

/// Q14 constants (reuse style from raidal).
const Q: i32 = 16384;
const TAU_Q14: i32 = 103246;

/// Display params.
const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = W / 2;
const CY: i32 = H / 2;
pub const PAD: i32 = 10;
pub const BEZEL_R: i32 = (W / 2) - PAD; // 223 for 466px with 10px padding

/// Bezel ring precompute + animation tuning (see docs/08-TIME-DISPLAY-HANDOFF.md).
const BEZEL_STEPS: i32 = 36_000;
const BEZEL_THICKNESS: i32 = 10;
const BEZEL_INITIAL_MS: u32 = 8_000;
const BEZEL_UNDRAW_MS: u32 = 3_500;
const BEZEL_REDRAW_MS: u32 = 3_500;
const FADE_MS: u32 = 2_400;
/// Cap arc centers processed per frame to bound render time during anim (9 px writes each).
const MAX_CENTERS_PER_FRAME: usize = 4_500;

pub struct Clock {
    last_minute: u8,
    last_change_ms: u32,
    /// High-res angular-order center offsets for incremental arc anim.
    bezel_offsets_anim: Vec<u32>,
    /// Deduped row-major offsets (diagnostic / fallback; static uses carried fb pixels).
    bezel_offsets_full: Vec<u32>,
    pub bezel_anim_len: u32,
    pub bezel_full_len: u32,
    /// Angular center count currently drawn on the carried framebuffer.
    drawn_centers: usize,
    /// Ring is complete and carried via ping-pong copy — skip ring draws.
    ring_static: bool,
    /// Pixel writes for ring this frame (profiling).
    pub last_bezel_writes: u32,
    /// Center count change this frame (profiling).
    pub last_bezel_center_delta: u32,
    /// Current drawn center count (profiling).
    pub last_bezel_centers: u32,
}

impl Clock {
    pub fn new() -> Self {
        let mut c = Self {
            last_minute: 99,
            last_change_ms: 0,
            bezel_offsets_anim: Vec::new(),
            bezel_offsets_full: Vec::new(),
            bezel_anim_len: 0,
            bezel_full_len: 0,
            drawn_centers: 0,
            ring_static: false,
            last_bezel_writes: 0,
            last_bezel_center_delta: 0,
            last_bezel_centers: 0,
        };
        c.precompute_bezel_ring();
        c
    }

    /// Precompute anim + full bezel lists once at init.
    fn precompute_bezel_ring(&mut self) {
        let steps = BEZEL_STEPS;
        let thickness = BEZEL_THICKNESS;
        let r = BEZEL_R;
        let mut anim_offs: Vec<u32> =
            Vec::with_capacity((steps as usize) * (thickness as usize + 1));
        let mut covered = vec![false; (W * H) as usize];

        for i in 0..steps {
            let phase = (i as i64 * TAU_Q14 as i64 / steps as i64) as i32;
            let c = lut_sin_cos_q14(phase);
            let s = lut_sin_cos_q14(phase + FRAC_PI_2_Q14);
            for dr in (-thickness / 2)..=(thickness / 2) {
                let rr = r + dr;
                let xx = CX + (((rr as i64 * c as i64) + 8192) >> 14) as i32;
                let yy = CY + (((rr as i64 * s as i64) + 8192) >> 14) as i32;
                if xx < 0 || xx >= W || yy < 0 || yy >= H {
                    continue;
                }
                let center_idx = (yy * W + xx) as usize;
                anim_offs.push((center_idx as u32) * 2);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let pxx = xx + dx;
                        let pyy = yy + dy;
                        if pxx >= 0 && pxx < W && pyy >= 0 && pyy < H {
                            covered[(pyy * W + pxx) as usize] = true;
                        }
                    }
                }
            }
        }

        let mut full_offs: Vec<u32> = Vec::with_capacity(32_000);
        for py in 0..H {
            for px in 0..W {
                let pix_idx = (py * W + px) as usize;
                if covered[pix_idx] {
                    full_offs.push((pix_idx as u32) * 2);
                }
            }
        }

        self.bezel_anim_len = anim_offs.len() as u32;
        self.bezel_full_len = full_offs.len() as u32;
        self.bezel_offsets_anim = anim_offs;
        self.bezel_offsets_full = full_offs;
    }

    fn current_hm(&self, elapsed_ms: u32) -> (u8, u8) {
        let secs = (elapsed_ms / 1000) as u32;
        let h = ((secs / 3600) % 24) as u8;
        let m = ((secs / 60) % 60) as u8;
        (h, m)
    }

    #[inline]
    fn cubic_ease_in_out(t: f32) -> f32 {
        if t < 0.5 {
            4.0 * t * t * t
        } else {
            let u = -2.0 * t + 2.0;
            1.0 - (u * u * u) / 2.0
        }
    }

    /// Render one frame into `fb`.
    /// `prev` is the last displayed buffer (ping-pong front); copy it then apply incremental updates.
    /// Pass `None` only for the first prime frame (black fill).
    pub fn render_to_fb(&mut self, fb: &mut [u8], prev: Option<&[u8]>, elapsed_ms: u32) {
        match prev {
            Some(p) if p.len() == fb.len() => fb.copy_from_slice(p),
            _ => fb.fill(0),
        }

        let (h, m) = self.current_hm(elapsed_ms);

        let cur_min = m;
        if self.last_minute == 99 {
            self.last_minute = cur_min;
            self.last_change_ms = elapsed_ms;
        }
        if cur_min != self.last_minute {
            self.last_minute = cur_min;
            self.last_change_ms = elapsed_ms;
            self.ring_static = false;
            // Ring is fully visible on the carried fb; start undraw from full center count.
            self.drawn_centers = self.bezel_offsets_anim.len();
        }

        let fade_q14 = if elapsed_ms < FADE_MS {
            let tp = elapsed_ms as f32 / FADE_MS as f32;
            (tp * Q as f32) as i32
        } else {
            Q
        };

        let since = elapsed_ms - self.last_change_ms;
        let in_initial = elapsed_ms < BEZEL_INITIAL_MS && self.last_change_ms < 100;
        let in_minute_cycle = since < BEZEL_UNDRAW_MS + BEZEL_REDRAW_MS;
        let bezel_p = if in_initial {
            Self::cubic_ease_in_out(elapsed_ms as f32 / BEZEL_INITIAL_MS as f32)
        } else if since < BEZEL_UNDRAW_MS {
            1.0 - Self::cubic_ease_in_out(since as f32 / BEZEL_UNDRAW_MS as f32)
        } else if since < BEZEL_UNDRAW_MS + BEZEL_REDRAW_MS {
            let t = (since - BEZEL_UNDRAW_MS) as f32 / BEZEL_REDRAW_MS as f32;
            Self::cubic_ease_in_out(t)
        } else {
            1.0
        };

        let animating = in_initial || in_minute_cycle;
        let prev_centers = self.drawn_centers;
        let bezel_writes = if !animating && bezel_p >= 1.0 {
            self.ring_static = true;
            0
        } else {
            self.ring_static = false;
            let hi_lo = bezel_color_bytes(fade_q14);
            self.apply_bezel_delta(fb, bezel_p, hi_lo.0, hi_lo.1)
        };

        self.last_bezel_writes = bezel_writes as u32;
        self.last_bezel_centers = self.drawn_centers as u32;
        self.last_bezel_center_delta = if self.drawn_centers > prev_centers {
            (self.drawn_centers - prev_centers) as u32
        } else {
            (prev_centers - self.drawn_centers) as u32
        };

        self.clear_text_region(fb);
        self.draw_time_centered(fb, h, m, fade_q14);
        self.draw_date(fb, fade_q14);
    }

    /// Incremental ring update: only add/remove the delta arc segment this frame.
    fn apply_bezel_delta(&mut self, fb: &mut [u8], eased_p: f32, hi: u8, lo: u8) -> usize {
        let list = &self.bezel_offsets_anim;
        if list.is_empty() {
            return 0;
        }

        let ideal = ((eased_p * list.len() as f32) as usize).min(list.len());
        let target = clamp_target(self.drawn_centers, ideal, MAX_CENTERS_PER_FRAME);

        let mut writes = 0usize;
        if target > self.drawn_centers {
            for &off in &list[self.drawn_centers..target] {
                writes += write_bezel_3x3(fb, off, hi, lo);
            }
        } else if target < self.drawn_centers {
            for &off in &list[target..self.drawn_centers] {
                writes += black_bezel_3x3(fb, off);
            }
        }
        self.drawn_centers = target;
        writes
    }

    /// Black out the text region before redraw (time fade + digit changes).
    fn clear_text_region(&self, fb: &mut [u8]) {
        let (x0, y0, x1, y1) = self.text_bbox();
        clear_rect(fb, x0, y0, x1, y1);
    }

    /// Union bbox of time (3x) + date (1x) with small padding.
    fn text_bbox(&self) -> (i32, i32, i32, i32) {
        let mut x0 = W;
        let mut y0 = H;
        let mut x1 = 0i32;
        let mut y1 = 0i32;

        let mut absorb = |text: &str, base_y: i32, scale: i32| {
            let mut total_w: i32 = 0;
            for ch in text.chars() {
                if let Some(g) = get_glyph(ch) {
                    total_w += ((g.advance as i32 + 1) / 2) * scale;
                }
            }
            let start_x = CX - total_w / 2;
            let mut x = start_x;
            for ch in text.chars() {
                if let Some(g) = get_glyph(ch) {
                    let unit_h = (g.height as i32 + 1) / 2;
                    let unit_ymin = g.ymin as i32 / 2;
                    let draw_h = unit_h * scale;
                    let draw_ymin = unit_ymin * scale;
                    let glyph_y = base_y - (draw_h + draw_ymin);
                    let draw_w = ((g.width as i32 + 1) / 2) * scale;
                    x0 = x0.min(x - 2);
                    y0 = y0.min(glyph_y - 2);
                    x1 = x1.max(x + draw_w + 2);
                    y1 = y1.max(glyph_y + draw_h + 2);
                    x += ((g.advance as i32 + 1) / 2) * scale;
                }
            }
        };

        absorb("00:00", CY + 5, 3);
        absorb("July 7th 2026", CY + 70, 1);

        if x0 > x1 {
            return (0, 0, W - 1, H - 1);
        }
        (x0.max(0), y0.max(0), x1.min(W - 1), y1.min(H - 1))
    }

    fn draw_time_centered(&self, fb: &mut [u8], h: u8, m: u8, fade_q14: i32) {
        let mut s = [b'0'; 5];
        s[0] = b'0' + (h / 10);
        s[1] = b'0' + (h % 10);
        s[2] = b':';
        s[3] = b'0' + (m / 10);
        s[4] = b'0' + (m % 10);
        self.draw_text_centered(fb, core::str::from_utf8(&s).unwrap(), CY + 5, fade_q14, 3);
    }

    fn draw_date(&self, fb: &mut [u8], fade_q14: i32) {
        let s = "July 7th 2026";
        self.draw_text_centered(fb, s, CY + 70, fade_q14, 1);
    }

    fn draw_text_centered(&self, fb: &mut [u8], text: &str, base_y: i32, fade_q14: i32, scale: i32) {
        let mut total_w: i32 = 0;
        for ch in text.chars() {
            if let Some(g) = get_glyph(ch) {
                let unit_adv = (g.advance as i32 + 1) / 2;
                total_w += unit_adv * scale;
            }
        }
        let start_x = CX - total_w / 2;

        let mut x = start_x;
        for ch in text.chars() {
            if let Some(g) = get_glyph(ch) {
                let unit_h = (g.height as i32 + 1) / 2;
                let unit_ymin = g.ymin as i32 / 2;
                let draw_h = unit_h * scale;
                let draw_ymin = unit_ymin * scale;
                let glyph_y = base_y - (draw_h + draw_ymin);
                self.draw_glyph(fb, x, glyph_y, g, fade_q14, scale);
                let unit_adv = (g.advance as i32 + 1) / 2;
                x += unit_adv * scale;
            }
        }
    }

    fn draw_glyph(&self, fb: &mut [u8], ox: i32, oy: i32, g: &Glyph, fade_q14: i32, scale: i32) {
        let color_r = 240u8;
        let color_g = 240u8;
        let color_b = 245u8;

        let src_w = g.width as i32;
        let src_h = g.height as i32;
        let out_w = ((src_w + 1) / 2) * scale;
        let out_h = ((src_h + 1) / 2) * scale;

        for oy_local in 0..out_h {
            for ox_local in 0..out_w {
                let count = sample_glyph_coverage(g, ox_local, oy_local, scale);
                if count == 0 {
                    continue;
                }
                let alpha = (count * 255) / 4;

                let x = ox + ox_local;
                let y = oy + oy_local;
                if x < 0 || x >= W || y < 0 || y >= H {
                    continue;
                }

                let base = fade_q14 as i64;
                let a64 = alpha as i64;
                let r = ((color_r as i64 * base * a64) >> (14 + 8)) as u8;
                let gg = ((color_g as i64 * base * a64) >> (14 + 8)) as u8;
                let b = ((color_b as i64 * base * a64) >> (14 + 8)) as u8;

                let r5 = (r as u16 * 31 / 255) & 0x1F;
                let g6 = (gg as u16 * 63 / 255) & 0x3F;
                let b5 = (b as u16 * 31 / 255) & 0x1F;
                let px = (r5 << 11) | (g6 << 5) | b5;

                let idx = ((y * W + x) * 2) as usize;
                if idx + 1 < fb.len() {
                    fb[idx] = (px >> 8) as u8;
                    fb[idx + 1] = px as u8;
                }
            }
        }
    }
}

#[inline]
fn clamp_target(drawn: usize, ideal: usize, max_step: usize) -> usize {
    let diff = ideal.abs_diff(drawn);
    if diff <= max_step {
        return ideal;
    }
    if ideal > drawn {
        drawn + max_step
    } else {
        drawn.saturating_sub(max_step)
    }
}

#[inline]
fn bezel_color_bytes(intensity_q14: i32) -> (u8, u8) {
    let bright = 255i32;
    let ii = (intensity_q14 * bright / 256).min(Q);
    let r8 = ((200 * ii) >> 14) as u8;
    let g8 = ((215 * ii) >> 14) as u8;
    let b8 = ((255 * ii) >> 14) as u8;
    let r5 = ((r8 as u16) * 31 / 255) & 0x1F;
    let g6 = ((g8 as u16) * 63 / 255) & 0x3F;
    let b5 = ((b8 as u16) * 31 / 255) & 0x1F;
    let px = (r5 << 11) | (g6 << 5) | b5;
    ((px >> 8) as u8, px as u8)
}

#[inline]
fn clear_rect(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            let idx = ((y * W + x) * 2) as usize;
            if idx + 1 < fb.len() {
                fb[idx] = 0;
                fb[idx + 1] = 0;
            }
        }
    }
}

#[inline]
fn write_bezel_3x3(fb: &mut [u8], byte_off: u32, hi: u8, lo: u8) -> usize {
    let pix = byte_off as usize / 2;
    let px = (pix % W as usize) as i32;
    let py = (pix / W as usize) as i32;
    let mut writes = 0usize;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let x = px + dx;
            let y = py + dy;
            if x < 0 || x >= W || y < 0 || y >= H {
                continue;
            }
            let i = ((y * W + x) * 2) as usize;
            if i + 1 < fb.len() {
                fb[i] = hi;
                fb[i + 1] = lo;
                writes += 1;
            }
        }
    }
    writes
}

#[inline]
fn black_bezel_3x3(fb: &mut [u8], byte_off: u32) -> usize {
    write_bezel_3x3(fb, byte_off, 0, 0)
}

#[inline]
fn get_glyph_bit(g: &Glyph, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= g.width as i32 || y >= g.height as i32 {
        return false;
    }
    let stride = (g.width as usize + 7) / 8;
    let ux = x as usize;
    let byte_idx = (y as usize) * stride + (ux / 8);
    let bit = 7 - (ux % 8);
    (g.data[byte_idx] & (1 << bit)) != 0
}

#[inline]
fn sample_glyph_coverage(g: &Glyph, ox_local: i32, oy_local: i32, scale: i32) -> i32 {
    let q = 256i64;
    let base_x = (ox_local as i64 * 2 * q) / (scale as i64);
    let base_y = (oy_local as i64 * 2 * q) / (scale as i64);

    let mut count = 0i32;
    let offs = [0i64, q / 2];
    for &dy in &offs {
        for &dx in &offs {
            let sx = ((base_x + dx) / q) as i32;
            let sy = ((base_y + dy) / q) as i32;
            if get_glyph_bit(g, sx, sy) {
                count += 1;
            }
        }
    }
    count
}

fn get_glyph(ch: char) -> Option<&'static Glyph> {
    let idx = ch as usize;
    if idx < 128 {
        GLYPHS[idx].as_ref()
    } else {
        None
    }
}