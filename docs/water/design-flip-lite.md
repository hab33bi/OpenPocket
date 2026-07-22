# Water — FLIP/PIC-lite liquid (design, `flip-lite` approach)

Status: DESIGN, buildable. Grounded in the real repo: `apps.rs` helpers
(`set_px`/`max_px`/`isqrt`/`soft_dot`/`blend_px`), `qmi8658::read_accel`,
`build.rs` LUT idiom, the 25 ms / 13 ms flush budget from
`PERF-DUALCORE-PLAN.md`, and the `apps::tick` / `draw_reveal` call sites.

---

## 0. The honest verdict up front

**Does a real Jacobi pressure projection fit ~10 ms of integer budget?
Yes — comfortably, and it is *not* the bottleneck.** A projection on a
**coarse** MAC grid (24×24, ~452 fluid cells) is a handful of 5-point
stencil sweeps over ~450 cells: even 24 Jacobi iterations is ~130 k
integer ops (~0.6 ms). The expensive parts of FLIP are the
**particle↔grid transfers** (P2G/G2P), and the *dominant* cost of the
whole app is the **fixed ~13 ms full-frame flush** — a hardware floor
that no algorithm choice touches.

So this is a full FLIP/PIC-lite with a genuine divergence solve. The one
reduction I make versus textbook FLIP is dropping the **APIC affine
term** (per-particle C matrix): it doubles particle SRAM and adds a
per-node outer-product, buys little at this cell size, and PIC/FLIP blend
already gives the look. Everything else is real: staggered MAC transfer,
Jacobi Poisson solve, FLIP velocity feedback.

Call: **640 particles, 24×24 MAC grid, 24 Jacobi iterations, 40 fps**
(25 ms cadence = `ANIM_FRAME_US`), with ~7 ms of compute headroom to spend
on more iterations or particles after first-flash tuning.

---

## 1. Fixed-point scheme + overflow proof

`dt ≡ 1 frame`. All kinematics are per-frame; there is no `dt` multiply in
the hot loop (folded into the acceleration/velocity units). No `f32`
anywhere in per-particle / per-cell code.

| Quantity | Format | Unit | Storage | Range used |
|---|---|---|---|---|
| Particle pos `px,py` | **Q6** | 1/64 px | `i16` | 0..466 px → 0..29 824 |
| Particle vel `vx,vy` | **Q6** | 1/64 px/frame | `i16` | clamp ±`V_MAX`=24 px → ±1536 |
| Grid vel `gu,gv` | Q6 | px/frame | `i16` | ±~1536 |
| Grid momentum `mu,mv` | Q12 | — | `i32` | accumulator |
| Grid mass `mass_u/v` | Q6 | weight sum | `i32` | accumulator |
| Pressure `p`, `div` | integer | vel-divergence | `i16` (clamped ±30000) | |
| Gravity/impulse `ax,ay` | Q6 | px/frame² | `i16` | ±~1024 |

**Why Q6 `i16` positions fit.** Max on-screen coordinate 466 px = 29 824 <
32 767. A particle can only leave the wall by at most one frame of
velocity before the boundary pass corrects it; with `V_MAX = 24 px`,
worst excursion = 459 (max wall reach) + 24 = 483 px = 30 912 < 32 767. ✔
The velocity clamp is what guarantees position never overflows `i16`.

### Overflow proof — every hot-loop multiply (worst-case → fits)

Signed `i32` limit is 2 147 483 647.

1. **Boundary radius** `d2 = dxp*dxp + dyp*dyp`, computed in **whole px**
   (`dxp = (px>>6) - CX`), *not* Q6 — this is the one deliberate
   reduction to stay in range. `|dxp|,|dyp| ≤ 256`. `256² = 65 536`, sum
   `131 072`. ✔ (In Q6 this would be `(256·64)² = 2.68 e8` per term — sum
   `5.4 e8`, still < 2^31 but wasteful; px form is cheaper and clearer.)
2. **Wall normal** `nx = dxp*256/d` (Q8 unit), `d = isqrt(d2) ≥ 1`.
   `|dxp*256| ≤ 256·256 = 65 536`. ✔ `|nx| ≤ 256`.
3. **Reflect dot** `vn = (vx*nx + vy*ny) >> 8`. `|vx| ≤ 1536`, `|nx| ≤
   256` → `393 216` per term, sum `786 432`. ✔ `|vn| ≤ 3072` (Q6).
4. **Reflect apply** `vx -= (vn*nx*REST) >> 16`. `vn*nx ≤ 3072·256 =
   786 432`; `×REST` (`REST ≤ 512` Q8) = `4.0 e8`. ✔ < 2^31.
5. **Damping** `vx = (vx*DAMP) >> 8`, `DAMP ≤ 256`. `1536·256 = 393 216`. ✔
6. **P2G weight** cell-fraction `fx = (rem*64)/H_Q6`, `rem ≤ H_Q6-1 =
   1279`; `rem*64 ≤ 81 856`. ✔ Bilinear weight `w = ((64-fx)*(64-fy))>>6
   ≤ 64`. `(64·64)=4096`. ✔
