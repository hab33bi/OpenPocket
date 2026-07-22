//! Water — tilt-driven liquid simulation (SYNTHESIS build, src/scenes/water.rs).
//!
//! Chosen model: a **2-D pairwise-hash particle liquid** (the realism
//! runner-up and the highest-aggregate model that satisfies the 2-axis
//! product goal), grafted with every judge must-adopt that fits the particle
//! structure:
//!   - PUSH_LUT sqrt-free/divide-free repulsion  (pairwise + overflow judges)
//!   - viscosity term to kill the grainy surface  (pairwise, "best surface idea")
//!   - SET-overlap body squares -> solid dithered sheet, max_px only for glow
//!   - per-particle surface detection WITH hysteresis (sparkly meniscus)
//!   - a continuous meniscus water LINE laid over the swarm  (heightfield)
//!   - DAMP ~ 0.992 + emergent surface-lift spray  (flip-lite)
//!   - rest-bias clamp +-0.1 g so a flat watch breathes, tilt always dominates
//!   - VMAX clamp AFTER damping + hard positional wall projection (no leak)
//!   - correct SHRINKING damage bbox (last = this tight; mark union)
//!   - pre-baked RGB565-BE WATER_LUT, index-only Bayer dither, aurora drift
//!
//! Everything in the hot loop is INTEGER / Q6 fixed-point (no FPU). State is a
//! normal struct held as a field in `apps::State` (internal SRAM, ~8.9 KB) —
//! no `static mut`, no `unsafe`. See docs/water/IMPL-SPEC.md for the full
//! rationale, overflow proof, and build order.
//!
//! This is the reference draft: real bodies, self-contained (duplicates the
//! tiny integer helpers so it compiles beside apps.rs). When merged it can
//! call apps.rs's identical `isqrt`/`fill_rect_black`/`max_px` instead.

#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]

use crate::display::watch_fb::WatchFb;
use crate::scenes::lock; // NOISE_A / NOISE_B live here (pub static [u8;256]).

// Build-time colour + repulsion tables (build.rs::generate_water_lut, §5f of
// the spec). WATER_LUT: [(u8,u8);256] RGB565-BE. PUSH_LUT: [i16;257] indexed
// by separation-squared d2, bakes STIFF*(H-d)/d clamped -> sqrt-free & /-free.
include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));

// ---------------------------------------------------------------------------
// Geometry / vessel
// ---------------------------------------------------------------------------
const W: i32 = 466;
const H: i32 = 466;
const CX: i32 = 233;
const CY: i32 = 233;
/// Interior wall radius: 1 px inside BEZEL_R=223 so a 5 px square never bleeds
/// the bezel. Hard positional projection keeps every particle at r <= WALL_R.
const WALL_R: i32 = 222;
const WALL_R2: i32 = WALL_R * WALL_R;
/// Liquid is clipped to y >= CLOCK_Y1 so wheel::draw_status (band ~26..66)
/// stays topmost and is never cleared or overdrawn.
const CLOCK_Y1: i32 = 70;

// ---------------------------------------------------------------------------
// Particle system
// ---------------------------------------------------------------------------
/// Particle count. Denser than pairwise's 320 (reads as a full sheet, not
/// dots); short of flip-lite's 640 by necessity — pairwise relax cost scales
/// with local density, and 448 + the candidate cap keeps the worst-slosh
/// frame inside the 25 ms cadence (see the budget in the spec).
const NW: usize = 448;
/// Spatial hash: cells of CELL_PX >= H_PX so all in-radius neighbours fall in
/// the 3x3 block by construction. 30x30 covers the 466 px face.
const GRID_W: i32 = 30;
const CELL_PX: i32 = 16;
const NCELL: usize = (GRID_W * GRID_W) as usize; // 900
/// Interaction radius (px). Finer than flip-lite's/pic-grid's 20 px grid.
const H_PX: i32 = 16;
const H2: i32 = H_PX * H_PX; // 256  (also the max PUSH_LUT index)
/// Candidate cap per particle: bounds the relax cost when a cell is dense
/// (short-range repulsion is dominated by the nearest neighbours, so capping
/// the tail is visually harmless and makes the worst-case frame provable).
const K_CAND: u32 = 24;

