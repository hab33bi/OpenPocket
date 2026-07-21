//! Digital time display on black background with startup fade + bezel circle animation.
//!
//! - Proper Inter font via "scale to gray" AA (build 72pt 1bpp + runtime 2x2 bitcount -> 5-level alpha).
//! - Time 3x, date 1x, both centered.
//! - Bezel: 10px pad solid thick ring, drawn/undrawn with eased curve on startup + every minute change,
//!   rounded faded end caps (comet tips) + seam heal on close.
//!   Dual precomputed lists: center-pixel angular-order for anim (~1.5 MiB), deduped row-major for static full.
//!   Anim progress is frame-indexed against build-time ease schedules (build.rs, docs/09 P0):
//!   per-frame deltas are bounded by schedule construction and phases exit on completion, not wall clock.
//! - P1: renders into a retained WatchFb canvas — incremental deltas only, no per-frame
//!   clear/copy; text redrawn only when time or fade changes; damage recorded in the DMI.
//! - Reuses: Q14 + LUT, DMA QSPI flush (direct from PSRAM).

use alloc::vec;
use alloc::vec::Vec;

use crate::display::watch_fb::{RectAcc, WatchFb};
use crate::time::WallTime;
use crate::trig::lut_sin_cos_q14;

include!(concat!(env!("OUT_DIR"), "/inter_font.rs"));
include!(concat!(env!("OUT_DIR"), "/watch_anim.rs"));

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

/// Bezel ring precompute (see docs/08-TIME-DISPLAY-HANDOFF.md).
/// BEZEL_STEPS × (BEZEL_THICKNESS + 1) taps must equal BEZEL_SCHED_TOTAL in build.rs —
/// anim durations and the ease curve live there (generate_bezel_schedules).
const BEZEL_STEPS: i32 = 36_000;
const BEZEL_THICKNESS: i32 = 10;
const FADE_MS: u32 = 2_400;

/// Anim-list layout: TAPS entries per angle step (dr = -HALF_T ..= HALF_T).
const TAPS: usize = (BEZEL_THICKNESS + 1) as usize;
const HALF_T: i32 = BEZEL_THICKNESS / 2;

/// Radial alpha profile (Q4 subpixel px from the stroke centerline):
/// solid core → antialiased edge. (A glow tail was tried 2026-07-21 and
/// removed — invisible against the AMOLED black floor at tasteful levels,
/// and its wider taps cost ~73% more sweep work.)
const AA_CORE_Q4: i32 = 64; // solid to 4.0 px
const AA_EDGE_Q4: i32 = 92; // stroke alpha reaches 0 at 5.75 px
/// Faded-edge window at each arc end, in angle steps (100 steps = 1°): alpha
/// ramps 0 → solid over this span, giving both ends a soft comet tip.
const FADE_STEPS: i32 = 400;
/// Thickness-taper zone: the ring narrows to a point on a semicircular profile
/// across the whole fade window. (Was 64 steps ≈ 2.5 px of arc — the ring hit
/// full thickness immediately, leaving a blunt dim stub that read as a square
/// at the arc origin during the startup sweep.)
const CAP_STEPS: i32 = FADE_STEPS;
/// Frames to blend the two faded rounded ends into a solid seam once the ring
/// closes (Heal), and back out again on minute change (Unheal).
/// 10 frames = 250 ms at the 40 fps anim cadence — a swift, decisive merge.
const HEAL_FRAMES: u32 = 10;

/// Lightsaber flourish v2 (PWR press while locked): blade ignition sweep →
/// whole-ring inner-glow BLOOM → shimmering hold → retraction. The glow
/// lives INSIDE the ring (only ~10 px exist outside before the panel edge):
/// a three-layer quadratic falloff reaching 40 px toward the center, Bayer-
/// dithered so the gradient reads as smooth blur instead of RGB565 banding,
/// at intensities well above the AMOLED black-crush floor.
const FL_IN_PX: i32 = 44; // inner band: 40 px design reach + falloff tail
const FL_OUT_PX: i32 = 10; // outer band: to the physical panel edge
const FL_SWEEP_F: u32 = 14;
const FL_BLOOM_F: u32 = 9;
const FL_SHIMMER_F: u32 = 18;
const FL_RETRACT_F: u32 = 14;
const FL_TOTAL_F: u32 = FL_SWEEP_F + FL_BLOOM_F + FL_SHIMMER_F + FL_RETRACT_F;

/// Bayer 4×4 ordered-dither offsets (≈ ±half an RGB565 blue step) — breaks
/// the 32-level blue quantization into invisible noise across the glow.
const BAYER4: [[i32; 4]; 4] = [
    [-7, 1, -5, 3],
    [5, -3, 7, -1],
    [-4, 4, -6, 2],
    [8, 0, 6, -2],
];

/// Bezel animation phase, advanced by frame index against the build-time schedules.
/// A sweep phase exits only when its schedule is consumed AND the ring reached the
/// schedule's end state — never on wall clock (docs/09 P0).
#[derive(Clone, Copy, PartialEq)]
enum BezelPhase {
    /// Startup sweep: 0 → full ring.
    Initial,
    /// Ring just closed: blend both faded rounded ends into a solid seam.
    Heal,
    /// Minute change, part 0: blend the solid seam back into two faded ends.
    Unheal,
    /// Minute change, part 1: full ring → 0.
    Undraw,
    /// Minute change, part 2: 0 → full ring.
    Redraw,
    /// Ring complete; pixels carried via ping-pong copy, zero ring writes.
    Static,
}