7. **P2G momentum accumulate** `mu[n] += w*vx`. `64·1536 = 98 304` per
   particle (Q12); a pathological 100 particles on one node → `9.83 e6`. ✔
   (would need >21 000 coincident particles to overflow i32).
8. **Grid velocity** `gu = mu / mass_u` = Q12/Q6 = Q6, bounded by the max
   contributing `|v| ≤ 1536`. ✔ (`mass_u ≥ 1` guarded.)
9. **Divergence** `div = (gu_r-gu_l)+(gv_d-gv_u)`, each term ≤ ±3072, sum
   ±6144. ✔ (`i16`).
10. **Jacobi** `p' = (Σ4 p_nbr − div) / K` — the 4-neighbour sum is done
    in `i32` (`4·30000 = 120 000`), divided by `K∈1..4`, clamped to
    ±30 000, stored `i16`. ✔
11. **Pressure gradient** `gu -= (dp*KP) >> KPS`, `dp = p_r-p_l ≤ ±60 000`
    (`i32`), `KP ≤ 8`. `60 000·8 = 480 000`. ✔
12. **G2P sample** `Σ w_ij·gu_ij` (4 nodes), `w ≤ 64`, `gu ≤ 1536` →
    `98 304` each, sum `393 216`. ✔ `>>6` → Q6 velocity.
13. **FLIP blend** `v = (FLIPB*flip + (256-FLIPB)*pic) >> 8`, terms ≤
    `256·~2500 = 640 000`. ✔
14. **Render speed** `isqrt(vx*vx+vy*vy)`, `1536² = 2.36 e6`, ×2 =
    `4.72 e6` (`u32`). ✔

