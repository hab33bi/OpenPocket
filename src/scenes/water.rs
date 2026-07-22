//! Water — tilt-driven liquid simulation (src/scenes/water.rs).
//!
//! Model: a 2-D pairwise-hash PARTICLE liquid — square particles carrying
//! position + velocity, a uniform spatial hash, and sqrt-free / divide-free
//! short-range repulsion for incompressibility (mitxela's fluid-pendant
//! confirms the model: FLIP-style motion *with explicit particle collisions*
//! — "without the collisions step, the whole fluid collapses into an
//! overlapping mess"). Gravity comes from the QMI8658 accelerometer, taken
//! RELATIVE to a neutral pose captured by an on-open calibration (like the
//! Waveshare demo's "hold still" step) so the pool sits centred at whatever
//! angle you actually hold the watch. A flick's jerk throws a breaking-crest
//! spray of real particles that arc and re-absorb.
//!
//! Render: mitxela's density-field idea, cheaply — each particle is a soft
//! radial GLOW blob (a baked LUT-index falloff) MAX-blended into the frame,
//! so overlapping blobs MERGE into one continuous luminous body with a
//! glowing rim (no hard squares, no connecting lines). Colour is the
//! deep-indigo→neon→cyan→white WATER_LUT, shaded by depth/speed, aurora-
//! drifted, Bayer index-dithered.
//!
//! Everything in the hot loop is INTEGER / Q6 fixed-point (no FPU). State is
//! a normal struct held in apps::State (internal SRAM). See
//! docs/water/IMPL-SPEC.md + docs/water/review-*.md for the full design,
//! overflow proof, and the 13 adversarial fixes carried over.

#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]

use crate::display::watch_fb::WatchFb;
use crate::scenes::lock; // NOISE_A/NOISE_B + TEXT_GLYPHS
use crate::scenes::wheel; // text helpers for the calibration screen
use esp_println::println;

// Build-time tables: WATER_LUT [(u8,u8);256] gradient; PUSH_LUT [i16;257]
// repulsion by separation² (monotonic, i16::MAX clamp); GLOW_* unused now.
include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));

// --- geometry / vessel -----------------------------------------------------
const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = 233;
const CY: i32 = 233;
const WALL_R: i32 = 222;
const WALL_R2: i32 = WALL_R * WALL_R;
/// Top clip. No status bar in Water (user), so the liquid fills the whole
/// round face — just inside the top chord.
const CLOCK_Y1: i32 = 10;

// --- particle system -------------------------------------------------------
/// ~2× the original 448 (user: "3× more", capped at 2× to stay on-stack /
/// no-unsafe; the static-module escape hatch is documented if we push higher).
const NW: usize = 900;
const GRID_W: i32 = 30;
const CELL_PX: i32 = 16;
const NCELL: usize = (GRID_W * GRID_W) as usize; // 900
const H_PX: i32 = 16;
const H2: i32 = H_PX * H_PX; // 256
/// Candidate cap (denser pool → nearest neighbours dominate; keeps relax bounded).
const K_CAND: u32 = 16;
/// 3×3 neighbour offsets, CENTRE-FIRST (nearest, strongest repulsions before the cap).
const CELL_OFF: [(i32, i32); 9] = [
    (0, 0),
    (-1, 0),
    (1, 0),
    (0, -1),
    (0, 1),
    (-1, -1),
    (1, -1),
    (-1, 1),
    (1, 1),
];

// --- fixed point — Q6 (1/64 px) --------------------------------------------
const FP: i32 = 6;
const VMAX_PX: i32 = 16;
const VMAX: i32 = VMAX_PX << FP; // 1024 Q6

// --- dynamics --------------------------------------------------------------
const DAMP: i32 = 254; // /256 round-toward-zero (both signs decay → settles)
const REST_E: i32 = 26; // wall restitution e≈0.10
const PUSH_SHIFT: i32 = 7;
const VISC_K: i32 = 40;
const COLO_NUDGE: i32 = 96; // deterministic d²==0 separation (anti-singularity)