// ---------------------------------------------------------------------------
// Fixed point — Q6 (1/64 px). Position AND velocity share Q6 so integration
// is a bare add. i16 storage is proven safe by the VMAX clamp: worst on-screen
// coord = CX + WALL_R + VMAX_PX = 233+222+16 = 471 px -> 471<<6 = 30144 < 32767.
// ---------------------------------------------------------------------------
const FP: i32 = 6;
const VMAX_PX: i32 = 16;
const VMAX: i32 = VMAX_PX << FP; // 1024 Q6 px/frame  (velocity hard clamp / CFL)

// ---------------------------------------------------------------------------
// Dynamics
// ---------------------------------------------------------------------------
/// v *= DAMP/256 ~= 0.992 per frame — flip-lite's value; bleeds energy so the
/// pool always settles and never runs perpetually.
const DAMP: i32 = 254;
/// Wall restitution: outward normal speed is scaled by (256+REST_E)/256 on the
/// reflect (e ~ 0.10) — energy loss, no perpetual bounce.
const REST_E: i32 = 26;
/// Repulsion impulse = (PUSH_LUT[d2]*dcomponent) >> PUSH_SHIFT.
const PUSH_SHIFT: i32 = 7;
/// Viscosity: nudge v_i toward each neighbour's v_j by VISC_K/256 — smooths the
/// free surface and kills particle boil (pairwise's best-in-class surface cure).
const VISC_K: i32 = 40;

// ---------------------------------------------------------------------------
// IMU -> screen mapping + calibration  ***FIRST-FLASH TUNABLES***
// Measure on-device exactly like the touch Y-flip: log read_accel while
// tilting each screen edge down; pick the two in-plane axes + signs so
// "tilt top-edge down" -> gravity points to screen-top (§ IMU recipe).
// ---------------------------------------------------------------------------
const IMU_X_SRC: usize = 0; // which raw axis feeds screen +x (right): 0=ax 1=ay 2=az
const IMU_X_SGN: i32 = 1; //   flip if "tilt right" runs the water left
const IMU_Y_SRC: usize = 1; // which raw axis feeds screen +y (down)
const IMU_Y_SGN: i32 = 1; //   flip if "tilt down" runs the water up
/// Raw (8192 LSB/g) -> Q6 accel: >>6 gives 128 Q6 = 2 px/frame^2 at 1 g.
const GRAV_SHR: i32 = 6;
/// Gravity clamp (Q6 accel). 512 = 4 g planar — matches the +-4 g full scale.
const GMAX: i32 = 512;
/// Fixed down-vector accel used when the IMU is dead (1 g down = 128 Q6).
const DOWN_G: i32 = 128;
/// Rest-bias clamp: +-0.1 g (0.1*8192 ~= 820 LSB). Removes only the sensor's
/// zero offset — can NEVER cancel a real tilt, so a flat watch reads in-plane
/// g ~ 0 and breathes, while any tilt dominates.
const REST_BIAS_CLAMP: i32 = 820;
const ACC_LSB_PER_G: i32 = 8192;

// ---------------------------------------------------------------------------
// Jerk -> whole-body slosh + surface-lift spray
// ---------------------------------------------------------------------------
/// |jerk_x|+|jerk_y| (raw LSB) above this is a flick (~0.32 g). Below it, no
/// slosh (sensor noise never sprays).
const JERK_TH: i32 = 2600;
/// Surface particles are flung up by (jmag >> SPRAY_SHR) on a flick — they arc
/// under gravity and re-absorb into the pool (emergent spray, same particles).
const SPRAY_SHR: i32 = 8;

// ---------------------------------------------------------------------------
// Rest breathing (alive-at-rest), amplitude scaled DOWN as |g| grows so it
// never fights a real tilt.
// ---------------------------------------------------------------------------
/// Breathing fades to zero once planar |g| exceeds this (Q6 accel; ~0.3 g).
const BREATHE_G_TH: i32 = 40;
/// Peak breathing accel (Q6). Small — a shimmer, not motion.
const BREATHE_AMP: i32 = 48;

