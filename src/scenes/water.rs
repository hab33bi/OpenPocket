//! Water — tilt-driven liquid simulation (src/scenes/water.rs).
//!
//! Model: a 2-D pairwise-hash particle liquid — ~448 neon-blue squares
//! carrying position + velocity, a uniform spatial hash, and sqrt-free /
//! divide-free short-range repulsion for incompressibility. Chosen by the
//! design workflow as the highest-aggregate 2-axis-capable model that is
//! also the most robust (a plain struct in `apps::State`, no `static mut`,
//! no `unsafe`). Gravity comes from the QMI8658 accelerometer each frame; a
//! flick's jerk throws a breaking-crest spray of real particles that arc and
//! re-absorb. See docs/water/IMPL-SPEC.md for the full rationale + overflow
//! proof, and docs/water/review-*.md for the adversarial findings applied
//! below (all 13; the 2 HIGH — non-monotonic repulsion + unbounded glow
//! divides — and the settle/calibration MEDIUMs are fixed here).
//!
//! Everything in the hot loop is INTEGER / Q6 fixed-point (no FPU).

#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]

use crate::display::watch_fb::WatchFb;
use crate::scenes::lock; // NOISE_A / NOISE_B (pub static [u8;256]) for the aurora drift.
use esp_println::println;

// Build-time tables: WATER_LUT [(u8,u8);256] colour gradient; PUSH_LUT
// [i16;257] repulsion by separation² (monotonic, clamp = i16::MAX);
// GLOW_SPARK / GLOW_LINE [(u8,u8);256] = tint·v RGB565-BE for divide-free glow.
include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));

// --- geometry / vessel -----------------------------------------------------
const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = 233;
const CY: i32 = 233;
/// Interior wall radius: 1 px inside BEZEL_R=223. Hard positional projection
/// keeps every particle at r <= WALL_R. (Drop ~2 px if the bezel edge shows.)
const WALL_R: i32 = 222;
const WALL_R2: i32 = WALL_R * WALL_R;
/// Liquid clipped to y >= CLOCK_Y1 so wheel::draw_status (band ~26..66) stays
/// topmost and is never cleared or overdrawn.
const CLOCK_Y1: i32 = 70;

// --- particle system -------------------------------------------------------
const NW: usize = 448;
const GRID_W: i32 = 30;
const CELL_PX: i32 = 16;
const NCELL: usize = (GRID_W * GRID_W) as usize; // 900
const H_PX: i32 = 16;
const H2: i32 = H_PX * H_PX; // 256 (= PUSH_LUT max index)
/// Candidate cap per particle — bounds the worst-case relax cost.
const K_CAND: u32 = 24;
/// 3×3 neighbour-cell offsets, CENTRE-FIRST (review physics-F3): the nearest,
/// strongest repulsions are always processed before the candidate cap bites.
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

// --- fixed point — Q6 (1/64 px), pos AND vel (integration is a bare add) ---
const FP: i32 = 6;
const VMAX_PX: i32 = 16;
const VMAX: i32 = VMAX_PX << FP; // 1024 Q6 px/frame (velocity clamp / CFL / overflow keystone)

// --- dynamics --------------------------------------------------------------
/// v *= DAMP/256 ≈ 0.992/frame. Applied via /256 (round-toward-zero) NOT >>8
/// (review overflow-F1: an arithmetic shift never decays small negative
/// velocities → the pool crept off-centre and never settled).
const DAMP: i32 = 254;
/// Wall restitution: outward normal speed scaled by (256+REST_E)/256 (e≈0.10).
const REST_E: i32 = 26;
/// Repulsion impulse = (PUSH_LUT[d2]·dcomponent) >> PUSH_SHIFT.
const PUSH_SHIFT: i32 = 7;
/// Viscosity: nudge v_i toward each neighbour's v_j by VISC_K/256 (surface calm).
const VISC_K: i32 = 40;
/// Deterministic separation impulse for an exact co-location (d2==0) — the
/// anti-singularity belt-and-suspenders (review physics-F1 #3).
const COLO_NUDGE: i32 = 96;