pub struct Clock {
    last_minute: u8,
    /// Minute changes trigger the undraw/redraw ring sweep only when true —
    /// the app disables it while the panel is dimmed (M5: no ornament
    /// animation playing to an empty room; text still updates).
    minute_anim: bool,
    phase: BezelPhase,
    /// Frames elapsed within the current anim phase (schedule index).
    frame_in_phase: u32,
    /// High-res angular-order center offsets for incremental arc anim.
    bezel_offsets_anim: Vec<u32>,
    /// Deduped row-major offsets — one-shot solidity blit on static entry.
    bezel_offsets_full: Vec<u32>,
    /// Contiguous horizontal runs of `bezel_offsets_full` (start byte offset,
    /// pixel count), row-major. Recoloring via sequential run fills is ~an
    /// order of magnitude faster than 19k scattered 2-byte writes through the
    /// PSRAM cache (measured 15+ ms → ~1 ms) — the scrub-fade recolors the
    /// whole ring once per level step during drags.
    bezel_runs: Vec<(u32, u16)>,
    /// Per-pixel radial alpha (W×H): AA stroke edges + glow tail. Every ring
    /// write path modulates through this so the look is identical whether a
    /// pixel was painted by the sweep, the full blit, or the scrub fade.
    ring_alpha: Vec<u8>,
    /// intensity (0..=255) → RGB565-BE ring color bytes.
    ring_lut: [(u8, u8); 256],
    /// Lightsaber flourish: row-major runs over the ±FLOURISH_BAND_PX
    /// annulus band, with per-pixel radial distance (Q3) and angle (0..=255,
    /// 0 at 12 o'clock, clockwise) in parallel arrays.
    flourish_runs: Vec<(u32, u16)>,
    flourish_d: Vec<u8>,
    flourish_ang: Vec<u8>,
    /// Frame counter; u32::MAX = inactive.
    flourish_frame: u32,
    /// Saber gradient, packed RGB565-BE: black → electric blue → neon azure
    /// → electric cyan → white-hot. Used directly at full effect strength.
    flourish_lut: [(u8, u8); 256],
    /// Same gradient as RGB tuples, for the per-frame crossfade between the
    /// resting ring hue and the neon (fade-in at ignition, fade-out at the
    /// end of retraction — the final frame is the pure ring LUT, so the
    /// handback is pixel-exact by construction).
    saber_rgb: [(u8, u8, u8); 256],
    pub bezel_anim_len: u32,
    pub bezel_full_len: u32,
    /// Angular center count currently drawn on the retained framebuffer.
    drawn_centers: usize,
    /// (h, m, fade_q14, y, mo, d) of the text currently on the retained canvas —
    /// text is only cleared/redrawn when this changes.
    last_text: (u8, u8, i32, u16, u8, u8),
    /// Bbox of the text currently on the canvas (cleared before a redraw —
    /// the date string can change length at midnight).
    last_text_bbox: (i32, i32, i32, i32),
    /// Formatted date line, e.g. "July 7th 2026".
    date_buf: [u8; 24],
    date_len: usize,
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
            minute_anim: true,
            phase: BezelPhase::Initial,
            frame_in_phase: 0,
            bezel_offsets_anim: Vec::new(),
            bezel_offsets_full: Vec::new(),
            bezel_runs: Vec::new(),
            ring_alpha: Vec::new(),
            ring_lut: [(0, 0); 256],
            flourish_runs: Vec::new(),
            flourish_d: Vec::new(),
            flourish_ang: Vec::new(),
            flourish_frame: u32::MAX,
            flourish_lut: [(0, 0); 256],
            saber_rgb: [(0, 0, 0); 256],
            bezel_anim_len: 0,
            bezel_full_len: 0,
            drawn_centers: 0,
            last_text: (255, 255, -1, 0, 0, 0),
            last_text_bbox: (0, 0, -1, -1),
            date_buf: [0; 24],
            date_len: 0,
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
            for dr in -HALF_T..=HALF_T {
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

        // Derive contiguous runs from the row-major offset list (offsets in a
        // run differ by exactly 2 bytes = adjacent pixels in a row).
        let mut runs: Vec<(u32, u16)> = Vec::with_capacity(2_048);
        for &off in &full_offs {
            match runs.last_mut() {
                Some((start, len)) if *start + (*len as u32) * 2 == off => *len += 1,
                _ => runs.push((off, 1)),
            }
        }

        // Per-pixel radial alpha for every covered pixel (AA edges + glow),
        // and the shared intensity→color LUT: one multiply + lookup per pixel
        // at runtime, no per-pixel color math.
        let mut alpha = vec![0u8; (W * H) as usize];
        for (pix, &cov) in covered.iter().enumerate() {
            if !cov {
                continue;
            }
            let px = (pix as i32) % W;
            let py = (pix as i32) / W;
            let dx = px - CX;
            let dy = py - CY;
            let r_q4 = isqrt(((dx * dx + dy * dy) as u32) * 256) as i32;
            alpha[pix] = ring_alpha_profile((r_q4 - BEZEL_R * 16).abs());
        }
        self.ring_alpha = alpha;
        for i in 0..256 {
            self.ring_lut[i] = bezel_color_bytes(Q * i as i32 / 255);
        }

        // Lightsaber flourish band: per-pixel SIGNED radial distance (Q1
        // half-px, biased +128; negative = inside the ring) + angle, over an
        // asymmetric annulus [R−FL_IN_PX, R+FL_OUT_PX], as row-major runs.
        let r_out = BEZEL_R + FL_OUT_PX;
        let r_in = BEZEL_R - FL_IN_PX;
        let (r_out2, r_in2) = (r_out * r_out, r_in * r_in);
        let mut fruns: Vec<(u32, u16)> = Vec::with_capacity(4_096);
        let mut fd: Vec<u8> = Vec::with_capacity(72_000);
        let mut fang: Vec<u8> = Vec::with_capacity(72_000);
        for py in 0..H {
            let dy = py - CY;
            for px in 0..W {
                let dx = px - CX;
                let r2 = dx * dx + dy * dy;
                if r2 < r_in2 || r2 > r_out2 {
                    continue;
                }
                let r_q4 = isqrt((r2 as u32) * 256) as i32;
                let d_biased = ((r_q4 - BEZEL_R * 16) / 8 + 128).clamp(0, 255) as u8;
                let off = ((py * W + px) * 2) as u32;
                match fruns.last_mut() {
                    Some((st, len)) if *st + (*len as u32) * 2 == off && *len < u16::MAX => {
                        *len += 1
                    }
                    _ => fruns.push((off, 1)),
                }
                fd.push(d_biased);
                // Rotate so the ignition starts at 12 o'clock.
                fang.push(angle256(dx, dy).wrapping_sub(192));
            }
        }
        self.flourish_runs = fruns;
        self.flourish_d = fd;
        self.flourish_ang = fang;

        // Electric-azure saber gradient (user-chosen): black → deep indigo
        // veil → azure glow → neon azure blade → white-hot core. The
        // crossfade in step_flourish handles continuity with the ring hue.
        for i in 0..256usize {
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
            self.saber_rgb[i] = (r as u8, g as u8, b as u8);
            let r5 = ((r as u16) * 31 / 255) & 0x1F;
            let g6 = ((g as u16) * 63 / 255) & 0x3F;
            let b5 = ((b as u16) * 31 / 255) & 0x1F;
            let px = (r5 << 11) | (g6 << 5) | b5;
            self.flourish_lut[i] = ((px >> 8) as u8, px as u8);
        }

        self.bezel_anim_len = anim_offs.len() as u32;
        self.bezel_full_len = full_offs.len() as u32;
        self.bezel_offsets_anim = anim_offs;
        self.bezel_offsets_full = full_offs;
        self.bezel_runs = runs;
    }