// --- IMU → screen mapping (from the on-device logs: −accelerometer in-plane) --
const IMU_X_SRC: usize = 0; // raw axis → screen +x (right)
const IMU_X_SGN: i32 = -1; // gravity = −accel in-plane (water flows toward the lowered edge)
const IMU_Y_SRC: usize = 1; // raw axis → screen +y (down)
const IMU_Y_SGN: i32 = -1;
const GRAV_DIV: i32 = 64; // raw/64 → 128 Q6 (2 px/frame²) at 1 g; round-toward-zero
const GMAX: i32 = 512; // ±4 g planar clamp
const DOWN_G: i32 = 128; // dead-IMU down-vector
const DEAD_LIMIT: u16 = 40; // ~1 s of faults → snap to down-vector

// --- onboard calibration (capture the neutral pose) ------------------------
/// Stable live frames required to latch the neutral reference.
const CALIB_NEED: u32 = 25;
/// Per-axis frame-to-frame delta (LSB) under which a sample counts as "still".
const CALIB_STILL: i32 = 420;
/// Give up waiting for stillness after this many frames → absolute gravity.
const CALIB_TIMEOUT: u32 = 150;

// --- jerk → slosh + spray --------------------------------------------------
const JERK_TH: i32 = 2600;
const SPRAY_SHR: i32 = 8;

// --- rest breathing --------------------------------------------------------
const BREATHE_G_TH: i32 = 40;
const BREATHE_AMP: i32 = 48;

// --- surface detection (brightness cue only; no meniscus line) -------------
const SURF_LO: u32 = 3;
const SURF_HI: u32 = 5;

// --- render: merging glow blobs (mitxela density field, cheap) -------------
/// Blob radius. Each particle paints a soft radial falloff; overlapping blobs
/// MAX-merge into one continuous luminous body.
const BLOB_R: i32 = 3;
/// Per-blob LUT-index falloff: 0 at the centre (full colour), −k·d² toward the
/// rim (fades to deep indigo → the glowing edge). MAX-blend then merges them.
const BLOB_FALLOFF: i32 = 11;
/// Damage half-extent per particle (blob + margin).
const SQ_MARGIN: i32 = BLOB_R + 1;
const BAYER4: [[i32; 4]; 4] = [
    [-8, 0, -6, 2],
    [4, -4, 6, -2],
    [-5, 3, -7, 1],
    [7, -1, 5, -3],
];

// ===========================================================================
pub struct Water {
    px: [i16; NW],
    py: [i16; NW],
    vx: [i16; NW],
    vy: [i16; NW],
    nbr: [u8; NW],
    flags: [u8; NW],
    cell_start: [u16; NCELL + 1],
    cursor: [u16; NCELL + 1],
    order: [u16; NW],
    // neutral-pose calibration
    ref_x: i32,
    ref_y: i32,
    calib_sx: i32,
    calib_sy: i32,
    calib_n: u32,
    calib_to: u32,
    calibrating: bool,
    // IMU history
    last_ax: i16,
    last_ay: i16,
    last_az: i16,
    last_rmx: i32,
    last_rmy: i32,
    dead_ctr: u16,
    ever_live: bool,
    // housekeeping
    rng: u32,
    phase: u32,
    last_bbox: (i16, i16, i16, i16),
    seeded: bool,
}

impl Water {
    pub const fn new() -> Self {
        Self {
            px: [0; NW],
            py: [0; NW],
            vx: [0; NW],
            vy: [0; NW],
            nbr: [0; NW],
            flags: [0; NW],
            cell_start: [0; NCELL + 1],
            cursor: [0; NCELL + 1],
            order: [0; NW],
            ref_x: 0,
            ref_y: 0,
            calib_sx: 0,
            calib_sy: 0,
            calib_n: 0,
            calib_to: 0,
            calibrating: false,
            last_ax: 0,
            last_ay: 0,
            last_az: 0,
            last_rmx: 0,
            last_rmy: 0,
            dead_ctr: 0,
            ever_live: false,
            rng: 0x2545_F491,
            phase: 0,
            last_bbox: (0, 0, -1, -1),
            seeded: false,
        }
    }