// --- IMU → screen mapping + calibration  ***FIRST-FLASH TUNABLES*** ---------
// Measure on-device like the touch Y-flip: log read_accel while tilting each
// screen edge down; pick the two in-plane axes + signs (§6 recipe).
const IMU_X_SRC: usize = 0; // raw axis → screen +x (right): 0=ax 1=ay 2=az
const IMU_X_SGN: i32 = 1; //   flip if "tilt right" runs the water left
const IMU_Y_SRC: usize = 1; // raw axis → screen +y (down)
const IMU_Y_SGN: i32 = 1; //   flip if "tilt down" runs the water up
/// Raw (8192 LSB/g) → Q6 accel divisor: /64 → 128 Q6 = 2 px/frame² at 1 g.
/// Division (round-toward-zero), NOT >>6, so zero-mean rest noise has no DC
/// bias (review overflow-F2).
const GRAV_DIV: i32 = 64;
const GMAX: i32 = 512; // ±4 g planar clamp (Q6 accel)
const DOWN_G: i32 = 128; // dead-IMU down-vector (1 g)
/// Rest-bias clamp: ±0.1 g (~820 LSB). Removes only the sensor zero offset —
/// can never cancel a real tilt.
const REST_BIAS_CLAMP: i32 = 820;
/// Consecutive faulted reads after which a once-live IMU snaps to the fixed
/// down-vector (~1 s at 40 fps) instead of freezing at the last tilt (imu-F3).
const DEAD_LIMIT: u16 = 40;

// --- jerk → whole-body slosh + surface-lift spray --------------------------
const JERK_TH: i32 = 2600; // |jx|+|jy| above this is a flick (~0.32 g)
const SPRAY_SHR: i32 = 8; // surface up-kick = jmag >> 8

// --- rest breathing (alive at rest; fades out as |g| grows) ----------------
const BREATHE_G_TH: i32 = 40;
const BREATHE_AMP: i32 = 48;

// --- surface detection (meniscus) with hysteresis --------------------------
const SURF_LO: u32 = 3;
const SURF_HI: u32 = 5;

// --- render ----------------------------------------------------------------
const SQ: i32 = 5; // body square side (SET over black → overlaps merge into a sheet)
const SQ_MARGIN: i32 = 4; // clear/bbox half-extent (square + r3 glow)
const NMENISC: usize = 64; // meniscus-line columns
/// Bounded glow budget per frame (review perf-F1 #2): cap surface sparkles so
/// render cost never scales with airborne spray count.
const GLOW_BUDGET: i32 = 72;
/// 4×4 Bayer (~±7) added to the LUT INDEX only (lit pixels) — kills RGB565
/// banding without touching the black background.
const BAYER4: [[i32; 4]; 4] = [
    [-8, 0, -6, 2],
    [4, -4, 6, -2],
    [-5, 3, -7, 1],
    [7, -1, 5, -3],
];