// ---------------------------------------------------------------------------
// Surface detection (meniscus) with hysteresis to stop flicker.
// A particle is "surface" when its neighbour count drops below SURF_LO; it
// only stops being surface once the count climbs back above SURF_HI.
// ---------------------------------------------------------------------------
const SURF_LO: u32 = 3;
const SURF_HI: u32 = 5;

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------
/// Body square side (SET over black). 5 px so dense particles overlap into a
/// solid dithered sheet; sparse surface particles read as individual sparkles.
const SQ: i32 = 5;
/// Damage/clear half-extent per particle (covers the 5 px square + r3 glow).
const SQ_MARGIN: i32 = 4;
/// Meniscus line resolution (screen columns).
const NMENISC: usize = 64;
/// 4x4 Bayer matrix (~ +-7) added to the LUT INDEX only (lit pixels), never
/// the channels — kills RGB565 banding on the gradient (build.rs doctrine).
const BAYER4: [[i32; 4]; 4] = [
    [-8, 0, -6, 2],
    [4, -4, 6, -2],
    [-5, 3, -7, 1],
    [7, -1, 5, -3],
];

// ===========================================================================
// State — all simulation arrays. Held as `pub wa: Water` in apps::State
// (internal SRAM, random-access every frame; NOT the PSRAM framebuffer).
// ~8.9 KB, const-initializable. No static mut, no unsafe.
// ===========================================================================
pub struct Water {
    // --- particles (SoA), Q6 ---
    px: [i16; NW],  // position x   (Q6 px)
    py: [i16; NW],  // position y   (Q6 px)
    vx: [i16; NW],  // velocity x   (Q6 px/frame)
    vy: [i16; NW],  // velocity y   (Q6 px/frame)
    nbr: [u8; NW],  // in-radius neighbour count (capped) — surface/depth cue
    flags: [u8; NW], // bit0 = surface (hysteresis-latched)
    // --- spatial hash (CSR count-sort) ---
    cell_start: [u16; NCELL + 1], // per-cell run starts (intact during relax)
    cursor: [u16; NCELL + 1],     // scatter cursor (copy of cell_start)
    order: [u16; NW],             // particle indices sorted by cell
    // --- meniscus line: topmost particle screen-y per column ---
    surf_top: [i16; NMENISC],
    // --- calibration / IMU history ---
    bias_x: i32,
    bias_y: i32,
    last_ax: i16,
    last_ay: i16,
    last_az: i16,
    // --- housekeeping ---
    rng: u32,
    phase: u32,
    last_bbox: (i16, i16, i16, i16), // previous frame's TIGHT bbox (shrinking)
    seeded: bool,
    ever_live: bool,   // has the IMU ever answered? (dead-IMU fallback)
    need_calib: bool,  // capture rest bias on the first live tick
}