    /// Called from open_app: seed the pool + START the on-open calibration.
    pub fn open(&mut self) {
        self.seed();
        self.calibrating = true;
        self.calib_sx = 0;
        self.calib_sy = 0;
        self.calib_n = 0;
        self.calib_to = 0;
        self.dead_ctr = 0;
        self.last_bbox = (0, 0, -1, -1);
    }

    fn seed(&mut self) {
        self.rng ^= 0x9E37_79B9;
        const COLS: i32 = 44;
        const SPACING: i32 = 6;
        let cx0 = CX - (COLS * SPACING) / 2;
        let cy0 = CY + 20;
        for i in 0..NW {
            let col = i as i32 % COLS;
            let row = i as i32 / COLS;
            let mut x = cx0 + col * SPACING + (rng(&mut self.rng) & 3) as i32;
            let mut y = cy0 + row * SPACING;
            let dx = x - CX;
            let dy = y - CY;
            if dx * dx + dy * dy > (WALL_R - 4) * (WALL_R - 4) {
                x = CX + dx / 2;
                y = CY + dy / 2;
            }
            self.px[i] = (x << FP) as i16;
            self.py[i] = (y << FP) as i16;
            self.vx[i] = 0;
            self.vy[i] = 0;
            self.nbr[i] = 8;
            self.flags[i] = 0;
        }
        self.seeded = true;
    }

    /// Reveal-frame render (fill-in on open). During calibration this shows the
    /// "hold still" screen fading up; once calibrated it fades the pool up.
    pub fn reveal(&mut self, wfb: &mut WatchFb, q_q8: i32, elapsed_ms: u32) {
        if !self.seeded {
            self.open();
        }
        if self.calibrating {
            // Calibration doesn't advance during the morph (no IMU here) —
            // show its real progress (0), the run-loop tick fills it once the
            // morph completes and live samples arrive.
            let prog = ((self.calib_n * 256) / CALIB_NEED).min(256) as i32;
            self.render_calib(wfb, prog);
        } else {
            self.render(wfb, elapsed_ms, q_q8.clamp(0, 256));
        }
    }