// ===========================================================================
// State — all sim arrays. Held as `pub wa: Water` in apps::State (internal
// SRAM, random-access every frame; NOT the PSRAM framebuffer). ~8.9 KB,
// const-initializable. No static mut, no unsafe.
// ===========================================================================
pub struct Water {
    px: [i16; NW],
    py: [i16; NW],
    vx: [i16; NW],
    vy: [i16; NW],
    nbr: [u8; NW],
    flags: [u8; NW], // bit0 = surface (hysteresis-latched)
    cell_start: [u16; NCELL + 1],
    cursor: [u16; NCELL + 1],
    order: [u16; NW],
    surf_top: [i16; NMENISC],
    bias_x: i32,
    bias_y: i32,
    last_ax: i16,
    last_ay: i16,
    last_az: i16,
    rng: u32,
    phase: u32,
    last_bbox: (i16, i16, i16, i16),
    dead_ctr: u16,
    seeded: bool,
    ever_live: bool,
    need_calib: bool,
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
            surf_top: [0; NMENISC],
            bias_x: 0,
            bias_y: 0,
            last_ax: 0,
            last_ay: 0,
            last_az: 0,
            rng: 0x2545_F491,
            phase: 0,
            last_bbox: (0, 0, -1, -1),
            dead_ctr: 0,
            seeded: false,
            ever_live: false,
            need_calib: false,
        }
    }

    /// Spawn the pool + defer calibration to the first live tick.
    pub fn open(&mut self) {
        self.seed();
        self.need_calib = true;
        self.dead_ctr = 0;
        self.last_bbox = (0, 0, -1, -1);
    }

    /// Spawn NW particles in a shallow low-centre block, at rest. Gravity +
    /// repulsion level them into a pool over the first ~10 frames.
    fn seed(&mut self) {
        self.rng ^= 0x9E37_79B9;
        const COLS: i32 = 32;
        const SPACING: i32 = 7;
        let cx0 = CX - (COLS * SPACING) / 2;
        let cy0 = CY + 40;
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

    /// Capture the rest bias on the first LIVE tick (±0.1 g) and seed the jerk
    /// history so the first frame's jerk is exactly zero (review imu-F1/F2 +
    /// physics-F2: a faulted first read no longer permanently disables calib,
    /// and opening the watch while tilted no longer sprays the calm pool).
    fn calibrate(&mut self, acc: (i16, i16, i16), live: bool) {
        if live {
            let bx = IMU_X_SGN * axis(acc, IMU_X_SRC);
            let by = IMU_Y_SGN * axis(acc, IMU_Y_SRC);
            self.bias_x = bx.clamp(-REST_BIAS_CLAMP, REST_BIAS_CLAMP);
            self.bias_y = by.clamp(-REST_BIAS_CLAMP, REST_BIAS_CLAMP);
            self.last_ax = acc.0; // first jerk = mapped(acc) − mapped(acc) = 0
            self.last_ay = acc.1;
            self.last_az = acc.2;
            self.ever_live = true;
            self.need_calib = false; // cleared ONLY on a live sample; else retry next frame
        }
    }

    /// Reveal-frame render (fill-in on open): the seeded pool fades up from
    /// black at alpha q. No physics — the first `tick` takes over.
    pub fn reveal(&mut self, wfb: &mut WatchFb, q_q8: i32, elapsed_ms: u32) {
        if !self.seeded {
            self.open();
        }
        self.render(wfb, elapsed_ms, q_q8.clamp(0, 256));
    }

    /// Per-frame entry. The run loop owns i2c and has just read the accel;
    /// `imu = None` on a bus fault.
    pub fn tick(&mut self, wfb: &mut WatchFb, imu: Option<(i16, i16, i16)>, elapsed_ms: u32) {
        let acc = imu.unwrap_or((self.last_ax, self.last_ay, self.last_az));
        let live = imu.is_some();
        if live {
            self.dead_ctr = 0;
        } else {
            self.dead_ctr = self.dead_ctr.saturating_add(1);
        }

        if !self.seeded {
            self.open();
        }
        if self.need_calib {
            self.calibrate(acc, live);
        }

        // Mapped raw kept in i32 (never a sign-flipped i16 — the -1·-32768 hole).
        let rmx = IMU_X_SGN * axis(acc, IMU_X_SRC);
        let rmy = IMU_Y_SGN * axis(acc, IMU_Y_SRC);

        // Gravity (Q6 accel), rest-bias removed, /64 round-toward-zero, clamped.
        let dead = self.dead_ctr > DEAD_LIMIT;
        let (gx, gy) = if (live || self.ever_live) && !dead {
            (
                ((rmx - self.bias_x) / GRAV_DIV).clamp(-GMAX, GMAX),
                ((rmy - self.bias_y) / GRAV_DIV).clamp(-GMAX, GMAX),
            )
        } else {
            (0, DOWN_G) // never-live or long-dead: pool falls to screen-bottom
        };

        // Jerk (i32) → whole-body slosh + spray gate. `last_*` was seeded to
        // the first live sample by calibrate(), so the first jerk is zero.
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

        // Rest breathing, scaled down as |g| grows.
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

        // IMU axis-mapping instrumentation (BUILD-ORDER step 2 — remove once
        // the IMU_* consts are baked): raw accel + mapped screen gravity,
        // throttled. Flat → gx,gy ≈ 0; tilt an edge down → that screen axis
        // grows and the water should run that way.
        if self.phase % 20 == 0 {
            println!(
                "water: raw=({},{},{}) grav=({},{}) live={}",
                acc.0, acc.1, acc.2, gx, gy, live as u8
            );
        }

        self.step(gx, gy, sx, sy, breathe, spray, jmag);
        self.render(wfb, elapsed_ms, 256);
    }

    // -----------------------------------------------------------------------
    // Physics: integrate → hash → relax → damp → wall. All Q6 integer.
    // -----------------------------------------------------------------------
    fn step(&mut self, gx: i32, gy: i32, sx: i32, sy: i32, breathe: i32, spray: bool, jmag: i32) {
        // 1) integrate + advect (semi-implicit Euler)
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

        // 2) rebuild the hash on the NEW positions (relax search is then exact)
        self.build_hash();

        // 3) relax: repulsion (incompressibility) + viscosity + surface flag
        self.relax();

        // 4) damp THEN clamp — round-toward-zero /256 so BOTH signs decay
        for i in 0..NW {
            self.vx[i] = (((self.vx[i] as i32 * DAMP) / 256).clamp(-VMAX, VMAX)) as i16;
            self.vy[i] = (((self.vy[i] as i32 * DAMP) / 256).clamp(-VMAX, VMAX)) as i16;
        }

        // 5) round wall — hard positional projection (no leak) + damped reflect
        for i in 0..NW {
            self.wall(i);
        }
    }

    /// CSR count-sort into `order` keyed by 16 px cell. O(2N + NCELL).
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

    /// Incompressibility: for each particle scan the 3×3 block CENTRE-FIRST;
    /// repulsion (sqrt-free PUSH_LUT) pushes off crowded neighbours, viscosity
    /// smooths the surface, the in-radius count drives the hysteresis surface
    /// flag. Candidate-capped.
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
                        // exact co-location: deterministic parity separation
                        ax += if i > j { COLO_NUDGE } else { -COLO_NUDGE };
                    } else {
                        // repulsion — monotonic, 1/d + falloff baked in
                        let push = PUSH_LUT[d2 as usize] as i32;
                        ax += (push * dx) >> PUSH_SHIFT;
                        ay += (push * dy) >> PUSH_SHIFT;
                        // viscosity — pull toward the neighbour's velocity
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
            // half the impulse to i (each pair seen from both ends) + clamp
            self.vx[i] = ((vix + (ax >> 1)).clamp(-VMAX, VMAX)) as i16;
            self.vy[i] = ((viy + (ay >> 1)).clamp(-VMAX, VMAX)) as i16;
        }
    }

    /// Round wall: hard positional projection onto the rim (leak impossible) +
    /// damped normal reflect. Velocity re-clamped so the documented ±VMAX
    /// invariant actually holds at render time (review overflow-F3).
    fn wall(&mut self, i: usize) {
        let dx = ((self.px[i] as i32) >> FP) - CX;
        let dy = ((self.py[i] as i32) >> FP) - CY;
        let r2 = dx * dx + dy * dy;
        if r2 <= WALL_R2 {
            return;
        }
        let r = isqrt(r2 as u32) as i32; // r >= WALL_R
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
    // Render: clear the tracked union bbox, splat SET squares through the LUT
    // (index-dithered, aurora-shaded), bounded surface sparkle, meniscus line.
    // `alpha` = reveal fade (256 normal).
    // -----------------------------------------------------------------------
    fn render(&mut self, wfb: &mut WatchFb, elapsed_ms: u32, alpha: i32) {
        // this frame's TIGHT bbox
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
        let mut glow_budget = GLOW_BUDGET;

        {
            let fb = wfb.buf_mut();
            fill_rect_black(fb, er.0, er.1, er.2, er.3);

            for c in self.surf_top.iter_mut() {
                *c = i16::MAX;
            }

            for i in 0..NW {
                let x = (self.px[i] as i32) >> FP;
                let y = (self.py[i] as i32) >> FP;
                if y < CLOCK_Y1 {
                    continue;
                }
                let surf = self.flags[i] & 1 != 0;
                let vx = self.vx[i] as i32;
                let vy = self.vy[i] as i32;
                // L1 speed (review perf-F2): zero divides, drives the brightness cue.
                let spd = vx.abs() + vy.abs();
                let au = (lock::NOISE_A[(((x >> 2) + t1) & 255) as usize] as i32
                    + lock::NOISE_B[(((y >> 2) - t2) & 255) as usize] as i32
                    - 256)
                    >> 3;
                let depth = self.nbr[i] as i32 * 3;
                let mut base = (if surf { 206 } else { 150 }) + (spd >> 4) + au - depth;
                base = (base * alpha) >> 8;
                base = base.clamp(0, 255);

                // SET 5×5 body square, index-dithered (divide-free per pixel)
                let x0 = x - SQ / 2;
                let y0 = y - SQ / 2;
                for oy in 0..SQ {
                    let yy = y0 + oy;
                    if yy < CLOCK_Y1 || yy >= H {
                        continue;
                    }
                    for ox in 0..SQ {
                        let xx = x0 + ox;
                        if xx < 0 || xx >= W {
                            continue;
                        }
                        let idx = (base + BAYER4[(yy & 3) as usize][(xx & 3) as usize])
                            .clamp(0, 255) as usize;
                        let (hi, lo) = WATER_LUT[idx];
                        put565(fb, xx, yy, hi, lo);
                    }
                }

                // bounded surface sparkle (baked-table glow, zero divides)
                if surf && alpha >= 200 && glow_budget > 0 {
                    soft_glow(fb, x, y, 3, &GLOW_SPARK, 150);
                    glow_budget -= 1;
                }

                // track column top for the continuous meniscus line
                let col = ((x * NMENISC as i32) / W).clamp(0, NMENISC as i32 - 1) as usize;
                if (y as i16) < self.surf_top[col] {
                    self.surf_top[col] = y as i16;
                }
            }

            if alpha >= 200 {
                draw_meniscus_line(fb, &self.surf_top);
            }
        }

        self.last_bbox = (cur.0 as i16, cur.1 as i16, cur.2 as i16, cur.3 as i16);
        wfb.mark_rect(er.0, er.1, er.2, er.3);
    }
}