    /// Compose one frame onto the retained `WatchFb` canvas: incremental ring
    /// deltas + text redraw only when time/date/fade changed. Damage lands in the
    /// DMI; a frame touching nothing leaves the fb clean (caller skips the flush).
    /// `elapsed_ms` is boot-relative and drives animation schedules + startup
    /// fade; `now` is the RTC-backed wall time shown on the face.
    pub fn render(&mut self, wfb: &mut WatchFb, elapsed_ms: u32, now: &WallTime) {
        let (h, m) = (now.hour, now.minute);

        if self.last_minute == 99 {
            self.last_minute = m;
        } else if m != self.last_minute {
            self.last_minute = m;
            // Start the unheal→undraw→redraw cycle only from Static; if a prior
            // cycle is somehow still running, let it finish rather than jumping state.
            if self.minute_anim && self.phase == BezelPhase::Static && self.flourish_frame == u32::MAX
            {
                self.phase = BezelPhase::Unheal;
                self.frame_in_phase = 0;
            }
        }

        // Startup fade applies to TEXT ONLY, quantized to 32 levels so the
        // retained text redraws ~32× total during the fade, not every frame.
        // The ring always renders at full brightness — its comet-tip end
        // profile is the designed fade. (Mixing the global fade into the ring
        // left a brightness gradient along the swept body: a bright refreshed
        // start-window block against a dim stale body — the "square" — and a
        // visible flash when the one-shot solidity blit normalized it.)
        let fade_q14 = if elapsed_ms < FADE_MS {
            const LEVELS: u32 = 32;
            let level = (elapsed_ms * LEVELS / FADE_MS).min(LEVELS - 1);
            (level as i32 * Q) / LEVELS as i32
        } else {
            Q
        };

        let prev_centers = self.drawn_centers;
        let mut ring_acc = RectAcc::empty();
        let bezel_writes = if self.flourish_frame != u32::MAX {
            self.step_flourish(wfb.buf_mut(), &mut ring_acc)
        } else {
            self.step_bezel(wfb.buf_mut(), &mut ring_acc)
        };
        if !ring_acc.is_empty() {
            // +1 slack: stamps are 3×3 around each accumulated center.
            wfb.mark_rect(
                ring_acc.x0 - 1,
                ring_acc.y0 - 1,
                ring_acc.x1 + 1,
                ring_acc.y1 + 1,
            );
        }

        self.last_bezel_writes = bezel_writes as u32;
        self.last_bezel_centers = self.drawn_centers as u32;
        self.last_bezel_center_delta = if self.drawn_centers > prev_centers {
            (self.drawn_centers - prev_centers) as u32
        } else {
            (prev_centers - self.drawn_centers) as u32
        };

        // Text is retained on the canvas — only touch it when content changes.
        let key = (h, m, fade_q14, now.year, now.month, now.day);
        if self.last_text != key {
            self.last_text = key;
            self.format_date(now);
            let old = self.last_text_bbox;
            let fb = wfb.buf_mut();
            // Clear the previous text's bbox — the date string can change length.
            clear_rect(fb, old.0, old.1, old.2, old.3);
            self.draw_time_centered(fb, h, m, fade_q14);
            self.draw_date(fb, fade_q14);
            let nb = self.text_bbox();
            self.last_text_bbox = nb;
            let dirty = if old.2 < old.0 {
                nb
            } else {
                (old.0.min(nb.0), old.1.min(nb.1), old.2.max(nb.2), old.3.max(nb.3))
            };
            wfb.mark_rect(dirty.0, dirty.1, dirty.2, dirty.3);
        }
    }

    /// Format "July 7th 2026" into the retained date buffer.
    fn format_date(&mut self, now: &WallTime) {
        let mut buf = [0u8; 24];
        let mut n = 0usize;
        let name = MONTH_NAMES[((now.month.max(1) - 1) as usize).min(11)];
        for b in name.bytes() {
            buf[n] = b;
            n += 1;
        }
        buf[n] = b' ';
        n += 1;
        if now.day >= 10 {
            buf[n] = b'0' + now.day / 10;
            n += 1;
        }
        buf[n] = b'0' + now.day % 10;
        n += 1;
        let suffix: &[u8; 2] = match now.day {
            11..=13 => b"th",
            d if d % 10 == 1 => b"st",
            d if d % 10 == 2 => b"nd",
            d if d % 10 == 3 => b"rd",
            _ => b"th",
        };
        buf[n] = suffix[0];
        buf[n + 1] = suffix[1];
        n += 2;
        buf[n] = b' ';
        n += 1;
        let y = now.year;
        for div in [1000u16, 100, 10, 1] {
            buf[n] = b'0' + ((y / div) % 10) as u8;
            n += 1;
        }
        self.date_buf = buf;
        self.date_len = n;
    }

    fn date_str(&self) -> &str {
        core::str::from_utf8(&self.date_buf[..self.date_len]).unwrap_or("")
    }

    /// Repaint the whole lock scene from scratch (returning from another scene
    /// that overwrote the canvas). Ring lands complete + static; text is
    /// painted at rest AND registered in the retained-text cache — leaving the
    /// cache invalid while text sits on the canvas makes the next changed
    /// render draw new digits over these without erasing (visible overlap
    /// when the minute rolls during a drag). Marks the frame dirty.
    pub fn repaint_full(&mut self, wfb: &mut WatchFb, now: &WallTime) {
        self.phase = BezelPhase::Static;
        self.frame_in_phase = 0;
        self.drawn_centers = self.bezel_offsets_anim.len();

        self.format_date(now);
        let fb = wfb.buf_mut();
        fb.fill(0);
        let mut acc = RectAcc::empty();
        self.blit_full_ring(fb, 256, &mut acc);
        self.draw_time_centered(fb, now.hour, now.minute, Q);
        self.draw_date(fb, Q);
        self.last_text = (now.hour, now.minute, Q, now.year, now.month, now.day);
        self.last_text_bbox = self.text_bbox();
        wfb.mark_rect(0, 0, W - 1, H - 1);
    }