impl Water {
    /// Const initializer so `apps::State::new()` stays a `const fn`.
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
            rng: 0x2545_F491, // non-zero xorshift seed
            phase: 0,
            last_bbox: (0, 0, -1, -1),
            seeded: false,
            ever_live: false,
            need_calib: false,
        }
    }

    /// Called from `open_app` (or the first `draw_reveal`): spawn the pool and
    /// defer calibration to the first live tick. Idempotent per open.
    pub fn open(&mut self) {
        self.seed();
        self.need_calib = true;
        self.last_bbox = (0, 0, -1, -1);
    }

    /// Spawn NW particles in a shallow block low-center, at rest. Gravity +
    /// repulsion level them into a pool over the first ~10 frames (a couple of
    /// settling ripples read as "the pool pours in").
    fn seed(&mut self) {
        self.rng ^= 0x9E37_79B9;
        const COLS: i32 = 32; // wide, shallow block
        const SPACING: i32 = 7; // px
        let cx0 = CX - (COLS * SPACING) / 2;
        let cy0 = CY + 40; // sits in the lower cap
        for i in 0..NW {
            let col = i as i32 % COLS;
            let row = i as i32 / COLS;
            let mut x = cx0 + col * SPACING + (rng(&mut self.rng) & 3) as i32;
            let mut y = cy0 + row * SPACING;
            // keep the spawn strictly inside the wall
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

    /// Capture the rest bias on the first live tick (clamped +-0.1 g). If the
    /// IMU is dead we leave bias at zero and use the down-vector fallback.
    fn calibrate(&mut self, acc: (i16, i16, i16), live: bool) {
        if live {
            let bx = IMU_X_SGN * axis(acc, IMU_X_SRC);
            let by = IMU_Y_SGN * axis(acc, IMU_Y_SRC);
            self.bias_x = bx.clamp(-REST_BIAS_CLAMP, REST_BIAS_CLAMP);
            self.bias_y = by.clamp(-REST_BIAS_CLAMP, REST_BIAS_CLAMP);
            self.ever_live = true;
        }
        self.need_calib = false;
    }

    /// Reveal-frame render (fill-in on open): draw the seeded pool at alpha q
    /// (0..=256) so the liquid fades up from black as the morph completes. No
    /// physics — the first `tick` takes over.
    pub fn reveal(&mut self, wfb: &mut WatchFb, q_q8: i32, elapsed_ms: u32) {
        if !self.seeded {
            self.open();
        }
        self.render(wfb, elapsed_ms, q_q8.clamp(0, 256));
    }

    /// The per-frame entry, called from the run loop (which owns i2c and has
    /// just read the accelerometer). `imu = None` on a bus fault.
    pub fn tick(&mut self, wfb: &mut WatchFb, imu: Option<(i16, i16, i16)>, elapsed_ms: u32) {
        // Reuse the last sample on a bus fault so a dropped read never jolts
        // the pool.
        let acc = imu.unwrap_or((self.last_ax, self.last_ay, self.last_az));
        let live = imu.is_some();

        if !self.seeded {
            self.open();
        }
        if self.need_calib {
            self.calibrate(acc, live);
        }

        // Mapped raw kept in i32 — NEVER store a sign-flipped raw in i16
        // (-1 * -32768 overflows i16; this is pic-grid's bug, avoided here).
        let rmx = IMU_X_SGN * axis(acc, IMU_X_SRC);
        let rmy = IMU_Y_SGN * axis(acc, IMU_Y_SRC);

        // --- gravity (Q6 accel), rest-bias removed, clamped ------------------
        let (gx, gy) = if live || self.ever_live {
            (
                ((rmx - self.bias_x) >> GRAV_SHR).clamp(-GMAX, GMAX),
                ((rmy - self.bias_y) >> GRAV_SHR).clamp(-GMAX, GMAX),
            )
        } else {
            (0, DOWN_G) // dead IMU: pool still falls "down" and settles
        };

        // --- jerk (i32) -> whole-body slosh + spray gate --------------------
        let lrmx = IMU_X_SGN * axis((self.last_ax, self.last_ay, self.last_az), IMU_X_SRC);
        let lrmy = IMU_Y_SGN * axis((self.last_ax, self.last_ay, self.last_az), IMU_Y_SRC);
        let jx = rmx - lrmx;
        let jy = rmy - lrmy;
        let jmag = jx.abs() + jy.abs();
        self.last_ax = acc.0;
        self.last_ay = acc.1;
        self.last_az = acc.2;
        let (sx, sy, spray) = if self.ever_live && jmag > JERK_TH {
            (
                (jx >> GRAV_SHR).clamp(-VMAX, VMAX),
                (jy >> GRAV_SHR).clamp(-VMAX, VMAX),
                true,
            )
        } else {
            (0, 0, false)
        };

        // --- rest breathing, scaled down as |g| grows -----------------------
        self.phase = self.phase.wrapping_add(1);
        let tri = {
            let p = (self.phase % 512) as i32;
            if p < 256 {
                p
            } else {
                511 - p
            }
        }; // 0..255 triangle
        let gmag = gx.abs() + gy.abs();
        let breathe = if gmag < BREATHE_G_TH {
            (((tri - 128) * BREATHE_AMP) >> 8) * (BREATHE_G_TH - gmag) / BREATHE_G_TH
        } else {
            0
        };

        self.step(gx, gy, sx, sy, breathe, spray, jmag);
        self.render(wfb, elapsed_ms, 256);
    }

    // -----------------------------------------------------------------------
    // Physics core: integrate -> hash -> relax (repulsion+viscosity+surface)
    //              -> damp -> wall. All Q6 integer.
    // -----------------------------------------------------------------------
    fn step(&mut self, gx: i32, gy: i32, sx: i32, sy: i32, breathe: i32, spray: bool, jmag: i32) {
        // 1) integrate forces + advect (semi-implicit Euler)
        for i in 0..NW {
            let surf = self.flags[i] & 1 != 0;
            let mut vx = self.vx[i] as i32 + gx + sx;
            let mut vy = self.vy[i] as i32 + gy + sy;
            if surf {
                vy += breathe; // rest shimmer
                if spray {
                    // surface-lift: fling up + jitter -> a breaking crest that
                    // arcs and re-absorbs (emergent, same particles).
                    let jit = (rng(&mut self.rng) & 63) as i32 - 32;
                    vx += jit;
                    vy -= jmag >> SPRAY_SHR;
                }
            }
            vx = vx.clamp(-VMAX, VMAX);
            vy = vy.clamp(-VMAX, VMAX);
            self.px[i] = (self.px[i] as i32 + vx) as i16; // Q6 add
            self.py[i] = (self.py[i] as i32 + vy) as i16;
            self.vx[i] = vx as i16;
            self.vy[i] = vy as i16;
        }

        // 2) rebuild the hash on the NEW positions (relax neighbour search is
        //    then exact — every within-radius particle is in the 3x3 block).
        self.build_hash();

        // 3) relax: incompressibility (repulsion) + viscosity + surface flag
        self.relax();

        // 4) damp, THEN clamp — the keystone: energy is strictly dissipative
        //    and bounded every frame (blow-up impossible).
        for i in 0..NW {
            self.vx[i] = (((self.vx[i] as i32 * DAMP) >> 8).clamp(-VMAX, VMAX)) as i16;
            self.vy[i] = (((self.vy[i] as i32 * DAMP) >> 8).clamp(-VMAX, VMAX)) as i16;
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
            self.cell_start[c + 1] += 1; // count into c+1
        }
        for c in 0..NCELL {
            self.cell_start[c + 1] += self.cell_start[c]; // prefix sum
        }
        for c in 0..=NCELL {
            self.cursor[c] = self.cell_start[c]; // scatter cursor
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

    /// The incompressibility pass. For each particle, scan the 3x3 cell block:
    /// repulsion (PUSH_LUT, sqrt-free) pushes it off crowded neighbours,
    /// viscosity nudges its velocity toward theirs (surface smoothing), and the
    /// in-radius count drives the hysteresis surface flag. Candidate-capped.
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

            'scan: for gy in (cy - 1).max(0)..=(cy + 1).min(GRID_W - 1) {
                for gx in (cx - 1).max(0)..=(cx + 1).min(GRID_W - 1) {
                    let c = (gy * GRID_W + gx) as usize;
                    for s in self.cell_start[c]..self.cell_start[c + 1] {
                        let j = self.order[s as usize] as usize;
                        if j == i {
                            continue;
                        }
                        let dx = xi - ((self.px[j] as i32) >> FP);
                        let dy = yi - ((self.py[j] as i32) >> FP);
                        let d2 = dx * dx + dy * dy;
                        if d2 >= H2 || d2 == 0 {
                            continue;
                        }
                        n += 1;
                        // repulsion — 1/d + falloff baked in; no sqrt, no divide
                        let push = PUSH_LUT[d2 as usize] as i32;
                        ax += (push * dx) >> PUSH_SHIFT;
                        ay += (push * dy) >> PUSH_SHIFT;
                        // viscosity — pull toward the neighbour's velocity
                        ax += (((self.vx[j] as i32) - vix) * VISC_K) >> 8;
                        ay += (((self.vy[j] as i32) - viy) * VISC_K) >> 8;
                        count += 1;
                        if count >= K_CAND {
                            break 'scan; // bound the worst-case cost
                        }
                    }
                }
            }

            self.nbr[i] = n.min(255) as u8;
            // hysteresis surface latch (stops meniscus flicker)
            let was = self.flags[i] & 1 != 0;
            let is_surf = if was { n < SURF_HI } else { n < SURF_LO };
            self.flags[i] = if is_surf { 1 } else { 0 };
            // half the impulse to i (each pair is seen from both ends) + clamp
            self.vx[i] = ((vix + (ax >> 1)).clamp(-VMAX, VMAX)) as i16;
            self.vy[i] = ((viy + (ay >> 1)).clamp(-VMAX, VMAX)) as i16;
        }
    }

    /// Round-wall boundary: hard positional projection onto the rim (leak
    /// impossible, independent of the reflect) + damped normal reflection
    /// (water slides along the glass, loses energy).
    fn wall(&mut self, i: usize) {
        let dx = ((self.px[i] as i32) >> FP) - CX;
        let dy = ((self.py[i] as i32) >> FP) - CY;
        let r2 = dx * dx + dy * dy;
        if r2 <= WALL_R2 {
            return;
        }
        let r = isqrt(r2 as u32) as i32; // one sqrt, boundary only  (r >= WALL_R)
        // clamp position exactly onto r = WALL_R (Q6). No leak, ever.
        self.px[i] = ((CX << FP) + ((dx * WALL_R) << FP) / r) as i16;
        self.py[i] = ((CY << FP) + ((dy * WALL_R) << FP) / r) as i16;
        let vx = self.vx[i] as i32;
        let vy = self.vy[i] as i32;
        let vn = (vx * dx + vy * dy) / r; // outward normal speed (Q6)
        if vn > 0 {
            let k = (vn * (256 + REST_E)) >> 8; // (1 + e)
            self.vx[i] = (vx - (k * dx) / r) as i16;
            self.vy[i] = (vy - (k * dy) / r) as i16;
        }
    }

    // -----------------------------------------------------------------------
    // Render: clear the tracked union bbox, splat SET squares through the LUT
    // (index-dithered, aurora-shaded), sparkle the surface, lay the meniscus
    // line over the swarm, mark the union. `alpha` = reveal fade (256 normal).
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
        // erase region = union(this, previous TIGHT bbox) — clip y >= CLOCK_Y1
        let lb = self.last_bbox;
        let er = (
            cur.0.min(lb.0 as i32).max(0),
            cur.1.min((lb.1 as i32).max(CLOCK_Y1)).max(CLOCK_Y1),
            cur.2.max(lb.2 as i32).min(W - 1),
            cur.3.max(lb.3 as i32).min(H - 1),
        );

        let t1 = (elapsed_ms >> 5) as i32; // aurora scroll
        let t2 = (elapsed_ms >> 6) as i32;

        {
            let fb = wfb.buf_mut();
            // clear ONLY the tracked region (row spans; never memset 424 KiB)
            fill_rect_black(fb, er.0, er.1, er.2, er.3);

            // reset meniscus column tops (topmost = smallest y)
            for c in self.surf_top.iter_mut() {
                *c = i16::MAX;
            }

            for i in 0..NW {
                let x = (self.px[i] as i32) >> FP;
                let y = (self.py[i] as i32) >> FP;
                if y < CLOCK_Y1 {
                    continue; // clock stays topmost
                }
                let surf = self.flags[i] & 1 != 0;
                let vx = self.vx[i] as i32;
                let vy = self.vy[i] as i32;
                let spd = isqrt((vx * vx + vy * vy) as u32) as i32;
                // aurora: NOISE ~ [40,215]; (a+b-256)>>3 -> ~ +-22 drift
                let au = (lock::NOISE_A[(((x >> 2) + t1) & 255) as usize] as i32
                    + lock::NOISE_B[(((y >> 2) - t2) & 255) as usize] as i32
                    - 256)
                    >> 3;
                let depth = self.nbr[i] as i32 * 3; // more neighbours -> deeper -> darker
                let mut base = (if surf { 206 } else { 150 }) + (spd >> 4) + au - depth;
                base = (base * alpha) >> 8; // reveal fade
                base = base.clamp(0, 255);

                // SET 5x5 body square, index-dithered (divide-free per pixel)
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

                // per-particle meniscus sparkle (max_px glow — merges, no stack)
                if surf && alpha >= 200 {
                    soft_glow(fb, x, y, 3, (0, 210, 255), 150);
                }

                // track column top for the continuous meniscus line
                let col = ((x * NMENISC as i32) / W).clamp(0, NMENISC as i32 - 1) as usize;
                if (y as i16) < self.surf_top[col] {
                    self.surf_top[col] = y as i16;
                }
            }

            // continuous, tilting meniscus water line over the swarm
            if alpha >= 200 {
                draw_meniscus_line(fb, &self.surf_top);
            }
        }

        // shrinking-bbox discipline: remember THIS tight bbox, mark the union
        self.last_bbox = (cur.0 as i16, cur.1 as i16, cur.2 as i16, cur.3 as i16);
        wfb.mark_rect(er.0, er.1, er.2, er.3);
    }
}