    /// Per-frame entry. The run loop owns i2c and just read the accel.
    pub fn tick(&mut self, wfb: &mut WatchFb, imu: Option<(i16, i16, i16)>, elapsed_ms: u32) {
        let acc = imu.unwrap_or((self.last_ax, self.last_ay, self.last_az));
        let live = imu.is_some();
        if live {
            self.dead_ctr = 0;
            self.ever_live = true;
        } else {
            self.dead_ctr = self.dead_ctr.saturating_add(1);
        }
        if !self.seeded {
            self.open();
        }

        let rmx = IMU_X_SGN * axis(acc, IMU_X_SRC);
        let rmy = IMU_Y_SGN * axis(acc, IMU_Y_SRC);

        // --- on-open calibration: latch the neutral pose while held still ---
        if self.calibrating {
            self.calib_to += 1;
            if live {
                if (rmx - self.last_rmx).abs() < CALIB_STILL
                    && (rmy - self.last_rmy).abs() < CALIB_STILL
                {
                    self.calib_sx += rmx;
                    self.calib_sy += rmy;
                    self.calib_n += 1;
                } else {
                    self.calib_n = 0;
                    self.calib_sx = 0;
                    self.calib_sy = 0;
                }
                self.last_rmx = rmx;
                self.last_rmy = rmy;
                self.last_ax = acc.0;
                self.last_ay = acc.1;
                self.last_az = acc.2;
            }
            if self.calib_n >= CALIB_NEED {
                self.ref_x = self.calib_sx / self.calib_n as i32;
                self.ref_y = self.calib_sy / self.calib_n as i32;
                self.calibrating = false;
            } else if self.calib_to > CALIB_TIMEOUT {
                self.ref_x = 0; // never settled / dead IMU → absolute gravity
                self.ref_y = 0;
                self.calibrating = false;
            }
            let prog = ((self.calib_n * 256) / CALIB_NEED).min(256) as i32;
            self.render_calib(wfb, prog);
            return;
        }

        // --- gravity RELATIVE to the neutral reference ----------------------
        let dead = self.dead_ctr > DEAD_LIMIT;
        let (gx, gy) = if self.ever_live && !dead {
            (
                ((rmx - self.ref_x) / GRAV_DIV).clamp(-GMAX, GMAX),
                ((rmy - self.ref_y) / GRAV_DIV).clamp(-GMAX, GMAX),
            )
        } else {
            (0, DOWN_G)
        };

        // --- jerk → slosh + spray ------------------------------------------
        let last = (self.last_ax, self.last_ay, self.last_az);
        let lrmx = IMU_X_SGN * axis(last, IMU_X_SRC);
        let lrmy = IMU_Y_SGN * axis(last, IMU_Y_SRC);
        let jx = rmx - lrmx;
        let jy = rmy - lrmy;
        let jmag = jx.abs() + jy.abs();
        self.last_ax = acc.0;
        self.last_ay = acc.1;
        self.last_az = acc.2;
        let (sx, sy, spray) = if self.ever_live && !dead && jmag > JERK_TH {
            (
                (jx / GRAV_DIV).clamp(-VMAX, VMAX),
                (jy / GRAV_DIV).clamp(-VMAX, VMAX),
                true,
            )
        } else {
            (0, 0, false)
        };

        // --- rest breathing -------------------------------------------------
        self.phase = self.phase.wrapping_add(1);
        let tri = {
            let p = (self.phase % 512) as i32;
            if p < 256 {
                p
            } else {
                511 - p
            }
        };
        let gmag = gx.abs() + gy.abs();
        let breathe = if gmag < BREATHE_G_TH {
            (((tri - 128) * BREATHE_AMP) >> 8) * (BREATHE_G_TH - gmag) / BREATHE_G_TH
        } else {
            0
        };

        if self.phase % 30 == 0 {
            println!(
                "water: raw=({},{},{}) ref=({},{}) grav=({},{}) live={}",
                acc.0, acc.1, acc.2, self.ref_x, self.ref_y, gx, gy, live as u8
            );
        }

        self.step(gx, gy, sx, sy, breathe, spray, jmag);
        self.render(wfb, elapsed_ms, 256);
    }

    // -----------------------------------------------------------------------
    // Physics
    // -----------------------------------------------------------------------
    fn step(&mut self, gx: i32, gy: i32, sx: i32, sy: i32, breathe: i32, spray: bool, jmag: i32) {
        for i in 0..NW {
            let surf = self.flags[i] & 1 != 0;
            let mut vx = self.vx[i] as i32 + gx + sx;
            let mut vy = self.vy[i] as i32 + gy + sy;
            if surf {
                vy += breathe;
                if spray {
                    let jit = (rng(&mut self.rng) & 63) as i32 - 32;
                    vx += jit;
                    vy -= jmag >> SPRAY_SHR;
                }
            }
            vx = vx.clamp(-VMAX, VMAX);
            vy = vy.clamp(-VMAX, VMAX);
            self.px[i] = (self.px[i] as i32 + vx) as i16;
            self.py[i] = (self.py[i] as i32 + vy) as i16;
            self.vx[i] = vx as i16;
            self.vy[i] = vy as i16;
        }
        self.build_hash();
        self.relax();
        for i in 0..NW {
            self.vx[i] = (((self.vx[i] as i32 * DAMP) / 256).clamp(-VMAX, VMAX)) as i16;
            self.vy[i] = (((self.vy[i] as i32 * DAMP) / 256).clamp(-VMAX, VMAX)) as i16;
        }
        for i in 0..NW {
            self.wall(i);
        }
    }