    /// Advance the bezel animation one frame against its build-time schedule.
    /// Per-frame deltas are bounded by schedule construction — no runtime cap.
    /// The ring always renders at full brightness (startup fade is text-only).
    fn step_bezel(&mut self, fb: &mut [u8], acc: &mut RectAcc) -> usize {
        match self.phase {
            BezelPhase::Static => 0,
            BezelPhase::Heal => {
                // Ring closed with two faded rounded ends meeting at the seam;
                // lift both end windows toward solid over HEAL_FRAMES.
                let lift = (self.frame_in_phase + 1) as i32;
                let mut writes = self.redraw_end_windows(fb, lift, HEAL_FRAMES as i32, acc);
                self.frame_in_phase += 1;
                if self.frame_in_phase >= HEAL_FRAMES {
                    // One-shot deduped full blit normalizes solidity, then the
                    // complete ring is retained on the canvas. Invisible as a
                    // brightness change — the body was drawn at full color.
                    writes += self.blit_full_ring(fb, 256, acc);
                    self.phase = BezelPhase::Static;
                }
                writes
            }
            BezelPhase::Unheal => {
                // Reverse of Heal: solid seam blends back into two faded ends
                // before the undraw sweep starts.
                let lift = (HEAL_FRAMES - 1 - self.frame_in_phase) as i32;
                let writes = self.redraw_end_windows(fb, lift, HEAL_FRAMES as i32, acc);
                self.frame_in_phase += 1;
                if self.frame_in_phase >= HEAL_FRAMES {
                    self.phase = BezelPhase::Undraw;
                    self.frame_in_phase = 0;
                }
                writes
            }
            BezelPhase::Initial | BezelPhase::Undraw | BezelPhase::Redraw => {
                let schedule: &[u32] = match self.phase {
                    BezelPhase::Initial => BEZEL_INITIAL_SCHEDULE,
                    BezelPhase::Undraw => BEZEL_UNDRAW_SCHEDULE,
                    _ => BEZEL_REDRAW_SCHEDULE,
                };

                let idx = (self.frame_in_phase as usize).min(schedule.len() - 1);
                let target = self.scale_sched(schedule[idx]);
                let writes = self.apply_bezel_target(fb, target, acc);
                self.frame_in_phase = self.frame_in_phase.saturating_add(1);

                // Exit on completion, never wall clock: schedule consumed AND ring at
                // the schedule's end state (extra frames clamp idx to the last entry).
                let end = self.scale_sched(*schedule.last().unwrap());
                if self.frame_in_phase as usize >= schedule.len() && self.drawn_centers == end
                {
                    match self.phase {
                        BezelPhase::Initial | BezelPhase::Redraw => {
                            self.phase = BezelPhase::Heal;
                            self.frame_in_phase = 0;
                        }
                        BezelPhase::Undraw => {
                            self.phase = BezelPhase::Redraw;
                            self.frame_in_phase = 0;
                        }
                        _ => {}
                    }
                }
                writes
            }
        }
    }

    /// Map a schedule center count onto the actual anim list length (identity when the
    /// precompute matches BEZEL_SCHED_TOTAL; monotonic scaling either way).
    fn scale_sched(&self, count: u32) -> usize {
        let len = self.bezel_offsets_anim.len();
        if len as u32 == BEZEL_SCHED_TOTAL {
            count as usize
        } else {
            ((count as u64 * len as u64) / BEZEL_SCHED_TOTAL as u64) as usize
        }
    }

    /// Incremental ring update with rounded faded end caps.
    /// Grow: redraw [prev_tip_fade_start .. target] so last frame's faded tip
    /// solidifies as the arc advances. Shrink: black the removed segment, then
    /// redraw the fade window behind the new tip.
    fn apply_bezel_target(&mut self, fb: &mut [u8], target: usize, acc: &mut RectAcc) -> usize {
        let len = self.bezel_offsets_anim.len();
        let target = target.min(len);
        let prev = self.drawn_centers;
        let fade_entries = FADE_STEPS as usize * TAPS;
        let mut writes = 0usize;

        if target > prev {
            let lo = prev.saturating_sub(fade_entries);
            writes += self.redraw_arc_range(fb, lo, target, target, 0, 1, acc);
        } else if target < prev {
            for i in target..prev {
                writes += black_stamp(fb, self.bezel_offsets_anim[i], acc);
            }
            let lo = target.saturating_sub(fade_entries);
            writes += self.redraw_arc_range(fb, lo, target, target, 0, 1, acc);
        }

        // Refresh the fixed start end while the moving tip's window overlaps it
        // (their combined profile changes as the tip crosses through).
        if target > 0 && target < 2 * fade_entries {
            let hi_end = target.min(fade_entries);
            writes += self.redraw_arc_range(fb, 0, hi_end, target, 0, 1, acc);
        }

        self.drawn_centers = target;
        writes
    }

    /// Redraw both end windows of the closed ring (fixed start + tip at list end)
    /// with the end profile lifted `lift/den` toward solid (Heal/Unheal seam blend).
    fn redraw_end_windows(&self, fb: &mut [u8], lift: i32, den: i32, acc: &mut RectAcc) -> usize {
        let len = self.bezel_offsets_anim.len();
        let fade_entries = (FADE_STEPS as usize * TAPS).min(len);
        let mut writes = self.redraw_arc_range(fb, 0, fade_entries, len, lift, den, acc);
        writes += self.redraw_arc_range(fb, len - fade_entries, len, len, lift, den, acc);
        writes
    }