// ===========================================================================
// Free helpers (duplicated tiny integer routines so the module is
// self-contained; identical to apps.rs — call those directly when merged).
// ===========================================================================

#[inline]
fn axis(a: (i16, i16, i16), s: usize) -> i32 {
    match s {
        0 => a.0 as i32,
        1 => a.1 as i32,
        _ => a.2 as i32,
    }
}

/// xorshift32 (non-zero state).
#[inline]
fn rng(s: &mut u32) -> u32 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *s = x;
    x
}

/// Integer sqrt (Newton), isqrt(0)=0 — same as apps::isqrt.
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

/// Black-fill an inclusive rect via row spans (streaming PSRAM writes).
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

/// SET a pre-baked RGB565-BE pair straight to the framebuffer (over black —
/// idempotent, divide-free). Clips the clock band and the panel.
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

/// Per-channel MAX write of tint*v (glow that never self-stacks) — the only
/// place we pay the RGB565 repack, and only on ~surface pixels.
#[inline]
fn max_px(fb: &mut [u8], x: i32, y: i32, tint: (i32, i32, i32), v: i32) {
    if x < 0 || x >= W || y < CLOCK_Y1 || y >= H {
        return;
    }
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 >= fb.len() {
        return;
    }
    let v = v.clamp(0, 255);
    let r5 = ((tint.0 * v / 255 * 31) / 255) as u16;
    let g6 = ((tint.1 * v / 255 * 63) / 255) as u16;
    let b5 = ((tint.2 * v / 255 * 31) / 255) as u16;
    let old = ((fb[idx] as u16) << 8) | fb[idx + 1] as u16;
    let px = ((old >> 11).max(r5) << 11)
        | (((old >> 5) & 0x3F).max(g6) << 5)
        | (old & 0x1F).max(b5);
    fb[idx] = (px >> 8) as u8;
    fb[idx + 1] = px as u8;
}