    fn build_hash(&mut self) {
        for c in 0..=NCELL {
            self.cell_start[c] = 0;
        }
        for i in 0..NW {
            let c = self.cell_of(i);
            self.cell_start[c + 1] += 1;
        }
        for c in 0..NCELL {
            self.cell_start[c + 1] += self.cell_start[c];
        }
        for c in 0..=NCELL {
            self.cursor[c] = self.cell_start[c];
        }
        for i in 0..NW {
            let c = self.cell_of(i);
            let s = self.cursor[c];
            self.order[s as usize] = i as u16;
            self.cursor[c] = s + 1;
        }
    }

    #[inline]
    fn cell_of(&self, i: usize) -> usize {
        let x = ((self.px[i] as i32) >> FP).clamp(0, W - 1);
        let y = ((self.py[i] as i32) >> FP).clamp(0, H - 1);
        let cx = (x / CELL_PX).clamp(0, GRID_W - 1);
        let cy = (y / CELL_PX).clamp(0, GRID_W - 1);
        (cy * GRID_W + cx) as usize
    }

    fn relax(&mut self) {
        for i in 0..NW {
            let xi = (self.px[i] as i32) >> FP;
            let yi = (self.py[i] as i32) >> FP;
            let cx = (xi / CELL_PX).clamp(0, GRID_W - 1);
            let cy = (yi / CELL_PX).clamp(0, GRID_W - 1);
            let vix = self.vx[i] as i32;
            let viy = self.vy[i] as i32;
            let (mut ax, mut ay) = (0i32, 0i32);
            let mut n: u32 = 0;
            let mut count: u32 = 0;

            'scan: for &(ox, oy) in CELL_OFF.iter() {
                let gx = cx + ox;
                let gy = cy + oy;
                if gx < 0 || gx >= GRID_W || gy < 0 || gy >= GRID_W {
                    continue;
                }
                let c = (gy * GRID_W + gx) as usize;
                for s in self.cell_start[c]..self.cell_start[c + 1] {
                    let j = self.order[s as usize] as usize;
                    if j == i {
                        continue;
                    }
                    let dx = xi - ((self.px[j] as i32) >> FP);
                    let dy = yi - ((self.py[j] as i32) >> FP);
                    let d2 = dx * dx + dy * dy;
                    if d2 >= H2 {
                        continue;
                    }
                    n += 1;
                    if d2 == 0 {
                        ax += if i > j { COLO_NUDGE } else { -COLO_NUDGE };
                    } else {
                        let push = PUSH_LUT[d2 as usize] as i32;
                        ax += (push * dx) >> PUSH_SHIFT;
                        ay += (push * dy) >> PUSH_SHIFT;
                        ax += (((self.vx[j] as i32) - vix) * VISC_K) >> 8;
                        ay += (((self.vy[j] as i32) - viy) * VISC_K) >> 8;
                    }
                    count += 1;
                    if count >= K_CAND {
                        break 'scan;
                    }
                }
            }

            self.nbr[i] = n.min(255) as u8;
            let was = self.flags[i] & 1 != 0;
            let is_surf = if was { n < SURF_HI } else { n < SURF_LO };
            self.flags[i] = if is_surf { 1 } else { 0 };
            self.vx[i] = ((vix + (ax >> 1)).clamp(-VMAX, VMAX)) as i16;
            self.vy[i] = ((viy + (ay >> 1)).clamp(-VMAX, VMAX)) as i16;
        }
    }

    fn wall(&mut self, i: usize) {
        let dx = ((self.px[i] as i32) >> FP) - CX;
        let dy = ((self.py[i] as i32) >> FP) - CY;
        let r2 = dx * dx + dy * dy;
        if r2 <= WALL_R2 {
            return;
        }
        let r = isqrt(r2 as u32) as i32;
        self.px[i] = ((CX << FP) + ((dx * WALL_R) << FP) / r) as i16;
        self.py[i] = ((CY << FP) + ((dy * WALL_R) << FP) / r) as i16;
        let vx = self.vx[i] as i32;
        let vy = self.vy[i] as i32;
        let vn = (vx * dx + vy * dy) / r;
        if vn > 0 {
            let k = (vn * (256 + REST_E)) >> 8;
            self.vx[i] = (vx - (k * dx) / r).clamp(-VMAX, VMAX) as i16;
            self.vy[i] = (vy - (k * dy) / r).clamp(-VMAX, VMAX) as i16;
        }
    }

    // -----------------------------------------------------------------------
    // Render — merging glow blobs (density-field look), MAX-blended.
    // -----------------------------------------------------------------------
    fn render(&mut self, wfb: &mut WatchFb, elapsed_ms: u32, alpha: i32) {
        let (mut cx0, mut cy0, mut cx1, mut cy1) = (W, H, 0, 0);
        for i in 0..NW {
            let x = (self.px[i] as i32) >> FP;
            let y = (self.py[i] as i32) >> FP;
            if x < cx0 {
                cx0 = x;
            }
            if x > cx1 {
                cx1 = x;
            }
            if y < cy0 {
                cy0 = y;
            }
            if y > cy1 {
                cy1 = y;
            }
        }
        let cur = (
            (cx0 - SQ_MARGIN).max(0),
            (cy0 - SQ_MARGIN).max(CLOCK_Y1),
            (cx1 + SQ_MARGIN).min(W - 1),
            (cy1 + SQ_MARGIN).min(H - 1),
        );
        let lb = self.last_bbox;
        let er = (
            cur.0.min(lb.0 as i32).max(0),
            cur.1.min((lb.1 as i32).max(CLOCK_Y1)).max(CLOCK_Y1),
            cur.2.max(lb.2 as i32).min(W - 1),
            cur.3.max(lb.3 as i32).min(H - 1),
        );

        let t1 = (elapsed_ms >> 5) as i32;
        let t2 = (elapsed_ms >> 6) as i32;

        {
            let fb = wfb.buf_mut();
            fill_rect_black(fb, er.0, er.1, er.2, er.3);

            for i in 0..NW {
                let x = (self.px[i] as i32) >> FP;
                let y = (self.py[i] as i32) >> FP;
                if y < CLOCK_Y1 {
                    continue;
                }
                let surf = self.flags[i] & 1 != 0;
                let vx = self.vx[i] as i32;
                let vy = self.vy[i] as i32;
                let spd = vx.abs() + vy.abs(); // L1 speed (no per-particle isqrt)
                let au = (lock::NOISE_A[(((x >> 2) + t1) & 255) as usize] as i32
                    + lock::NOISE_B[(((y >> 2) - t2) & 255) as usize] as i32
                    - 256)
                    >> 3;
                let depth = self.nbr[i] as i32 * 3;
                let mut base = (if surf { 210 } else { 150 }) + (spd >> 4) + au - depth;
                base = (base * alpha) >> 8;
                base = base.clamp(0, 255);

                // soft glow blob: centre = base colour, rim fades to indigo;
                // overlapping blobs MAX-MERGE into a continuous luminous body.
                for oy in -BLOB_R..=BLOB_R {
                    let yy = y + oy;
                    if yy < CLOCK_Y1 || yy >= H {
                        continue;
                    }
                    for ox in -BLOB_R..=BLOB_R {
                        let d2 = ox * ox + oy * oy;
                        if d2 > BLOB_R * BLOB_R {
                            continue;
                        }
                        let xx = x + ox;
                        if xx < 0 || xx >= W {
                            continue;
                        }
                        let dith = BAYER4[(yy & 3) as usize][(xx & 3) as usize];
                        let idx = (base - d2 * BLOB_FALLOFF + dith).clamp(0, 255) as usize;
                        let (hi, lo) = WATER_LUT[idx];
                        max565(fb, xx, yy, hi, lo);
                    }
                }
            }
        }

        self.last_bbox = (cur.0 as i16, cur.1 as i16, cur.2 as i16, cur.3 as i16);
        wfb.mark_rect(er.0, er.1, er.2, er.3);
    }

    /// Calibration screen: "HOLD STILL" + a neon progress bar. `prog` 0..=256.
    fn render_calib(&mut self, wfb: &mut WatchFb, prog: i32) {
        let prog = prog.clamp(0, 256);
        {
            let fb = wfb.buf_mut();
            fill_rect_black(fb, 0, 0, W - 1, H - 1);
            // pulsing centre dot (a bead of the liquid)
            let pulse = 150 + ((self.phase as i32 * 4) % 512 - 256).abs() / 4;
            let (chi, clo) = WATER_LUT[pulse.clamp(0, 255) as usize];
            for oy in -4..=4 {
                for ox in -4..=4 {
                    if ox * ox + oy * oy <= 16 {
                        put565(fb, CX + ox, CY - 44 + oy, chi, clo);
                    }
                }
            }
            // headings
            let t1 = "CALIBRATING";
            let w1 = wheel::text_width(t1, &lock::TEXT_GLYPHS);
            wheel::draw_text_at(fb, t1, CX - w1 / 2, CY + 4, 210, &lock::TEXT_GLYPHS);
            let t2 = "HOLD STILL";
            let w2 = wheel::text_width(t2, &lock::TEXT_GLYPHS);
            wheel::draw_text_at(fb, t2, CX - w2 / 2, CY + 34, 120, &lock::TEXT_GLYPHS);
            // progress bar
            let bx = CX - 90;
            let by = CY + 60;
            let filled = bx + (180 * prog) / 256;
            for y in by..by + 6 {
                for x in bx..=bx + 180 {
                    let (hi, lo) = if x <= filled {
                        WATER_LUT[210]
                    } else {
                        WATER_LUT[70]
                    };
                    put565(fb, x, y, hi, lo);
                }
            }
        }
        self.phase = self.phase.wrapping_add(1);
        wfb.mark_rect(0, 0, W - 1, H - 1);
    }
}