// ===========================================================================
// Free helpers (self-contained integer routines).
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

/// SET a pre-baked RGB565-BE pair (over black — divide-free). Clips clock+panel.
#[inline]
fn put565(fb: &mut [u8], x: i32, y: i32, hi: u8, lo: u8) {
    if x < 0 || x >= W || y < CLOCK_Y1 || y >= H {
        return;
    }
    let i = ((y * W + x) * 2) as usize;
    if i + 1 < fb.len() {
        fb[i] = hi;
        fb[i + 1] = lo;
    }
}

/// Per-channel MAX of a PRE-BAKED RGB565-BE pair against the framebuffer —
/// zero divides (the review perf-F1 fix; the tint·v tables are baked in
/// build.rs). Glow that never self-stacks.
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

/// Small soft radial glow (quadratic falloff, one divide/pixel), coloured via
/// a baked table, MAX-blended — surface sparkle.
fn soft_glow(fb: &mut [u8], cx: i32, cy: i32, r: i32, table: &[(u8, u8); 256], vpeak: i32) {
    let r2 = r * r;
    let denom = (r2 * r2).max(1);
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let f = r2 - d2;
            let v = (vpeak * f * f / denom).clamp(0, 255) as usize;
            let (hi, lo) = table[v];
            max565(fb, cx + dx, cy + dy, hi, lo);
        }
    }
}