    /// Redraw anim-list entries [lo..hi) applying the end profile (alpha fade +
    /// rounded-cap taper) relative to both arc ends: the fixed start (step 0) and
    /// the moving tip (entry `arc_end`). `lift/den` blends the profile toward
    /// solid for the seam heal. Two passes: blacks first so a narrowing cap leaves
    /// no stale pixels, colors second so they win all 3×3 overlaps.
    #[allow(clippy::too_many_arguments)]
    fn redraw_arc_range(
        &self,
        fb: &mut [u8],
        lo: usize,
        hi: usize,
        arc_end: usize,
        lift: i32,
        den: i32,
        acc: &mut RectAcc,
    ) -> usize {
        let hi = hi.min(self.bezel_offsets_anim.len());
        if lo >= hi {
            return 0;
        }
        let end_step = (arc_end / TAPS) as i32;

        // Per-step (alpha, hw) profile, cached across the TAPS-entry runs
        // sharing one angle step.
        let step_profile = |step: i32| {
            let (a0, h0) = end_profile(step); // fixed start end
            let (a1, h1) = end_profile(end_step - step); // moving tip end
            let mut alpha = a0.min(a1);
            let mut hw = h0.min(h1);
            if lift > 0 {
                alpha += (256 - alpha) * lift / den;
                hw += (HALF_T - hw) * lift / den;
            }
            (alpha, hw)
        };

        // Restamp every other angle step: at ~26 steps/px angular density with
        // 3×3 stamps, stride 2 is still ~13 steps/px — gap-free — and halves
        // the per-frame cost of the comet-tip window (the constant term that
        // dominates sweep frames at the 40 fps anim cadence).
        let mut writes = 0usize;
        // Pass 1: blacks — a narrowing cap must leave no stale pixels.
        let mut cur_step = i32::MIN;
        let (mut alpha, mut hw) = (0i32, 0i32);
        for idx in lo..hi {
            let step = (idx / TAPS) as i32;
            if step & 1 == 1 {
                continue;
            }
            if step != cur_step {
                cur_step = step;
                (alpha, hw) = step_profile(step);
            }
            let dr = (idx % TAPS) as i32 - HALF_T;
            if dr.abs() > hw {
                writes += black_stamp(fb, self.bezel_offsets_anim[idx], acc);
            }
        }
        // Pass 2: colors — so they win all stamp overlaps.
        cur_step = i32::MIN;
        for idx in lo..hi {
            let step = (idx / TAPS) as i32;
            if step & 1 == 1 {
                continue;
            }
            if step != cur_step {
                cur_step = step;
                (alpha, hw) = step_profile(step);
            }
            let dr = (idx % TAPS) as i32 - HALF_T;
            if alpha > 0 && dr.abs() <= hw {
                writes += self.stamp_ring(fb, self.bezel_offsets_anim[idx], alpha, acc);
            }
        }
        writes
    }