// ===========================================================================
// Free helpers
// ===========================================================================
#[inline]
fn axis(a: (i16, i16, i16), s: usize) -> i32 {
    match s {
        0 => a.0 as i32,
        1 => a.1 as i32,
        _ => a.2 as i32,
    }
}

#[inline]
fn rng(s: &mut u32) -> u32 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *s = x;
    x
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

fn fill_rect_black(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    let (x0, x1) = (x0.max(0), x1.min(W - 1));
    if x1 < x0 {
        return;
    }
    for y in y0.max(0)..=y1.min(H - 1) {
        let a = ((y * W + x0) * 2) as usize;
        let b = ((y * W + x1) * 2 + 2) as usize;
        if b <= fb.len() {
            fb[a..b].fill(0);
        }
    }
}

/// SET a pre-baked RGB565-BE pair (over black — divide-free). Clip is the
/// panel only now (no clock band).
#[inline]
fn put565(fb: &mut [u8], x: i32, y: i32, hi: u8, lo: u8) {
    if x < 0 || x >= W || y < 0 || y >= H {
        return;
    }
    let i = ((y * W + x) * 2) as usize;
    if i + 1 < fb.len() {
        fb[i] = hi;
        fb[i + 1] = lo;
    }
}

/// Per-channel MAX of a pre-baked RGB565-BE pair — the merging-blob blend, zero
/// divides.
#[inline]
fn max565(fb: &mut [u8], x: i32, y: i32, hi: u8, lo: u8) {
    if x < 0 || x >= W || y < CLOCK_Y1 || y >= H {
        return;
    }
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 >= fb.len() {
        return;
    }
    let new = ((hi as u16) << 8) | lo as u16;
    let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
    let px = ((old >> 11).max(new >> 11) << 11)
        | (((old >> 5) & 0x3F).max((new >> 5) & 0x3F) << 5)
        | ((old & 0x1F).max(new & 0x1F));
    fb[idx] = (px >> 8) as u8;
    fb[idx + 1] = px as u8;
}