/// Connect the per-column surface tops into one bright, tilting cyan line laid
/// over the swarm (2 px, MAX-blended, baked colour). Empty columns break it.
fn draw_meniscus_line(fb: &mut [u8], surf_top: &[i16; NMENISC]) {
    let (hi, lo) = GLOW_LINE[210];
    let colw = W / NMENISC as i32;
    let mut prev: Option<(i32, i32)> = None;
    for c in 0..NMENISC {
        let ty = surf_top[c];
        if ty == i16::MAX {
            prev = None;
            continue;
        }
        let xc = c as i32 * colw + colw / 2;
        let yc = (ty as i32).max(CLOCK_Y1);
        if let Some((x0, y0)) = prev {
            line_max565(fb, x0, y0, xc, yc, hi, lo);
        }
        prev = Some((xc, yc));
    }
}

/// 2 px MAX line between two points — fixed-point DDA (one divide total, not
/// per step: review perf-F1 #3).
fn line_max565(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, hi: u8, lo: u8) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    let sx = ((x1 - x0) << 8) / steps;
    let sy = ((y1 - y0) << 8) / steps;
    let mut xf = x0 << 8;
    let mut yf = y0 << 8;
    for _ in 0..=steps {
        let x = xf >> 8;
        let y = yf >> 8;
        max565(fb, x, y, hi, lo);
        max565(fb, x, y + 1, hi, lo);
        xf += sx;
        yf += sy;
    }
}