/// Small soft radial glow (quadratic falloff, MAX-blended) — surface sparkle.
fn soft_glow(fb: &mut [u8], cx: i32, cy: i32, r: i32, tint: (i32, i32, i32), vpeak: i32) {
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let v = vpeak * (r2 - d2) / r2;
            let v = (v * (r2 - d2)) / r2; // quadratic
            max_px(fb, cx + dx, cy + dy, tint, v);
        }
    }
}

/// Connect the per-column surface tops into one bright, tilting cyan line laid
/// over the particle swarm (max_px, 2 px thick). Columns with no fluid break
/// the line.
fn draw_meniscus_line(fb: &mut [u8], surf_top: &[i16; NMENISC]) {
    let colw = W / NMENISC as i32; // ~7 px
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
            line_max(fb, x0, y0, xc, yc, (150, 240, 255), 210);
        }
        prev = Some((xc, yc));
    }
}

/// 2 px MAX-blended line between two points (DDA over the longer axis).
fn line_max(fb: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, tint: (i32, i32, i32), v: i32) {
    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let steps = dx.max(dy).max(1);
    for s in 0..=steps {
        let x = x0 + (x1 - x0) * s / steps;
        let y = y0 + (y1 - y0) * s / steps;
        max_px(fb, x, y, tint, v);
        max_px(fb, x, y + 1, tint, v); // 2 px thick
    }
}