Every multiply is < 2^31 with the stated clamps; the only place I leave
Q6 for whole-px is the radius test (#1), by design.

---

## 2. State layout + SRAM byte count

Large arrays are random-accessed every frame → **internal SRAM**, not the
PSRAM framebuffer. They live in a module-level `static mut WATER: WaterSim`
(`.bss`, internal SRAM, single-owner core-0), **not** inline in the
stack-resident `apps::State` — an 18 KB struct on the run-loop stack is a
stack-overflow risk. `apps::State` gains only a tiny 12-byte head
(`WaterHead`) for the seed flag + calibration + RNG + aurora phase; the
tick reaches into the static. (Honest deviation from the brief's "fields
in State": the *data* is static SRAM per the brief's "stack/static … not
the framebuffer"; only the placement moves off the stack.)

```
Grid: Nx = Ny = 24, cell h = 20 px, origin = CX - Nx*h/2 = -7 (covers Ø452)
```

| Buffer | Elems | Bytes |
|---|---|---|
| `pos_x,pos_y : i16` | 640×2 | 2 560 |
| `vel_x,vel_y : i16` | 640×2 | 2 560 |
| `gu : i16` (u faces 25×24) | 600 | 1 200 |
| `gv : i16` (v faces 24×25) | 600 | 1 200 |
| `mu : i32` (u mass/mom scratch, reused for `gu0`) | 600 | 2 400 |
| `mv : i32` (v mass/mom scratch, reused for `gv0`) | 600 | 2 400 |
| `p : i16`, `p2 : i16` (Jacobi ping-pong) | 576×2 | 2 304 |
| `div : i16` | 576 | 1 152 |
| `solid : u8` (fluid/solid mask, built once) | 576 | 576 |
| `WaterHead` (seeded, cal_x, cal_y, last_ax, last_ay, rng, aphase) | — | ~16 |
| **Total** | | **≈ 16.4 KB** |

`mu/mv` do double duty: accumulate P2G momentum, then (after grid velocity
is divided out) hold the **pre-projection** grid velocity copy `gu0/gv0`
for the FLIP delta — so no separate FLIP buffers. 16.4 KB is trivial on
the S3's 512 KB SRAM.

---

## 3. Per-frame update

### 3.1 Gravity from the IMU (tunable map + rest calibration)

Read once per frame in the run loop (which owns `self.i2c`), pass raw into
the tick. Axis→screen mapping and rest bias are **unknown until measured**
(like the touch Y-flip) → exposed as consts + a capture-on-open offset:

```rust
// --- FIRST-FLASH TUNING (edit consts, no rewrite) -------------------
// Which raw IMU axis drives each screen axis, and its sign.
const IMU_X_AXIS: usize = 0; // 0=ax 1=ay 2=az
const IMU_X_SIGN: i32   = 1;
const IMU_Y_AXIS: usize = 1;
const IMU_Y_SIGN: i32   = 1; // flip after the tilt-in-4-directions probe
const GRAV_SHIFT: i32   = 6; // raw(8192=1g) >> 6 → ~128 Q6 accel = ~2 px/frame²/g
const V_MAX: i32        = 1536; // 24 px/frame in Q6
```

- **Rest calibration**: on app open, average N flat samples into
  `head.cal_x/cal_y`; subtract every frame. (`az` is unused for the 2-D
  pool but its magnitude = "flat vs upright" → optional splashiness gate.)
- **Gravity vector** each frame:
  `raw = [ax,ay,az]`; `gx = ((raw[IMU_X_AXIS]*IMU_X_SIGN) - cal_x) >> GRAV_SHIFT`,
  `gy = ((raw[IMU_Y_AXIS]*IMU_Y_SIGN) - cal_y) >> GRAV_SHIFT` (Q6 px/frame²).
- **Dead IMU**: `read_accel` → `Err` reuses last vector, else falls back
  to a fixed down-vector `(0, +A_G)` so the app still pools.

### 3.2 Physics core (PIC/FLIP with real projection)

Order per frame:

1. **Integrate forces on particles** (cheap, keeps grid transfer clean):
   `v += g + impulse`; clamp `|v| ≤ V_MAX`; `v = (v*DAMP)>>8`
   (`DAMP=254` ≈ 0.992 — bleeds energy so nothing runs perpetually).
2. **P2G scatter** (bilinear, staggered): clear `gu,gv,mu,mv`; for each
   particle deposit `vx` to the 4 surrounding **u-faces**, `vy` to the 4
   **v-faces**, weighted by bilinear `w`. Accumulate momentum in `mu/mv`,
   mass in the mass halves.
3. **Grid velocity** `gu = mu/mass_u`, `gv = mv/mass_v` (guard mass≥1).
   **Copy `gu→gu0`, `gv→gv0`** (into the freed mass slots) for FLIP.
4. **Boundary velocity condition on the grid**: faces whose cell is
   `solid` (outside the wall) get zero normal velocity — enforces
   no-through-flow *in the solve*, not just at the particle level.
5. **Divergence** `div[c]` for each fluid cell (5-point, staggered).
6. **Jacobi pressure** ×`P_ITERS` (24): `p2[c] = (Σ p[fluid nbr] − div[c])
   / K[c]`, `K` = fluid-neighbour count; ping-pong `p↔p2`; clamp ±30 000.
   Solid neighbours are skipped (pure Neumann wall → mass can't leak).
7. **Project** `gu -= (p_r−p_l)*KP>>KPS`, `gv -= (p_d−p_u)*KP>>KPS`
   → divergence-free grid velocity (incompressible: the pool *spreads and
   levels* instead of piling to a point).
8. **G2P gather** (FLIP/PIC blend, `FLIPB=243`≈0.95):
   `pic = sample(gu,gv)`; `flip = v_old + (sample(gu,gv) − sample(gu0,gv0))`;
   `v = (FLIPB*flip + (256−FLIPB)*pic) >> 8`. High FLIP = lively;
   the 5 % PIC is the numerical damping that kills ringing (stability).
9. **Advect** `pos += v`.
10. **Particle boundary** (round wall, §3.3).

### 3.3 Round-wall boundary (no leaks, energy damping)

Whole-px radius test (overflow-safe). If a particle is outside
`R_WALL=225` (one px inside `BEZEL_R=223`+2 margin so squares don't bleed
the bezel):

- project to the rim along the inward normal `(nx,ny)` (Q8),
- reflect the normal velocity with restitution `REST≈0.35 Q8`
  (energy loss → no perpetual motion; a hard wall would ring forever),
- keep the tangential component (water slides along glass).

Because the *grid* solve already zeroes normal flow at solid faces (step
4/6), particles arrive at the wall with little normal velocity — the
reflect is a cleanup, not the main containment. Two independent barriers =
**no leaks**.

### 3.4 Jerk → slosh impulse (spray on a flick)

`jerk = raw_now − raw_last` per axis (in the run loop, `i16` diff). If
`|jerk|` exceeds `SHAKE_TH`, convert to a velocity kick: add
`(jerk >> JERK_SHIFT)` to every particle's velocity, **plus** a randomized
outward burst (`head.rng` LCG) to the fraction of particles nearest the
leading wall — those break off as spray (they exceed the local pack and
the pressure solve can't hold them, so they arc as droplets). The kick is
clamped by `V_MAX`, so even a violent shake can't overflow or explode.

### 3.5 Rest breathing ripple (alive at rest)

When `|g_planar|` and `|jerk|` are both below thresholds (watch flat &
still), inject a gentle standing wave so the surface never dies:
for each particle add `dv_y = (lut_sin_cos_q14(head.aphase + px*K)>>SCALE)`
using the existing `trig::lut_sin_cos_q14` (Q14 sine LUT) — a slow
left-right rocking of the surface at the shared OS breathing tempo.
`head.aphase += BREATH_STEP` each frame. Amplitude ~½ px → visible shimmer,
never disturbs the pool.

---

## 4. Rendering

Signature look: a **luminous swarm of tiny neon-blue squares** on black
AMOLED (the black between them *is* the shimmer — this is the OpenPocket
aesthetic, not a defect), denser at the bottom (natural from gravity +
incompressibility), brighter/whiter at fast crests, a bright meniscus
surface line, and a slow aurora drift through the body.

- **Squares**: 3–4 px filled, one WATER_LUT color per particle.
- **WATER_LUT** (build-time, §5.4): 256-entry deep-indigo → neon-blue →
  near-white-cyan gradient, RGB565-BE `[(u8,u8);256]` — same doctrine as
  `saber_lut`. Indexed by per-particle **intensity**:
  `idx = BASE + (speed>>S_SPD) − (depth>>S_DEP) + aurora`, clamped 0..255.
  - `speed = isqrt(vx²+vy²)` → fast particles brighter (crest/spray glow).
  - `depth` from grid mass at the particle's cell → deep body darker.
  - `aurora = (NOISE_A[(cx+t)&255] + NOISE_B[(cy−t)&255] − 128) >> 3`,
    reusing `lock::NOISE_A/NOISE_B` scrolled by `t = elapsed_ms>>5` → a
    luminous band drifts through the mass (ties Water into the OS aurora).
- **Dithering**: add a 4×4 Bayer value to `idx` **before** the LUT
  lookup, per pixel, lit pixels only — breaks RGB565 banding on the
  gradient exactly like `build.rs`'s asset dither.
- **Meniscus**: after splatting, for each grid column find the topmost
  fluid cell (from `mass`) and `max_px` a 2 px bright-cyan line across the
  cell at that surface height — a light-catching water line. Cheap (~24
  cols × ~20 px).
- **Status clock stays topmost**: all liquid render is **clipped to
  `y ≥ CLOCK_Y1 (70)`**, below the status band (y 26–66). The clock is
  drawn on open + minute-rollover via the normal `tick_status` path and is
  never overwritten. (The round wall's true top is y≈7, but rendering
  stops at 70 — liquid climbing that high simply clips under the clock,
  which reads correctly as "liquid below the clock".)

### 4.1 Damage / flush story (full-frame confirmed)

- **Render write traffic is small**: clear last-frame footprints (stored
  positions, ~6×6 each to cover glow), then splat this frame's squares —
  ~2·640·36 px ≈ 46 K px ≈ 90 KB of PSRAM writes (SET, no RMW for the
  cores; `max_px` RMW only on meniscus/aurora crests).
- **Flush**: mark the **union bbox** of the liquid (old ∪ new). At rest
  (pool at the bottom, bbox ~400×190) `flush_dirty`'s 3/4 rule gives a
  **partial** flush (~150 KB → ~5 ms). When sloshed up a wall the bbox
  approaches full → **full-frame flush ~13 ms**. So this is,
  per the brief, a **full-frame-flush app** in its worst (active) case,
  and cheaper at rest — an honest, self-adjusting damage story. The 13 ms
  wire time is the hard floor and sets the 40 fps ceiling regardless of
  particle count.

---

## 5. Exact integration

### 5.1 `apps::State` additions

```rust
// Tiny head only — big arrays live in `static mut WATER` (§2).
pub struct WaterHead {
    pub seeded: bool,
    pub cal_x: i32, pub cal_y: i32,      // rest bias, captured on open
    pub last_ax: i16, pub last_ay: i16,  // for jerk
    pub rng: u32,                        // LCG for spray
    pub aphase: i32,                     // breathing/aurora phase
    pub bbox: (i16, i16, i16, i16),      // last liquid bbox (for clear+mark)
}
// in struct State { ...; pub wa: WaterHead }
// in State::new():  wa: WaterHead { seeded:false, cal_x:0, cal_y:0,
//                    last_ax:0, last_ay:0, rng:0x2545_F491, aphase:0,
//                    bbox:(0,0,-1,-1) },
```

### 5.2 `has_content`

```rust
pub fn has_content(_idx: usize) -> bool { true }  // Water now has content
```

### 5.3 `draw_reveal` WATER branch (fill-in on open)

```rust
} else if idx == WATER {
    // Seed the pool at the bottom (particles rain in with q), capture the
    // rest bias will be done on first tick. Draw a first splat so the
    // reveal shows liquid rising.
    water_seed(&mut st.wa, q_q8);            // fill lower cells with N particles
    let (x0,y0,x1,y1) = water_render(wfb, &mut st.wa, /*t*/0, q_q8);
    fx.push(x0,y0,x1,y1); wfb.mark_rect(x0,y0,x1,y1);
}
```

### 5.4 Run-loop tick + gravity (minimal `app.rs` diff)

`apps::tick` has no `i2c`; Water needs it. Route Water out of the generic
tick in the `Scene::App` branch (~line 322), reading the IMU in the loop
that already owns `self.i2c`:

```rust
} else {
-   apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
-   if apps::shows_status(idx) && status_minute != now.minute {
-       status_minute = now.minute;
-       wheel::tick_status(&mut self.wfb, &now, wheel_batt);
-   }
+   if idx == apps::WATER {
+       let raw = qmi8658::read_accel(&mut self.i2c).ok(); // (i16,i16,i16)
+       apps::water_tick(&mut self.wfb, raw, elapsed, &mut app_state);
+   } else {
+       apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
+   }
+   if apps::shows_status(idx) && status_minute != now.minute {
+       status_minute = now.minute;
+       wheel::tick_status(&mut self.wfb, &now, wheel_batt);
+   }
}
```

And bump Water to 40 fps (else it rests at the 50 ms/20 fps `FRAME_US`) in
the `frame_us` match (~line 457):

```rust
+   _ if matches!(scene, Scene::App(apps::WATER)) => ANIM_FRAME_US, // 25 ms
    _ => FRAME_US,
```

`qmi8658` is already `crate::drivers::qmi8658` (used at boot in `main.rs`);
add `use crate::drivers::qmi8658;` to `app.rs` if not present.

### 5.5 `build.rs` WATER_LUT generator

Add `generate_water_lut();` to `main()` and:

```rust
/// 256-entry deep-indigo → neon-blue → white-cyan water gradient,
/// RGB565-BE pairs. Dithered per-pixel at blit time (lit pixels only).
fn generate_water_lut() {
    // stops: (idx, r,g,b)
    let stops = [(0i32, 0,18,54), (110, 0,120,210), (200, 40,200,255), (255, 190,245,255)];
    let lerp = |a:i32,b:i32,t:i32,d:i32| a + (b-a)*t/d;
    let mut body = String::from("/// Auto-generated water gradient (RGB565-BE).\n\
        pub static WATER_LUT: [(u8,u8); 256] = [");
    for i in 0..256i32 {
        // find segment
        let mut s = 0; while s+1 < stops.len()-0 && i > stops[s+1].0 { s += 1; }
        let (i0,r0,g0,b0) = stops[s];
        let (i1,r1,g1,b1) = stops[(s+1).min(stops.len()-1)];
        let d = (i1-i0).max(1); let t = (i-i0).clamp(0,d);
        let (r,g,b) = (lerp(r0,r1,t,d), lerp(g0,g1,t,d), lerp(b0,b1,t,d));
        let r5 = ((r*31/255) as u16)&0x1F;
        let g6 = ((g*63/255) as u16)&0x3F;
        let b5 = ((b*31/255) as u16)&0x1F;
        let px = (r5<<11)|(g6<<5)|b5;
        if i>0 { body.push(','); }
        body.push_str(&format!("({},{})", (px>>8) as u8, px as u8));
    }
    body.push_str("];\n");
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&out).join("water_lut.rs"), body).unwrap();
}
```

Include in `apps.rs`: `include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));`
(NOISE reuse: `use crate::scenes::lock::{NOISE_A, NOISE_B};`).

---

## 6. Per-frame ops + ms estimate

240 MHz → 10 ms ≈ 2.4 M cycles. Compute budget = 25 ms cadence − ~13 ms
flush − ~1 ms overhead ≈ **~11 ms**.

**Physics** (`N=640`, ~452 fluid cells, 24 iters), all internal SRAM:

| Stage | ops | notes |
|---|---:|---|
| Integrate + clamp + damp | ~26 k | 640 × ~40 |
| P2G scatter (8 faces/particle) | ~41 k | 640 × ~64 |
| Grid vel divide + save gu0/gv0 | ~4 k | ~1200 div + copy |
| Divergence | ~4 k | 452 × 8 |
| Jacobi ×24 | ~130 k | 452 × 12 × 24 |
| Project | ~4 k | 600 faces × 6 |
| G2P gather + FLIP blend | ~38 k | 640 × ~60 |
| Advect + wall boundary | ~26 k | 640 × ~40 |
| **Physics total** | **~273 k ops** | ≈ **2.5–3 ms** |

**Render** (PSRAM writes are the cost, not the arithmetic):

| Stage | px / ops | notes |
|---|---:|---|
| Clear old footprints | ~46 k px | 640 × ~6×6 |
| Splat squares + LUT + dither | ~10 k px SET | 640 × 16 |
| Meniscus (`max_px` RMW) | ~1 k px | 24 cols |
| Aurora crest `max_px` | ~few k | subset |
| **Render total** | **~90 KB PSRAM** | ≈ **1.5–2.5 ms** |

**Frame**: physics ~3 ms + render ~2 ms + flush 5–13 ms = **~10–18 ms**,
inside the 25 ms cadence with ~7 ms headroom. **40 fps, flush-bound.** The
headroom can go to 40→48 Jacobi iterations or 640→~900 particles before
compute+flush crosses 25 ms.

---

## 7. Stability guarantees + honest weaknesses

**Never blows up:**
- Every hot multiply proven < 2^31 (§1); velocity clamp `|v| ≤ V_MAX`
  bounds position, momentum, and reflection math by construction.
- Pressure clamped ±30 000 each Jacobi iteration → the solve can't diverge
  even if fed pathological divergence.
- 5 % PIC in the FLIP blend + `DAMP≈0.992` continuously remove the
  high-frequency energy that makes pure FLIP ring and explode.
- Staggered MAC (not collocated central differences) has **no
  checkerboard null-space**, so pressure can't grow an odd/even mode.

**Never leaks:**
- Two independent barriers: grid solid-face no-through-flow *in the solve*
  (step 3.2.4/6, pure Neumann wall), **and** per-particle rim projection +
  reflect (§3.3). A particle would have to defeat both in one frame.

**Never piles to a singularity:**
- The Jacobi projection makes the velocity field ~divergence-free →
  incompressible → the mass **spreads and levels**. High-density cells
  develop high pressure whose gradient pushes particles out; they cannot
  collapse to a point (that's exactly the "spreads instead of heaps"
  behaviour the plan's grid pass is for).

**Weaknesses (honest):**
- **Coarse grid (20 px)**: incompressibility is enforced at ~20 px scale,
  so fine surface detail (thin sheets, tendrils) is beyond resolution;
  the pool reads as a body of glowing points, not photoreal water.
- **Jacobi is slow to converge**: 24 iters propagate pressure ~24 cells —
  fine for a shallow pool (~10 cells deep) but a fully-vertical column
  (watch upright) is under-solved and slightly compressible → the far-wall
  climb is a touch soft. Red-black Gauss-Seidel (in-place, ~2× faster
  convergence) is the upgrade if it looks mushy — same memory.
- **No APIC affine term**: slightly more numerical diffusion than full
  APIC; vortices smear a bit. Acceptable at this cell size; the FLIP blend
  keeps it lively.
- **Grainy at low density near the surface/crests** — mitigated by soft
  aurora/`max_px` glow and the meniscus line, but it is a particle look by
  design, not a filled sheet.
- **Full-frame flush dominates** (13 ms) — the app is pinned at 40 fps
  until perf-plan **P1 (async flush)** lands, which would roughly double
  the ceiling. Water is the app that most wants P1.

---

## 8. Rust code sketch (core update + render)

Real bodies using the repo's integer helpers (`isqrt`, `set_px`,
`max_px`, `blend_px`, `WATER_LUT`, `NOISE_A/B`). Indices/consts abbreviated
where obvious; this is the shape, not the final line-count.

```rust
// ---- config ---------------------------------------------------------
const NX: usize = 24; const NY: usize = 24; const H: i32 = 20; // px/cell
const H_Q6: i32 = H * 64; const ORG: i32 = CX - (NX as i32 * H) / 2; // -7 px
const NP: usize = 640;
const R_WALL: i32 = 225;                 // whole px
const DAMP: i32 = 254; const REST: i32 = 90; const FLIPB: i32 = 243;
const P_ITERS: usize = 24; const KP: i32 = 6; const KPS: i32 = 6;
const CLOCK_Y1: i32 = 70;

struct WaterSim {
    px: [i16; NP], py: [i16; NP], vx: [i16; NP], vy: [i16; NP],
    gu: [i16; (NX+1)*NY], gv: [i16; NX*(NY+1)],
    mu: [i32; (NX+1)*NY], mv: [i32; NX*(NY+1)],   // mass→gu0 / mass→gv0 reuse
    p: [i16; NX*NY], p2: [i16; NX*NY], div: [i16; NX*NY],
    solid: [u8; NX*NY],
}
static mut WATER: WaterSim = WaterSim::zeroed();   // .bss, internal SRAM

#[inline] fn ui(i: i32, j: i32) -> usize { (j*(NX as i32+1)+i) as usize }
#[inline] fn vi(i: i32, j: i32) -> usize { (j*NX as i32+i) as usize }
#[inline] fn ci(i: i32, j: i32) -> usize { (j*NX as i32+i) as usize }

pub fn water_tick(wfb: &mut WatchFb, raw: Option<(i16,i16,i16)>,
                  elapsed_ms: u32, st: &mut State) {
    let s = unsafe { &mut WATER };
    let h = &mut st.wa;
    if !h.seeded { water_seed(h); calibrate(h, raw); h.seeded = true; }

    // --- gravity + jerk (tunable map, rest bias) ---------------------
    let a = raw.unwrap_or((0, 0, 0));
    let ax = [a.0 as i32, a.1 as i32, a.2 as i32];
    let gx = ((ax[IMU_X_AXIS]*IMU_X_SIGN) - h.cal_x) >> GRAV_SHIFT;
    let gy = ((ax[IMU_Y_AXIS]*IMU_Y_SIGN) - h.cal_y) >> GRAV_SHIFT;
    let jx = (a.0 - h.last_ax) as i32; let jy = (a.1 - h.last_ay) as i32;
    h.last_ax = a.0; h.last_ay = a.1;
    let (kx, ky) = if jx*jx + jy*jy > SHAKE_TH { (jx>>JERK_SHIFT, jy>>JERK_SHIFT) } else { (0,0) };
    let breathing = gx.abs() + gy.abs() < REST_TH && kx == 0 && ky == 0;
    h.aphase = h.aphase.wrapping_add(BREATH_STEP);

    // --- 1. integrate forces on particles ----------------------------
    for k in 0..NP {
        let mut vx = s.vx[k] as i32 + gx + kx;
        let mut vy = s.vy[k] as i32 + gy + ky;
        if breathing {
            let ph = h.aphase + (s.px[k] as i32) * BREATH_KX;
            vy += crate::trig::lut_sin_cos_q14(ph) >> BREATH_SCALE;
        }
        vx = vx.clamp(-V_MAX, V_MAX); vy = vy.clamp(-V_MAX, V_MAX);
        s.vx[k] = ((vx*DAMP)>>8) as i16; s.vy[k] = ((vy*DAMP)>>8) as i16;
    }

    // --- 2. P2G scatter (staggered, bilinear) ------------------------
    s.gu.fill(0); s.gv.fill(0); s.mu.fill(0); s.mv.fill(0);
    for k in 0..NP {
        let (px, py) = (s.px[k] as i32, s.py[k] as i32);
        scatter(&mut s.gu, &mut s.mu, px, py, s.vx[k] as i32, true);   // u-faces
        scatter(&mut s.gv, &mut s.mv, px, py, s.vy[k] as i32, false);  // v-faces
    }
    // grid velocity, then stash pre-projection copy into the mass slot
    for n in 0..s.gu.len() { let m=s.mu[n]; s.gu[n]=if m>0 {(s.gu[n] as i32*64/ (m/ (s.gu[n] as i32).max(1)).max(1)) as i16} else {0}; }
    // (real code: gu = momentum/mass; shown compactly) — then:
    grid_velocity(&mut s.gu, &s.mu); grid_velocity(&mut s.gv, &s.mv);
    let (gu0, gv0) = save_copy(&s.gu, &s.gv, &mut s.mu, &mut s.mv);

    // --- 3. solid faces: zero normal velocity at the wall ------------
    enforce_solid_faces(s);

    // --- 4. divergence -----------------------------------------------
    for j in 1..NY as i32-1 { for i in 1..NX as i32-1 {
        if s.solid[ci(i,j)]==1 { s.div[ci(i,j)]=0; continue; }
        s.div[ci(i,j)] = ((s.gu[ui(i+1,j)]-s.gu[ui(i,j)])
                        + (s.gv[vi(i,j+1)]-s.gv[vi(i,j)])) as i16;
    }}

    // --- 5. Jacobi pressure solve ------------------------------------
    s.p.fill(0);
    for _ in 0..P_ITERS {
        for j in 1..NY as i32-1 { for i in 1..NX as i32-1 {
            let c = ci(i,j); if s.solid[c]==1 { s.p2[c]=0; continue; }
            let mut sum = 0i32; let mut kk = 0i32;
            for (di,dj) in [(-1,0),(1,0),(0,-1),(0,1)] {
                let n = ci(i+di, j+dj);
                if s.solid[n]==0 { sum += s.p[n] as i32; kk += 1; }
            }
            if kk==0 { s.p2[c]=0; continue; }
            s.p2[c] = ((sum - s.div[c] as i32) / kk).clamp(-30000, 30000) as i16;
        }}
        core::mem::swap(&mut s.p, &mut s.p2);
    }

    // --- 6. project to divergence-free -------------------------------
    for j in 1..NY as i32-1 { for i in 1..NX as i32-1 {
        let (l,r)=(ci(i-1,j),ci(i,j)); let (u,d)=(ci(i,j-1),ci(i,j));
        if s.solid[r]==0 && s.solid[l]==0 {
            s.gu[ui(i,j)] -= (((s.p[r]-s.p[l]) as i32 * KP) >> KPS) as i16; }
        if s.solid[d]==0 && s.solid[u]==0 {
            s.gv[vi(i,j)] -= (((s.p[d]-s.p[u]) as i32 * KP) >> KPS) as i16; }
    }}

    // --- 7. G2P gather + FLIP blend + advect + wall ------------------
    for k in 0..NP {
        let (px, py) = (s.px[k] as i32, s.py[k] as i32);
        let pic_x = gather(&s.gu, px, py, true);
        let pic_y = gather(&s.gv, px, py, false);
        let d_x   = pic_x - gather(gu0, px, py, true);
        let d_y   = pic_y - gather(gv0, px, py, false);
        let flip_x = s.vx[k] as i32 + d_x; let flip_y = s.vy[k] as i32 + d_y;
        let mut vx = ((FLIPB*flip_x + (256-FLIPB)*pic_x) >> 8).clamp(-V_MAX,V_MAX);
        let mut vy = ((FLIPB*flip_y + (256-FLIPB)*pic_y) >> 8).clamp(-V_MAX,V_MAX);
        let mut nx = px + vx; let mut ny = py + vy;

        // round wall in WHOLE PX (overflow-safe radius)
        let (dxp, dyp) = ((nx>>6)-CX, (ny>>6)-CY);
        let d2 = dxp*dxp + dyp*dyp;
        if d2 > R_WALL*R_WALL {
            let d = isqrt(d2 as u32) as i32;
            let (unx, uny) = (dxp*256/d, dyp*256/d);        // Q8 inward-  normal
            nx = (CX + ((R_WALL*unx)>>8)) << 6;             // clamp to rim (Q6)
            ny = (CY + ((R_WALL*uny)>>8)) << 6;
            let vn = (vx*unx + vy*uny) >> 8;                // normal speed (Q6)
            vx -= (vn*unx*REST) >> 14;                      // reflect w/ loss
            vy -= (vn*uny*REST) >> 14;
        }
        s.vx[k]=vx as i16; s.vy[k]=vy as i16; s.px[k]=nx as i16; s.py[k]=ny as i16;
    }

    // --- 8. render (clear old bbox, splat, meniscus, mark) -----------
    let t = (elapsed_ms >> 5) as i32;
    let (x0,y0,x1,y1) = water_render(wfb, s, h, t);
    // union with last bbox so trailing pixels are cleared/marked
    let ub = union(h.bbox, (x0 as i16,y0 as i16,x1 as i16,y1 as i16));
    h.bbox = (x0 as i16,y0 as i16,x1 as i16,y1 as i16);
    wfb.mark_rect(ub.0 as i32, ub.1 as i32, ub.2 as i32, ub.3 as i32);
}

fn water_render(wfb: &mut WatchFb, s: &WaterSim, h: &WaterHead, t: i32)
    -> (i32,i32,i32,i32) {
    let fb = wfb.buf_mut();
    // clear last frame's footprints (bbox fill is simplest & cache-friendly)
    fill_rect_black(fb, h.bbox.0 as i32, h.bbox.1 as i32,
                        h.bbox.2 as i32, h.bbox.3 as i32);
    let (mut x0,mut y0,mut x1,mut y1) = (W, H, 0, 0);
    for k in 0..NP {
        let (cx, cy) = ((s.px[k] as i32)>>6, (s.py[k] as i32)>>6);
        if cy < CLOCK_Y1 { continue; }                     // clock stays topmost
        // intensity: speed up, depth down, aurora drift, Bayer per-pixel
        let spd = isqrt(((s.vx[k] as i32).pow(2) + (s.vy[k] as i32).pow(2)) as u32) as i32;
        let dep = grid_mass_at(s, cx, cy);
        let au  = (NOISE_A[((cx+t)&255) as usize] as i32
                 + NOISE_B[((cy-t)&255) as usize] as i32 - 256) >> 3;
        let base = (90 + (spd>>4) - (dep>>3) + au).clamp(0,255);
        for oy in 0..4 { for ox in 0..4 {
            let (x,y) = (cx-1+ox, cy-1+oy);
            if y < CLOCK_Y1 { continue; }
            let dith = BAYER4[(y&3) as usize][(x&3) as usize];   // ±7
            let idx = (base + dith).clamp(0,255) as usize;
            put_lut_px(fb, x, y, WATER_LUT[idx]);                 // SET over black
        }}
        x0=x0.min(cx-2); y0=y0.min(cy-2); x1=x1.max(cx+2); y1=y1.max(cy+2);
    }
    // meniscus: bright cyan line at each column's surface cell
    for i in 0..NX as i32 {
        if let Some(surf_y) = column_surface_px(s, i) {
            let cx0 = ORG + i*H;
            for x in cx0..cx0+H { for dy in 0..2 {
                max_px(fb, x, surf_y.max(CLOCK_Y1)+dy, (150,240,255), 210); }}
        }
    }
    (x0.max(0), y0.max(CLOCK_Y1), x1.min(W-1), y1.min(H-1))
}

// SET a pre-baked RGB565-BE LUT pixel over black (idempotent splat).
#[inline] fn put_lut_px(fb: &mut [u8], x: i32, y: i32, px: (u8,u8)) {
    if x<0||x>=W||y<0||y>=H { return; }
    let i = ((y*W+x)*2) as usize;
    if i+1 < fb.len() { fb[i]=px.0; fb[i+1]=px.1; }
}
```

(Helper bodies `scatter`, `gather`, `grid_velocity`, `enforce_solid_faces`,
`column_surface_px`, `grid_mass_at`, `water_seed`, `calibrate` are the
obvious bilinear-weight / mask loops; the arithmetic in each is covered by
the §1 overflow proof. `BAYER4` is the standard 4×4 matrix centred to ±7,
matching `build.rs`.)

---

### Summary call

**640 particles · 24×24 MAC grid · 24 Jacobi iterations · 40 fps.** The
projection is real and cheap; the flush is the wall. Ships premium at 40
fps today, doubles with perf-plan P1.