    /// Stamp a ring tap (3×3) at `step_alpha_q8` (0..=256), each pixel
    /// modulated by its precomputed radial alpha (AA edges).
    fn stamp_ring(&self, fb: &mut [u8], byte_off: u32, step_alpha_q8: i32, acc: &mut RectAcc) -> usize {
        let pix = byte_off as usize / 2;
        let px = (pix % W as usize) as i32;
        let py = (pix / W as usize) as i32;
        acc.add(px, py);
        let mut writes = 0usize;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let x = px + dx;
                let y = py + dy;
                if x < 0 || x >= W || y < 0 || y >= H {
                    continue;
                }
                let p = (y * W + x) as usize;
                let pa = self.ring_alpha[p] as i32;
                let (hi, lo) = self.ring_lut[(((step_alpha_q8 * pa) >> 8).min(255)) as usize];
                let i = p * 2;
                if i + 1 < fb.len() {
                    fb[i] = hi;
                    fb[i + 1] = lo;
                    writes += 1;
                }
            }
        }
        writes
    }

    /// Blast the deduped row-major ring list at `level_q8` (256 = full),
    /// each pixel modulated by its radial alpha (AA + glow).
    fn blit_full_ring(&self, fb: &mut [u8], level_q8: i32, acc: &mut RectAcc) -> usize {
        // Damage = the whole ring annulus bounding box.
        acc.add(CX - BEZEL_R - HALF_T - 1, CY - BEZEL_R - HALF_T - 1);
        acc.add(CX + BEZEL_R + HALF_T + 1, CY + BEZEL_R + HALF_T + 1);
        for &off in &self.bezel_offsets_full {
            let i = off as usize;
            let p = i / 2;
            let pa = self.ring_alpha[p] as i32;
            let (hi, lo) = self.ring_lut[(((level_q8 * pa) >> 8).min(255)) as usize];
            if i + 1 < fb.len() {
                fb[i] = hi;
                fb[i + 1] = lo;
            }
        }
        self.bezel_offsets_full.len()
    }

    /// Union bbox of time (3x) + date (1x) with small padding.
    fn text_bbox(&self) -> (i32, i32, i32, i32) {
        self.text_bbox_at(0)
    }

    /// Text bbox with the baseline shifted vertically by `y_shift`.
    fn text_bbox_at(&self, y_shift: i32) -> (i32, i32, i32, i32) {
        let mut x0 = W;
        let mut y0 = H;
        let mut x1 = 0i32;
        let mut y1 = 0i32;

        let mut absorb = |text: &str, base_y: i32, glyphs: &[Option<Glyph>; 128]| {
            let mut total_w: i32 = 0;
            for ch in text.chars() {
                if let Some(g) = get_glyph(glyphs, ch) {
                    total_w += g.advance as i32;
                }
            }
            let start_x = CX - total_w / 2;
            let mut x = start_x;
            for ch in text.chars() {
                if let Some(g) = get_glyph(glyphs, ch) {
                    let draw_h = g.height as i32;
                    let glyph_y = base_y - (draw_h + g.ymin as i32);
                    x0 = x0.min(x - 2);
                    y0 = y0.min(glyph_y - 2);
                    x1 = x1.max(x + g.width as i32 + 2);
                    y1 = y1.max(glyph_y + draw_h + 2);
                    x += g.advance as i32;
                }
            }
        };

        absorb("00:00", CY + 5 + y_shift, &TIME_GLYPHS);
        absorb(self.date_str(), CY + 70 + y_shift, &TEXT_GLYPHS);

        if x0 > x1 {
            return (0, 0, W - 1, H - 1);
        }
        (x0.max(0), y0.max(0), x1.min(W - 1), y1.min(H - 1))
    }

    /// Bbox of the text currently drawn on the retained canvas (for the drag
    /// composer to erase when it takes over the sheet).
    pub fn canvas_text_bbox(&self) -> (i32, i32, i32, i32) {
        self.last_text_bbox
    }

    /// Draw time + date for the unlock sheet, baseline shifted by `y_shift`
    /// (negative = up), clipped to rows < `clip_y`. Returns the clipped bbox.
    /// Used by the drag composer (M4); the lock scene's own path is unchanged.
    pub fn draw_sheet_text(
        &mut self,
        fb: &mut [u8],
        now: &WallTime,
        y_shift: i32,
        clip_y: i32,
    ) -> (i32, i32, i32, i32) {
        self.format_date(now);
        let mut s = [b'0'; 5];
        s[0] = b'0' + (now.hour / 10);
        s[1] = b'0' + (now.hour % 10);
        s[2] = b':';
        s[3] = b'0' + (now.minute / 10);
        s[4] = b'0' + (now.minute % 10);
        self.draw_text_centered_clipped(
            fb,
            core::str::from_utf8(&s).unwrap(),
            CY + 5 + y_shift,
            0,
            Q,
            &TIME_GLYPHS,
            clip_y,
        );
        self.draw_text_centered_clipped(fb, self.date_str(), CY + 70 + y_shift, 0, Q, &TEXT_GLYPHS, clip_y);
        let (x0, y0, x1, y1) = self.text_bbox_at(y_shift);
        (x0, y0, x1, y1.min(clip_y - 1))
    }

    /// Redraw the ring's pixels on rows `[min_y..max_y)` at the given
    /// brightness (scrub-tracked fade during the unlock drag). Row-major list
    /// → skip the prefix below `min_y`, early exit past `max_y`. Restricting
    /// the row window keeps the damage rect (and PSRAM traffic) proportional
    /// to what actually changed instead of ~the whole screen.
    pub fn draw_ring_rows(
        &self,
        fb: &mut [u8],
        min_y: i32,
        max_y: i32,
        level_q14: i32,
        acc: &mut RectAcc,
    ) -> usize {
        let level_q8 = (level_q14 >> 6).clamp(0, 256);
        let row_bytes = (W * 2) as u32;
        let min_off = min_y.max(0) as u32 * row_bytes;
        let max_off = max_y.max(0) as u32 * row_bytes;
        let mut writes = 0usize;
        // Runs never span rows, and min/max are row boundaries, so a run is
        // always entirely inside or outside the window.
        for &(start, len) in &self.bezel_runs {
            if start < min_off {
                continue;
            }
            if start >= max_off {
                break;
            }
            let s = start as usize;
            let e = (s + len as usize * 2).min(fb.len());
            let mut p = s / 2;
            for px in fb[s..e].chunks_exact_mut(2) {
                let pa = self.ring_alpha[p] as i32;
                let (hi, lo) = self.ring_lut[(((level_q8 * pa) >> 8).min(255)) as usize];
                px[0] = hi;
                px[1] = lo;
                p += 1;
            }
            writes += (e - s) / 2;
        }
        if writes > 0 {
            acc.add(CX - BEZEL_R - HALF_T - 1, min_y.max(0));
            acc.add(
                CX + BEZEL_R + HALF_T + 1,
                (max_y - 1).min(CY + BEZEL_R + HALF_T + 1),
            );
        }
        writes
    }

    /// Whether the bezel is mid-animation — the app runs a faster frame
    /// cadence (matching the 40 fps build-time schedules) while true.
    pub fn is_animating(&self) -> bool {
        self.phase != BezelPhase::Static || self.flourish_frame != u32::MAX
    }

    /// Enable/disable the minute-change ring sweep (M5: off while dimmed).
    pub fn set_minute_anim(&mut self, enabled: bool) {
        self.minute_anim = enabled;
    }

    /// Start the lightsaber flourish (PWR press while locked). Only from a
    /// resting ring; ignored while any ring animation runs.
    pub fn start_flourish(&mut self) {
        if self.phase == BezelPhase::Static && self.flourish_frame == u32::MAX {
            self.flourish_frame = 0;
        }
    }

    pub fn flourish_active(&self) -> bool {
        self.flourish_frame != u32::MAX
    }

    /// Abort the flourish by writing its zero-glow closing frame (exact
    /// resting ring), so a drag can take over a clean canvas immediately.
    pub fn cancel_flourish(&mut self, fb: &mut [u8], acc: &mut RectAcc) {
        if self.flourish_frame != u32::MAX {
            self.flourish_frame = FL_TOTAL_F - 1;
            self.step_flourish(fb, acc);
        }
    }

    /// One flourish frame: blade ignition sweep → whole-ring glow BLOOM →
    /// shimmering hold → retraction. Per pixel:
    /// val = max(resting ring alpha, angular-mask × radial profile), dithered,
    /// through the (crossfaded) saber LUT. The final retract frame uses the
    /// resting ring LUT with zero glow and no dither — pixel-exact handback.
    fn step_flourish(&mut self, fb: &mut [u8], acc: &mut RectAcc) -> usize {
        let fr = self.flourish_frame;

        // Phase → (bloom 0..=256 = inner-glow strength, shimmer = saber-hum
        // intensity wobble applied to the glow only).
        let (bloom, shimmer): (i32, i32) = if fr < FL_SWEEP_F {
            (0, 0) // blade only; the glow arrives as one beat at close
        } else if fr < FL_SWEEP_F + FL_BLOOM_F {
            // Whole-ring rise: ease-out surge to full.
            let i = (fr - FL_SWEEP_F) as i32;
            let n = FL_BLOOM_F as i32 - 1;
            let rem = n - i;
            (256 - (256 * rem * rem) / (n * n).max(1), 0)
        } else if fr < FL_SWEEP_F + FL_BLOOM_F + FL_SHIMMER_F {
            // Shimmer hold: full glow with a ±~7% deterministic hum.
            const HUM: [i32; 18] = [
                0, 8, -6, 14, -10, 4, -14, 10, -4, 12, -8, 16, -12, 2, -16, 6, -2, 0,
            ];
            (256, HUM[(fr - FL_SWEEP_F - FL_BLOOM_F) as usize])
        } else {
            // Quadratic collapse to exactly zero on the last frame.
            let i = (fr - FL_SWEEP_F - FL_BLOOM_F - FL_SHIMMER_F) as i32;
            let n = FL_RETRACT_F as i32 - 1;
            let rem = n - i;
            ((256 * rem * rem) / (n * n).max(1), 0)
        };

        // Radial profile LUT indexed by biased Q1 distance (128 = centerline,
        // < 128 = inside). Blade: white-hot centerline melting to hot edges.
        // Outer: a thin bright rim to the panel edge. Inner: three layered
        // quadratic falloffs (tight bright / soft mid / wide veil) — summed,
        // capped below blade brightness, scaled by bloom and shimmer.
        let mut prof = [0u8; 256];
        for (idx, e) in prof.iter_mut().enumerate() {
            let dh = idx as i32 - 128;
            let v: i32 = if (-11..=11).contains(&dh) {
                255 - (dh * dh) / 3
            } else if dh > 11 {
                let x = dh - 11;
                if x <= 4 { 205 - x * 8 } else { (173 - (x - 4) * 24).max(0) }
            } else {
                let x = -dh - 11;
                let layer = |a: i32, reach: i32| -> i32 {
                    if x >= reach { 0 } else { a * (reach - x) * (reach - x) / (reach * reach) }
                };
                let g = (layer(150, 20) + layer(95, 48) + layer(50, 80)).min(220);
                (((g * bloom) >> 8) * (256 + shimmer)) >> 8
            };
            *e = v.clamp(0, 255) as u8;
        }

        // Ignition front (0..=256 around the ring), quadratic ease-out:
        // strikes fast, lands smoothly. ≥512 = fully swept.
        let front: i32 = if fr < FL_SWEEP_F {
            let rem = (FL_SWEEP_F - fr) as i32;
            256 - (256 * rem * rem) / (FL_SWEEP_F * FL_SWEEP_F) as i32
        } else {
            512
        };

        // Color crossfade: the whole ring FADES INTO the neon over the first
        // 8 frames and melts back to the pure ring hue over the last 8, so
        // the effect blooms in rather than snapping, and the closing frame
        // is the resting ring LUT exactly.
        const FL_FADE_F: u32 = 8;
        let g_in = (((fr + 1) * 256) / FL_FADE_F).min(256) as i32;
        let g_out = (((FL_TOTAL_F - 1 - fr) * 256) / FL_FADE_F).min(256) as i32;
        let g = g_in.min(g_out);
        let mut lut_local = [(0u8, 0u8); 256];
        let lut: &[(u8, u8); 256] = if g >= 256 {
            &self.flourish_lut
        } else if g <= 0 {
            &self.ring_lut
        } else {
            for (i, e) in lut_local.iter_mut().enumerate() {
                let (sr, sg, sb) = self.saber_rgb[i];
                let i = i as i32;
                let (rr, rg, rb) = (200 * i / 255, 215 * i / 255, i);
                let r = rr + (((sr as i32 - rr) * g) >> 8);
                let gg = rg + (((sg as i32 - rg) * g) >> 8);
                let b = rb + (((sb as i32 - rb) * g) >> 8);
                let r5 = ((r as u16) * 31 / 255) & 0x1F;
                let g6 = ((gg as u16) * 63 / 255) & 0x3F;
                let b5 = ((b as u16) * 31 / 255) & 0x1F;
                let px = (r5 << 11) | (g6 << 5) | b5;
                *e = ((px >> 8) as u8, px as u8);
            }
            &lut_local
        };

        // Dither only while the saber look is active (g > 0) — the closing
        // frame must write exact ring-LUT values, and dither noise must never
        // persist on the resting canvas.
        let do_dither = g > 0;

        let mut writes = 0usize;
        let mut pi = 0usize;
        for &(start, len) in &self.flourish_runs {
            let s = start as usize;
            let n = len as usize;
            let px_base = s / 2;
            for k in 0..n {
                let sab = prof[self.flourish_d[pi + k] as usize] as i32;
                let f = if front >= 512 {
                    256
                } else {
                    ((front - self.flourish_ang[pi + k] as i32) * 13).clamp(0, 256)
                };
                let p = px_base + k;
                let mut val = (self.ring_alpha[p] as i32).max((sab * f) >> 8);
                if do_dither && val > 0 && val < 232 {
                    let x = p % W as usize;
                    let y = p / W as usize;
                    val = (val + BAYER4[y & 3][x & 3]).clamp(0, 255);
                }
                let (hi, lo) = lut[val as usize];
                let idx = p * 2;
                if idx + 1 < fb.len() {
                    fb[idx] = hi;
                    fb[idx + 1] = lo;
                }
            }
            pi += n;
            writes += n;
        }
        acc.add(CX - BEZEL_R - FL_OUT_PX - 1, CY - BEZEL_R - FL_OUT_PX - 1);
        acc.add(CX + BEZEL_R + FL_OUT_PX + 1, CY + BEZEL_R + FL_OUT_PX + 1);

        self.flourish_frame += 1;
        if self.flourish_frame >= FL_TOTAL_F {
            self.flourish_frame = u32::MAX;
        }
        writes
    }

    /// AOD minimal frame: black canvas, HH:MM only, drifted by a
    /// minute-indexed pixel offset to spread AMOLED wear. Invalidates the
    /// retained caches — waking repaints via `repaint_full`.
    pub fn draw_aod(&mut self, wfb: &mut WatchFb, now: &WallTime) {
        const DRIFT: [(i32, i32); 8] = [
            (0, 0),
            (5, 3),
            (-4, 6),
            (3, -5),
            (-6, -2),
            (6, 4),
            (-3, 3),
            (2, -6),
        ];
        let (dx, dy) = DRIFT[(now.minute % 8) as usize];
        let fb = wfb.buf_mut();
        fb.fill(0);
        let mut s = [b'0'; 5];
        s[0] = b'0' + (now.hour / 10);
        s[1] = b'0' + (now.hour % 10);
        s[2] = b':';
        s[3] = b'0' + (now.minute / 10);
        s[4] = b'0' + (now.minute % 10);
        self.draw_text_centered_clipped(
            fb,
            core::str::from_utf8(&s).unwrap(),
            CY + 5 + dy,
            dx,
            Q,
            &TIME_GLYPHS,
            H,
        );
        // Canvas no longer matches the clock scene: invalidate so the wake
        // repaint starts from scratch, and park the ring machine.
        self.last_text = (255, 255, -1, 0, 0, 0);
        self.last_text_bbox = (0, 0, -1, -1);
        self.phase = BezelPhase::Static;
        self.frame_in_phase = 0;
        self.drawn_centers = self.bezel_offsets_anim.len();
        wfb.mark_rect(0, 0, W - 1, H - 1);
    }

    fn draw_time_centered(&self, fb: &mut [u8], h: u8, m: u8, fade_q14: i32) {
        let mut s = [b'0'; 5];
        s[0] = b'0' + (h / 10);
        s[1] = b'0' + (h % 10);
        s[2] = b':';
        s[3] = b'0' + (m / 10);
        s[4] = b'0' + (m % 10);
        self.draw_text_centered(fb, core::str::from_utf8(&s).unwrap(), CY + 5, fade_q14, &TIME_GLYPHS);
    }

    fn draw_date(&self, fb: &mut [u8], fade_q14: i32) {
        self.draw_text_centered(fb, self.date_str(), CY + 70, fade_q14, &TEXT_GLYPHS);
    }

    fn draw_text_centered(
        &self,
        fb: &mut [u8],
        text: &str,
        base_y: i32,
        fade_q14: i32,
        glyphs: &[Option<Glyph>; 128],
    ) {
        self.draw_text_centered_clipped(fb, text, base_y, 0, fade_q14, glyphs, H);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_centered_clipped(
        &self,
        fb: &mut [u8],
        text: &str,
        base_y: i32,
        x_shift: i32,
        fade_q14: i32,
        glyphs: &[Option<Glyph>; 128],
        clip_y: i32,
    ) {
        let mut total_w: i32 = 0;
        for ch in text.chars() {
            if let Some(g) = get_glyph(glyphs, ch) {
                total_w += g.advance as i32;
            }
        }
        let start_x = CX - total_w / 2 + x_shift;

        let mut x = start_x;
        for ch in text.chars() {
            if let Some(g) = get_glyph(glyphs, ch) {
                let glyph_y = base_y - (g.height as i32 + g.ymin as i32);
                self.draw_glyph(fb, x, glyph_y, g, fade_q14, clip_y);
                x += g.advance as i32;
            }
        }
    }

    /// Blend one 4-bit-alpha atlas glyph at its native size (16 levels of
    /// fontdue's true edge coverage — no runtime scaling or reconstruction).
    fn draw_glyph(&self, fb: &mut [u8], ox: i32, oy: i32, g: &Glyph, fade_q14: i32, clip_y: i32) {
        let color_r = 240u8;
        let color_g = 240u8;
        let color_b = 245u8;

        let w = g.width as i32;
        let h = g.height as i32;
        let stride = (g.width as usize + 1) / 2;

        for gy in 0..h {
            // Clip before decoding: offscreen/clipped glyph rows (common while
            // the sheet text slides past the panel edge) must cost nothing.
            let y = oy + gy;
            if y < 0 || y >= H || y >= clip_y {
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
                let alpha = (a4 as i64) * 17; // 0..255

                let base = fade_q14 as i64;
                let r = ((color_r as i64 * base * alpha) >> (14 + 8)) as u8;
                let gg = ((color_g as i64 * base * alpha) >> (14 + 8)) as u8;
                let b = ((color_b as i64 * base * alpha) >> (14 + 8)) as u8;

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

/// (alpha_q8, cap_halfwidth) for an arc entry `d` angle steps from an arc end.
/// Alpha ramps linearly over FADE_STEPS; thickness follows a semicircular cap
/// profile over CAP_STEPS (hw = HALF_T·√(d·(2C−d))/C — a round stroke cap).
#[inline]
fn end_profile(d: i32) -> (i32, i32) {
    if d >= FADE_STEPS {
        return (256, HALF_T);
    }
    let d = d.max(0);
    let alpha = d * 256 / FADE_STEPS;
    let hw = if d >= CAP_STEPS {
        HALF_T
    } else {
        isqrt((HALF_T * HALF_T * d * (2 * CAP_STEPS - d)) as u32) as i32 / CAP_STEPS
    };
    (alpha, hw)
}

/// Octant-based integer angle, 0..=255 (0 = +x axis, increasing toward +y =
/// screen-clockwise). Linear in-octant approximation (max ~4° error) — the
/// flourish front's soft ramp absorbs it invisibly.
#[inline]
fn angle256(dx: i32, dy: i32) -> u8 {
    let ax = dx.abs();
    let ay = dy.abs();
    if ax == 0 && ay == 0 {
        return 0;
    }
    let t = (ay.min(ax) * 32) / ax.max(ay);
    let oct = if ax >= ay {
        if dx >= 0 {
            if dy >= 0 { t } else { 256 - t }
        } else if dy >= 0 {
            128 - t
        } else {
            128 + t
        }
    } else if dy >= 0 {
        if dx >= 0 { 64 - t } else { 64 + t }
    } else if dx >= 0 {
        192 + t
    } else {
        192 - t
    };
    (oct & 255) as u8
}

/// Newton integer square root (inputs here are ≤ ~102k; converges in a few steps).
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

/// Whether a rect could contain bezel-ring pixels: true when its farthest
/// corner from the ring center reaches the annulus (conservative bbox test).
/// Used by the drag composer to know a text erase clobbered ring pixels.
#[inline]
pub fn rect_touches_ring(x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
    let inner = BEZEL_R - HALF_T - 2;
    let dx = (CX - x0).max(x1 - CX).max(0);
    let dy = (CY - y0).max(y1 - CY).max(0);
    dx * dx + dy * dy >= inner * inner
}

#[inline]
pub fn clear_rect(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
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
fn black_stamp(fb: &mut [u8], byte_off: u32, acc: &mut RectAcc) -> usize {
    let pix = byte_off as usize / 2;
    let px = (pix % W as usize) as i32;
    let py = (pix / W as usize) as i32;
    acc.add(px, py);
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
                fb[i] = 0;
                fb[i + 1] = 0;
                writes += 1;
            }
        }
    }
    writes
}

/// Radial alpha profile: solid core with a ~1.75 px antialiased stroke edge.
#[inline]
fn ring_alpha_profile(d_q4: i32) -> u8 {
    if d_q4 <= AA_CORE_Q4 {
        255
    } else if d_q4 < AA_EDGE_Q4 {
        (255 * (AA_EDGE_Q4 - d_q4) / (AA_EDGE_Q4 - AA_CORE_Q4)) as u8
    } else {
        0
    }
}

fn get_glyph<'a>(glyphs: &'a [Option<Glyph>; 128], ch: char) -> Option<&'a Glyph> {
    let idx = ch as usize;
    if idx < 128 { glyphs[idx].as_ref() } else { None }
}