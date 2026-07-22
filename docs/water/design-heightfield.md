# Water — HEIGHTFIELD design (shallow-water columns)

Assigned approach: **1-D shallow-water height field**. N vertical columns
across the round face; each column carries a water *depth* and a *flux
velocity*; the surface evolves under tilt-projected gravity via the
staggered upwind shallow-water equations. Rendered as a filled wave of
tiny neon squares with a bright meniscus. A small pool of ballistic
**spray** particles is spawned on a flick (jerk impulse) so the liquid can
still break and throw droplets. Everything is integer / fixed-point; the
physics is nearly free and the render + full-frame flush set the fps.

This document is buildable against the real repo (`src/scenes/apps.rs`
helpers, `src/scenes/wheel.rs` LUT idiom, `src/drivers/qmi8658.rs`,
`src/app.rs` run loop, `build.rs` generators). Signatures below match those
files as of this commit.

Geometry constants used throughout (from the repo): `W = H = 466`,
`CX = CY = 233`, interior wall radius `R_WALL = 226` (`R² = 51076`,
`2R = 452`), status clock band `y ∈ 26..66`.

---

## 0. Why a height field on a round face (honest up-front framing)

A 1-D height field has exactly **one slosh axis**. We fix that axis to
screen-**x** (wrist *roll*), because rolling the wrist is the gesture a
watch wearer performs most and it is the one that reads as "the water ran
to the side and climbed the wall." Consequences, stated plainly so the
rest of the design is read with them in mind:

- **Roll (screen-x tilt):** full, beautiful behaviour — runs downhill,
  climbs the far wall, sloshes, overshoots, settles. This is the money
  shot and the height field nails it cheaply.
- **Pitch (screen-y tilt):** *not* a slosh direction. Its in-plane
  component only modulates wave stiffness / splashiness; water does not
  visibly run up/down the screen. (A height field indexed by x physically
  cannot — see §7.)
- **Past vertical / inversion:** the surface is single-valued in x, so it
  cannot tumble, overhang, or sit on the ceiling. As |roll| → 90° the
  model *saturates* (piles into the downhill edge columns and clamps at
  the chord) rather than pouring over. Spray masks the flick; a sustained
  inversion looks like a filled wedge, not tumbling liquid.

The height field's compensating superpowers: it is **unconditionally
mass-conserving and incompressible by construction** (no pressure-grid
relaxation pass, unlike the particle plan), it **cannot blow up or leak**
when the four bounds in §7 hold, and it costs almost nothing (§6), leaving
the whole CPU budget for a rich render.

---

## 1. Fixed-point scheme + overflow proof

All hot-loop state is `i32`. Formats:

| Quantity | Symbol | Format | Range (clamped) | Notes |
|---|---|---|---|---|
| Column water depth | `d[i]` | **Q8 px** | `0 .. cap[i]·256` ≤ `452·256 = 115_712` | depth above the disc floor |
| Face flux velocity | `u[i]` | **Q8 px/frame** | `−U_MAX .. U_MAX`, `U_MAX = 1024` (=4 px/frame) | staggered: at face between col i,i+1 |
| Surface screen-y | `ys[i]` | **Q8 px** | `0 .. 466·256 = 119_296` | `= (yf[i]<<8) − d[i]` |
| Column floor / chord | `yf[i]`,`cap[i]` | **px (i16)** | `yf ≤ 459`, `cap ≤ 452` | precomputed once at open |
| Normalised gravity | `gx_n,gy_n,gz_n` | **Q8 (256 = 1 g)** | `−512 .. 512` (±2 g headroom) | from raw ±4 g accel |
| Spray pos / vel | `px,py,vx,vy` | **Q6 (i16)** | pos `≤ 466·64 = 29_824` | ballistic droplets |

`U_MAX = 1024` (4 px/frame) is chosen so the **CFL number**
`U_MAX·dt/dx = 4 px / 4 px = 1.0` with `dt = 1 frame`, `dx = CW = 4 px`:
water never moves more than one cell per frame ⇒ first-order upwind is
stable.

### Every hot-loop multiply, worst case → fits i32 (`i32::MAX = 2_147_483_647`)

1. **Gravity normalise** (per frame, ×3):
   `gx_n = (raw_debiased · 256) / 8192`.
   `raw_debiased ∈ [−32767, 32767]` (±4 g full scale). Product
   `32767 · 256 = 8_388_352` — **0.4 % of i32**. ✓

2. **Pressure / leveling term** (per face, N times):
   `a_press = (K_LEVEL · dsurf_q8) >> 8`, `dsurf_q8 = ys[i+1] − ys[i]`.
   `|dsurf_q8| ≤ 119_296` (Q8), `K_LEVEL ≤ 64`. Product
   `64 · 119_296 = 7_634_944` — **0.36 % of i32**. ✓

3. **Lateral gravity term** (per face): `a_grav = (GRAV_A · gx_n) >> 8`.
   `GRAV_A ≤ 256`, `|gx_n| ≤ 512`. Product `256 · 512 = 131_072`. ✓

4. **Damping** (per face): `u = (u · DAMP) >> 8`, `DAMP ≤ 256`,
   `|u| ≤ 1024`. Product `1024 · 256 = 262_144`. ✓

5. **Upwind flux** — *the worst one* (per face):
   `F = (u · d_upwind) >> 8`. `|u| ≤ 1024` (Q8), `d_upwind ≤ 115_712`
   (Q8). Product `1024 · 115_712 = 118_489_088` — **5.5 % of i32**,
   ~18× margin. ✓ (This is why `d` is capped at the chord and `u` at
   `U_MAX`; both caps are load-bearing for the proof.)

6. **Continuity**: `d[i] += (F[i−1] − F[i]) >> 2` (the `>>2` folds in
   `1/dx = 1/4`). Operands are Q8 fluxes ≤ ~1.2e8; sum fits. ✓

7. **Volume nudge**: `vol = Σ d[i]`, `N = 116`, each `≤ 115_712` ⇒
   `Σ ≤ 13_422_592` — **0.6 % of i32**. Correction is an *additive*
   per-column nudge (no multiply), so no product to bound. ✓

8. **Spray integrate** (per particle, ≤64): `vy += g_q6`,
   `py += vy`; all Q6, magnitudes < 30_000. ✓

9. **Render LUT index** (per cell): index math is byte-scale
   (`idx ∈ 0..255` plus small `>>8` scaled offsets). The only multiply is
   `(speed · K_SPD) >> 8` with `speed ≤ 1024`, `K_SPD ≤ 256` ⇒
   `262_144`. ✓

No intermediate exceeds ~1.2e8; **no i64 is required anywhere**. The two
caps (`U_MAX`, `d ≤ cap`) are the guardrails that keep multiply #5 inside
i32 *and* keep the sim stable — one mechanism, two payoffs.

---

## 2. State layout + SRAM byte count

Lives as a nested struct inside `apps::State` (internal SRAM — random
access every frame, never the PSRAM framebuffer). `const fn new()` so it
fits the existing `State::new()` const initializer.

```rust
const N_COLS: usize = 116;      // 4-px columns across the 464-px span
const CW: i32 = 4;              // column / cell pitch (px)
const SPRAY_MAX: usize = 64;    // droplet pool

pub struct WaterSim {
    // --- dynamic field (Q8) ---
    d:   [i32; N_COLS],   // depth per column        464 B
    u:   [i32; N_COLS],   // flux velocity per face  464 B
    // --- geometry cache (px, i16), computed once in reset() ---
    yf:  [i16; N_COLS],   // floor screen-y          232 B
    cap: [i16; N_COLS],   // chord height (max depth) 232 B
    // --- spray droplets (Q6, i16) ---
    sx:  [i16; SPRAY_MAX], sy: [i16; SPRAY_MAX],   // 128 + 128 B
    svx: [i16; SPRAY_MAX], svy:[i16; SPRAY_MAX],   // 128 + 128 B
    slife:[u8; SPRAY_MAX],                          //  64 B
    // --- scalars / calibration ---
    gx_n: i32, gy_n: i32, gz_n: i32,               // live gravity (Q8)
    bias_x: i32, bias_y: i32, bias_z: i32,         // rest offset (raw)
    last_ax: i16, last_ay: i16,                    // for jerk
    target_vol: i32,                               // conserved volume
    breathe: i32,                                  // breathing phase
    rng: u32,                                       // spray jitter
    calib_n: u8, seeded: bool, spray_n: u8,
    last_bbox: (i16, i16, i16, i16),               // clear region
}
```

Byte total: `464+464+232+232 + (128·4+64) + ~48 ≈ **2 096 B ≈ 2.05 KiB**`.
Comfortably inside the "few KB" internal-SRAM budget, and it is one static
instance (State is owned once by the run loop). Every array is
const-initializable (`[0i32; N]`, `[0i16; N]`, `[0u8; N]`).

---

## 3. Per-frame update

### 3a. Gravity from the IMU — tunable axis map + rest calibration

The IMU→screen mapping is unknown until measured on the board (like the
touch Y-flip). It is exposed as consts so first-flash tuning is a const
edit:

```rust
// Which raw accel axis feeds screen +x (the slosh axis) and screen +y,
// and their signs. Measured on-device: tilt in each screen direction,
// watch the logged (ax,ay,az), pick the two in-plane axes + signs.
const IMU_X_AXIS: usize = 0;   // 0=ax 1=ay 2=az
const IMU_X_SIGN: i32   = 1;
const IMU_Y_AXIS: usize = 1;
const IMU_Y_SIGN: i32   = 1;
const IMU_Z_AXIS: usize = 2;   // out-of-plane: flat-vs-upright + splash gate
```

Per frame the run loop reads `qmi8658::read_accel(&mut self.i2c)` (one
6-byte I²C read, ~0.2 ms) and hands the raw triple to `water_tick`. On a
bus error it passes `None` and the sim reuses `last_ax/last_ay` (gravity
holds; a dead IMU falls back to a fixed down-vector so the app still
works).

**Rest calibration**: for the first `CALIB_FRAMES = 8` ticks after open,
accumulate raw samples and store the mean as `bias_{x,y,z}`. Thereafter:

```rust
let raw = [ax, ay, az];
let rx = IMU_X_SIGN * (raw[IMU_X_AXIS] as i32 - bias_x);
let gx_n = (rx * 256 / ACC_LSB_PER_G).clamp(-512, 512);   // Q8, 256 = 1g
// gy_n, gz_n likewise. ACC_LSB_PER_G = 8192 (from the driver).
```

(A "recalibrate" affordance — a corner tap — can re-enter the calib state;
nice-to-have, not required.)

### 3b. Physics core — staggered upwind shallow water

Textbook stable form: depth `d` at cell centers, velocity `u` at faces.

```
for each interior face i (0 .. N-2):
    dsurf = ys[i+1] - ys[i]                         // Q8, screen-y (down+)
    a = (K_LEVEL*dsurf >> 8)                         // leveling (flatten surface)
      + (GRAV_A*gx_n  >> 8)                          // lateral gravity (tilt)
      + breathe_force(i)                             // §3e rest ripple
    u[i]  = clamp( ((u[i]+a) * DAMP) >> 8, -U_MAX, U_MAX )

// walls: force the two disc-edge faces toward 0 (reflect, damped)
u[first_wet_face-1] = -(u[..]>>2); u[last_wet_face] = -(u[..]>>2);

for each interior face i:
    d_up = if u[i] >= 0 { d[i] } else { d[i+1] }     // upwind
    F[i] = (u[i] * d_up) >> 8                         // flux, Q8

for each cell i:
    d[i] = clamp( d[i] + ((F[i-1] - F[i]) >> 2), 0, (cap[i] as i32) << 8 )
    ys[i] = ((yf[i] as i32) << 8) - d[i]
```

- **Leveling term** drives the surface flat (incompressibility for free —
  water spreads until `ys` is level; deeper where the round floor is
  lower ⇒ the correct lens/segment shape). No relaxation pass needed.
- **Lateral gravity** biases the equilibrium to a *tilted* flat line:
  balance is `dsurf = −GRAV_A·gx_n / K_LEVEL`, i.e. surface slope ∝ tilt.
  That tilted deep wedge **is** the far-wall climb.
- **Clamp `d ∈ [0, cap]`** is the round wall (§3c) and the anti-singularity
  guarantee in one line.
- **Damping < 1** bleeds energy every frame ⇒ always settles, never grows.

### 3c. Round-wall boundary (no leaks, energy damping)

The round vessel is entirely encoded in the precomputed `yf[i]`, `cap[i]`
(`cap[i] = 2·isqrt(R² − (x_i−CX)²)`; 0 outside the disc):

- **Floor & top rim**: `d` is clamped to `[0, cap[i]<<8]`. Hitting `cap`
  = the crest touched the far/top wall; the excess momentum is dropped
  (and, on a flick, converted to spray — §3d). Hitting `0` = column ran
  dry.
- **Left/right rim**: columns with `cap==0` are skipped; the outermost
  *wet* faces are **reflecting**: `u` is reversed and quartered
  (`>>2`, heavy loss) so water bounces off the rim losing energy instead
  of fluxing out of the disc. Flux only ever moves between two valid
  interior faces ⇒ the transport topology is closed ⇒ **cannot leak**.
- **Volume conservation nudge**: `vol = Σ d[i]`; if it drifts from
  `target_vol` (set at seed) by more than `ε`, spread the difference as a
  tiny per-column additive nudge weighted by `cap[i]`. Guarantees the pool
  neither drains nor floods from accumulated clamp rounding.

### 3d. Jerk → slosh impulse + spray

```rust
let jerk = (ax - last_ax).abs() + (ay - last_ay).abs();   // raw LSB
if jerk > JERK_THRESH {                                    // ~1500 LSB ≈ 0.18g
    let kick = (IMU_X_SIGN*(ax as i32 - last_ax as i32)) * K_KICK >> 8;
    for f in u.iter_mut() { *f = (*f + kick).clamp(-U_MAX, U_MAX); }
    // emit droplets from the fastest wet crests, budget per frame
    spawn_spray(jerk);
}
```

`spawn_spray` picks the few columns with the largest `|u[i]|` (or those
clamped at `cap`, i.e. crests that hit a wall), and emits up to
`SPRAY_PER_FLICK = 12` droplets at `(x_i, ys[i])` with velocity
`(u[i]/2 , −POP − (jerk-scaled))` plus RNG jitter. Droplets are ballistic:

```
vy += G_SPRAY;  px += vx; py += vy;  life -= 1;
// die when life==0 OR py falls back below the local surface ys (re-absorbed,
// with a tiny +d splash-back added to that column so volume is conserved).
```

`SPRAY_MAX = 64` is a hard ceiling (ring-buffer reuse). This is the *only*
way the height field throws detached water — it is a deliberate bolt-on,
not emergent.

### 3e. Rest breathing ripple

Always-on, so the liquid is alive even flat and still (the signature
"never static" quality). A slow, low-amplitude standing wave forced into
the momentum:

```rust
// crate::trig::lut_sin_cos_q14 is the repo's Q14 sine.
fn breathe_force(i: usize, phase: i32) -> i32 {
    let ph = phase + (i as i32) * K_SPACE;         // spatial wavelength
    (BREATHE_AMP * trig::lut_sin_cos_q14(ph)) >> 20 // ~±1 px surface motion
}
```

`phase` advances ~1 rev / 5 s (shared-tempo breathing, like the wheel
ring). Amplitude is tuned to ≈1 px so it is a gentle shimmer, swamped by
real tilt but visible at rest. (Aurora drift, §4, adds a second slow life
to the *colour* independent of the surface motion.)

---

## 4. Rendering

Signature look: a **dot-matrix of tiny neon-blue squares**, brighter
crests, deeper indigo body, a bright meniscus surface line, slow aurora
drift, on black AMOLED. The atomic unit is a **cell** = one column ×
4-px vertical step, drawn as a **3×3 lit square in a 4-px grid** (1-px
dark gutter → reads unmistakably as tiny squares).

**One LUT colour per cell (the cost trick).** Instead of per-pixel
gradient math, each cell computes a single `WATER_LUT` index and stamps
its 3×3 block with the two raw RGB565-BE bytes. Dithering happens at
**cell granularity** (a 4×4 Bayer offset on the index) — the cell *is* the
visual unit, so cell-level dither is exactly right and costs one add.

Per column `i` (skip if `cap[i]==0` or `d[i]==0`):

```
surf_y = ys[i] >> 8                       // integer surface row
floor_y = yf[i]
speed  = u_at_col(i).abs()                // avg of adjacent faces
for cell_y in (surf_y ..= floor_y).step_by(4):
    depth_px = cell_y - surf_y            // 0 at surface, grows downward
    idx = SURF_IDX
        - ((depth_px * K_DEPTH) >> 5)     // deeper → lower (indigo)
        + ((speed   * K_SPD)   >> 8)      // faster → brighter
        + aurora(i, cell_y)               // NOISE drift, §below
        + BAYER4[cell_y & 3][x_i & 3]     // cell-level dither
    if cell_y < surf_y + 4 { idx = MENISCUS_IDX }   // bright surface line
    let (hi, lo) = WATER_LUT[idx.clamp(0,255) as usize];
    fill_cell(fb, x_i, cell_y, 3, hi, lo);          // 3×3 raw store
```

- **WATER_LUT** (build-time, §5f): 256-entry deep-indigo → cyan → neon
  white in RGB565-BE `(hi,lo)`, Bayer-friendly. Same doctrine as
  `wheel::saber_lut` / `azure_lut`, but a static table so we can stamp raw
  bytes (no per-pixel repack).
- **Meniscus**: the top cell-row of every column is forced to
  `MENISCUS_IDX` (near-white cyan) — a continuous bright water line that
  tilts with the surface. Optionally a 1-px `max_px` highlight exactly on
  `ys[i]` for extra catch-light.
- **Aurora drift**: `aurora(i,y) = ((NOISE_A[(i + t1) & 255] as i32
  + NOISE_B[(y_cell + 512 - t2) & 255] as i32) - 256) * K_AUR >> 8`,
  reusing `lock::NOISE_A/NOISE_B` (already in the crate) with slowly
  advancing `t1,t2` — a luminous shimmer creeps through the body, tying
  Water into the OS aurora language.
- **Spray**: each live droplet is one 3×3 `fill_cell` at a LUT index driven
  by `life` (fresh = white-cyan, fading = dim), giving thrown light.

**Damage / flush story — confirmed FULL-FRAME.** The liquid is a large,
fully-dynamic region whose surface spans the width and moves every frame;
partial damage would approach the whole face anyway, so this is a
full-frame-flush app (`~13 ms`, the ~40 MB/s wire floor from
PERF-DUALCORE-PLAN). Each frame:

1. `fill_rect_black` the **tracked `last_bbox`** (union of last frame's
   wet cells + spray). At rest this is just the shallow pool (cheap); at
   full tilt it is the lower disc.
2. Render body cells + meniscus + spray; accumulate the new bbox.
3. **`wheel::draw_status(fb, now, batt)` last** — the status clock is
   stamped on top every frame, so even if a crest or a droplet climbs into
   `y 26..66` it is immediately overpainted and the clock stays topmost
   and intact.
4. `wfb.mark_rect(0,0,W-1,H-1)` → one full flush.

(Optimization noted, not required: `mark_rect(last_bbox ∪ new_bbox)` +
partial flush would cut rest-frame flush time, but tilt frames go
full-frame regardless. This is the app the plan says most wants **P1
async flush**, which would overlap the 13 ms flush with the next frame's
render and lift the ceiling toward the render bound.)

---

## 5. Exact integration

### 5a. Fields added to `apps::State`

```rust
pub struct State {
    // ... existing fields ...
    pub water: WaterSim,
}
impl State {
    pub const fn new() -> Self {
        Self { /* ... existing ... */, water: WaterSim::new() }
    }
}
```

### 5b. `has_content`

```rust
pub fn has_content(idx: usize) -> bool {
    idx != /* nothing */ usize::MAX   // Water now has content:
}                                     // simply: `true` for all, delete the WATER exception.
```
Change the current `idx != WATER` to `true` (every app has content). This
flips Water from "rest on splash" to the content-reveal path.

### 5c. `draw_reveal` WATER branch (fill-in on open)

During the morph reveal, draw a **rising flat pool** (cheap: fill the
bottom disc segment up to level fraction `q`) and mark the sim unseeded;
the first `water_tick` seeds the field and takes over. Add to
`draw_reveal`:

```rust
} else if idx == WATER {
    {
        let fb = wfb.buf_mut();
        // rising pool: flat surface at fraction q of the resting level
        st.water.draw_fill_in(fb, q_q8);
        wheel::draw_status(fb, now, batt);   // clock stays on top
    }
    st.water.seeded = false;                  // first tick seeds + animates
    fx.push(0, 60, W - 1, H - 1);
    wfb.mark_rect(0, 60, W - 1, H - 1);
}
```

### 5d. Rest tick — the run-loop hook (minimal `app.rs` diff)

Water needs live gravity, which needs `self.i2c` (owned by the run loop).
Following the plan's recommended **option 1** (read in the loop, pass into
the tick), special-case Water at the existing tick call site (~line 322):

```rust
// src/app.rs, in the Scene::App(idx) / Power::Awake block:
} else if idx == apps::WATER {
    // Live gravity from the IMU; on a bus fault pass None (sim holds).
    let g = qmi8658::read_accel(&mut self.i2c).ok();
    apps::water_tick(&mut self.wfb, g, &now, wheel_batt, elapsed, &mut app_state);
} else {
    apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
    if apps::shows_status(idx) && status_minute != now.minute {
        status_minute = now.minute;
        wheel::tick_status(&mut self.wfb, &now, wheel_batt);
    }
}
```

(Water restamps the status clock itself every frame — §4 — so it needs no
minute-rollover branch.) Two more one-liners in `app.rs`:

```rust
// (i) import, top of file:
use openpocket::drivers::qmi8658;   // (or add qmi8658 to the existing drivers use)

// (ii) run Water at 40 fps (25 ms), not the 50 ms app-rest cadence:
let frame_us = match power {
    Power::Aod | Power::Sleep => IDLE_FRAME_US,
    _ if scene == Scene::Locked && self.clock.is_animating() => CLOCK_ANIM_FRAME_US,
    _ if scene == Scene::Wheel && self.wheel_fx.intro_active() => ANIM_FRAME_US,
    _ if matches!(scene, Scene::App(apps::WATER)) => ANIM_FRAME_US,   // ← added
    _ => FRAME_US,
};

// (iii) seed on open — in open_app, beside the existing TIME reset:
if idx == apps::WATER { st.water.reset(); }
```

That is the entire `app.rs` footprint: one import, one `else if` tick
branch, one cadence line, one open-reset line. `apps` stays I²C-free (the
raw triple crosses the boundary, not the bus).

### 5e. New public surface in `apps.rs`

```rust
pub fn water_tick(
    wfb: &mut WatchFb,
    g: Option<(i16, i16, i16)>,   // raw accel, or None on bus fault
    now: &WallTime,
    batt: Option<u8>,
    elapsed_ms: u32,
    st: &mut State,
) { st.water.tick(wfb, g, now, batt, elapsed_ms); }
```

### 5f. `build.rs` WATER_LUT generator

Mirrors `generate_noise_luts` / the wheel-asset emit idiom; writes a static
RGB565-BE `(hi,lo)` table, `include!`d in `apps.rs`.

```rust
// build.rs
fn generate_water_lut() {
    // 4 stops: deep indigo → mid blue → cyan → neon white.
    // (r,g,b) 0..255 control points at t = 0, 0.45, 0.8, 1.0
    let stops = [(4, 6, 40), (0, 60, 150), (40, 200, 255), (210, 245, 255)];
    let lerp = |a: i32, b: i32, t: i32, d: i32| a + (b - a) * t / d;
    let mut body = String::from(
        "/// Auto-generated by build.rs (generate_water_lut). RGB565-BE (hi,lo).\n\
         pub static WATER_LUT: [(u8,u8); 256] = [",
    );
    for i in 0..256i32 {
        // piecewise across the 4 stops
        let (r, g, b) = if i < 115 {
            let t = i;               let d = 115;
            (lerp(stops[0].0,stops[1].0,t,d), lerp(stops[0].1,stops[1].1,t,d), lerp(stops[0].2,stops[1].2,t,d))
        } else if i < 205 {
            let t = i-115;           let d = 90;
            (lerp(stops[1].0,stops[2].0,t,d), lerp(stops[1].1,stops[2].1,t,d), lerp(stops[1].2,stops[2].2,t,d))
        } else {
            let t = i-205;           let d = 51;
            (lerp(stops[2].0,stops[3].0,t,d), lerp(stops[2].1,stops[3].1,t,d), lerp(stops[2].2,stops[3].2,t,d))
        };
        let r5 = ((r.clamp(0,255) as u16) * 31 / 255) & 0x1F;
        let g6 = ((g.clamp(0,255) as u16) * 63 / 255) & 0x3F;
        let b5 = ((b.clamp(0,255) as u16) * 31 / 255) & 0x1F;
        let px = (r5 << 11) | (g6 << 5) | b5;
        if i > 0 { body.push(','); }
        body.push_str(&format!("({},{})", (px >> 8) as u8, px as u8));
    }
    body.push_str("];\n");
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&out).join("water_lut.rs"), body).unwrap();
}
// call generate_water_lut(); from main(), and in apps.rs:
//   include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));
```

`SURF_IDX ≈ 210`, `MENISCUS_IDX ≈ 250` index into the bright cyan/white
end; `depth` pulls toward the indigo end. (Aurora is emitted separately —
reuse `lock::NOISE_A/NOISE_B`, no new asset.)

---

## 6. Per-frame ops + ms budget

Target cadence **25 ms (40 fps)**; flush is fixed at **~13 ms**, leaving
**~12 ms** for clear + physics + render.

**Physics** (N = 116 faces/cells, 64 spray):
- momentum loop: 116 × ~10 ops ≈ 1 160
- flux + continuity: 116 × ~10 ops ≈ 1 160
- volume nudge + breathe: 116 × ~5 ≈ 580
- spray integrate: 64 × ~12 ≈ 770
- gravity/calib/jerk: ~60
- **≈ 3.7 k int ops ⇒ < 0.2 ms.** Physics is essentially free — the
  height field's headline advantage.

**Render** (worst case = full roll, water fills ~half the disc):
- clear `last_bbox`: worst ≈ 456×230 px sequential PSRAM memset ≈
  **1–2 ms** (rest: just the pool, ~0.4 ms).
- body cells: worst area ≈ 80 000 px / (4·4) ≈ **5 000 cells**; per cell =
  1 LUT index (~6 ops) + 9 raw stores ≈ 15 ⇒ 75 k ops ≈ **~1.5–2.5 ms**.
- meniscus (116 cells) + spray (≤64) + aurora sampling: **< 0.5 ms**.
- `draw_status` restamp: **~0.4 ms**.
- **Render+clear ≈ 4–6 ms worst; ~2–3 ms at rest.**

**Frame total**: physics 0.2 + render/clear 4–6 + flush 13 ≈ **17–19 ms**
→ **40 fps sustained with headroom**; the pathological fully-tilted-and-
sloshing frame stays ≤ ~20 ms (still ≥ 40 fps). IMU read (~0.2 ms) is in
the loop, not the render path.

**The call:** `N = 116` columns (4-px), `64` spray droplets,
`~5 000` body cells worst case → **40 fps** at the 25 ms cadence. The
render is deliberately cheap (cell-granular LUT + raw stores) so the
flush, not the CPU, is the ceiling — exactly the app that most benefits
from **P1 async flush** (which would push toward the render bound, ~55+ fps
equivalent work headroom).

---

## 7. Stability guarantees + honest weaknesses

**Why it never blows up / leaks / piles to a singularity** — four bounded
invariants, each a single clamp/topology fact:

1. **`u` clamped to `±U_MAX` (CFL = 1.0).** No cell can transfer more than
   its own contents in a frame ⇒ upwind advection is stable; also bounds
   multiply #5 inside i32.
2. **`d` clamped to `[0, cap[i]]`.** No negative depth, no depth beyond the
   physical chord ⇒ **no singular pile-up**; the round wall is the clamp
   itself.
3. **`DAMP < 256` every frame.** Total energy strictly decreases without
   forcing ⇒ always **settles to rest**, never grows unbounded. Breathing
   is a *bounded* forcing (±1 px), swamped by damping at any real
   amplitude.
4. **Closed flux topology + reflecting edges.** Water only moves between
   two valid interior faces; edge faces are forced to reflect ⇒ the disc
   is a sealed vessel ⇒ **cannot leak through the wall**. The volume nudge
   corrects clamp-rounding drift so it can neither drain nor overflow.

Together these make the state space compact and contracting — divergence
is impossible by construction, not by tuning.

**Honest weakness list (height-field specific):**

1. **One slosh axis.** Rich response to *roll* (screen-x); *pitch*
   (screen-y) only stiffens the water, no front/back run. A true 2-D tilt
   in an arbitrary direction is projected onto x only. (This is the
   fundamental price of 1-D; the particle approach in the plan does not
   pay it.)
2. **Single-valued surface — no past-vertical.** Cannot overhang, break,
   invert, or sit on the ceiling. Near |roll| → 90° it *saturates* into a
   filled edge wedge rather than pouring over the top. Sustained inversion
   looks wrong.
3. **Droplets are bolted on.** Spray particles are the only detached
   water; there are no large flung blobs, and re-absorption is
   approximate. Turn the flick threshold too low and spray looks noisy;
   too high and a hard shake under-reacts.
4. **"Liquid" is a shaded fill, not a field of bodies.** The tiny-squares
   look comes from the *render grid*, not the physics; the body can read
   as luminous gel/matrix rather than granular water. Meniscus and surface
   tension are *drawn*, not emergent.
5. **Roll-axis choice is a product bet.** We assume wrist-roll is the
   dominant gesture. If user testing shows people tilt the watch away/
   toward themselves to "pour," this model under-delivers and would want
   the particle approach (or a gravity-aligned rotating grid, which we
   rejected: re-projecting to gravity-space each frame silently kills the
   slosh transient — see §0).

Net: within the ~12 ms compute budget the height field delivers a
gorgeous, rock-solid, always-alive roll-slosh liquid at 40 fps for almost
no CPU, at the cost of one slosh axis and no genuine past-vertical
behaviour.

---

## 8. Core update + render code sketch (real bodies, repo helpers)

Uses the existing `apps.rs` integer helpers (`set_px`, `max_px`,
`fill_rect_black`, `isqrt`), the `WATER_LUT` static, `wheel::draw_status`,
`lock::NOISE_A/NOISE_B`, and `crate::trig`.

```rust
// ---- tunables (first-flash: edit these, not the code) ----
const K_LEVEL:   i32 = 40;    // surface-flattening stiffness
const GRAV_A:    i32 = 220;   // lateral-gravity strength
const DAMP:      i32 = 250;   // /256 per frame (energy bleed)
const U_MAX:     i32 = 1024;  // Q8 px/frame  (CFL = 1.0 at CW=4)
const K_KICK:    i32 = 300;   // jerk → flux impulse
const JERK_THRESH: i32 = 1500;
const G_SPRAY:   i16 = 26;    // Q6 px/frame² droplet gravity
const SURF_IDX:  i32 = 210;
const MENISCUS_IDX: i32 = 250;
const K_DEPTH:   i32 = 40;
const K_SPD:     i32 = 90;
const K_AUR:     i32 = 60;
const NEON: (i32,i32,i32) = (0, 190, 255);  // accent(WATER)
const BAYER4: [[i32;4];4] =
    [[-24,8,-18,14],[20,-12,26,-6],[-14,16,-20,10],[30,-2,24,-8]];

impl WaterSim {
    pub const fn new() -> Self { /* zeroed arrays, seeded:false */ }

    /// Precompute round-vessel geometry + reset dynamic state (on open).
    pub fn reset(&mut self) {
        let x0 = CX - (N_COLS as i32 * CW) / 2;      // = 1
        for i in 0..N_COLS {
            let x = x0 + i as i32 * CW + CW / 2;      // column center x
            let dx = x - CX;
            let half = if dx*dx < R2 { isqrt((R2 - dx*dx) as u32) as i32 } else { 0 };
            self.yf[i]  = (CY + half) as i16;
            self.cap[i] = (2 * half) as i16;
            self.d[i] = 0; self.u[i] = 0;
        }
        self.seeded = false; self.calib_n = 0; self.spray_n = 0;
        self.bias_x = 0; self.bias_y = 0; self.bias_z = 0;
    }

    /// Seed a resting pool of ~POOL_FRAC of the disc, flat surface.
    fn seed(&mut self) {
        let surf = CY + 46;                          // resting waterline (px)
        let mut vol = 0i32;
        for i in 0..N_COLS {
            let cap = self.cap[i] as i32;
            let depth = ((self.yf[i] as i32 - surf).max(0)).min(cap);
            self.d[i] = depth << 8;
            vol += self.d[i];
        }
        self.target_vol = vol;
        self.seeded = true;
    }

    pub fn tick(&mut self, wfb: &mut WatchFb, g: Option<(i16,i16,i16)>,
                now: &WallTime, batt: Option<u8>, elapsed_ms: u32) {
        // 1) gravity (axis map + rest calibration)
        if let Some((ax,ay,az)) = g {
            let raw = [ax as i32, ay as i32, az as i32];
            if self.calib_n < 8 {                    // capture rest bias
                self.bias_x += raw[IMU_X_AXIS]; self.bias_y += raw[IMU_Y_AXIS];
                self.bias_z += raw[IMU_Z_AXIS]; self.calib_n += 1;
                if self.calib_n == 8 {
                    self.bias_x /= 8; self.bias_y /= 8; self.bias_z /= 8;
                }
            }
            let rx = IMU_X_SIGN * (raw[IMU_X_AXIS] - self.bias_x);
            let ry = IMU_Y_SIGN * (raw[IMU_Y_AXIS] - self.bias_y);
            self.gx_n = (rx * 256 / ACC_LSB_PER_G).clamp(-512, 512);
            self.gy_n = (ry * 256 / ACC_LSB_PER_G).clamp(-512, 512);
            // jerk
            let jerk = (ax - self.last_ax).abs() as i32 + (ay - self.last_ay).abs() as i32;
            self.last_ax = ax; self.last_ay = ay;
            if self.seeded && jerk > JERK_THRESH { self.apply_jerk(ax as i32, jerk); }
        }
        if !self.seeded { self.seed(); }

        self.step_physics(elapsed_ms);
        self.render(wfb, now, batt, elapsed_ms);
    }

    fn step_physics(&mut self, elapsed_ms: u32) {
        let phase = (elapsed_ms as i32 * trig::TAU_Q14 / 5000) % trig::TAU_Q14;
        let a_grav = (GRAV_A * self.gx_n) >> 8;

        // momentum at faces
        for i in 0..N_COLS-1 {
            if self.cap[i] == 0 || self.cap[i+1] == 0 { self.u[i] = 0; continue; }
            let ys_i  = ((self.yf[i]   as i32) << 8) - self.d[i];
            let ys_i1 = ((self.yf[i+1] as i32) << 8) - self.d[i+1];
            let a_press = (K_LEVEL * (ys_i1 - ys_i)) >> 8;
            let breathe = (BREATHE_AMP *
                trig::lut_sin_cos_q14(phase + i as i32 * K_SPACE)) >> 20;
            let un = ((self.u[i] + a_press + a_grav + breathe) * DAMP) >> 8;
            self.u[i] = un.clamp(-U_MAX, U_MAX);
        }

        // upwind flux + continuity (double-buffer d via a running delta)
        let mut f_prev = 0i32;
        for i in 0..N_COLS {
            let f_i = if i < N_COLS-1 && self.cap[i] != 0 && self.cap[i+1] != 0 {
                let d_up = if self.u[i] >= 0 { self.d[i] } else { self.d[i+1] };
                (self.u[i] * d_up) >> 8
            } else { 0 };
            let cap_q8 = (self.cap[i] as i32) << 8;
            self.d[i] = (self.d[i] + ((f_prev - f_i) >> 2)).clamp(0, cap_q8);
            f_prev = f_i;
        }

        // volume conservation nudge (additive, no multiply)
        let vol: i32 = self.d.iter().sum();
        let err = self.target_vol - vol;             // signed
        if err.abs() > (N_COLS as i32) {
            let per = err / (N_COLS as i32);
            for i in 0..N_COLS {
                if self.cap[i] != 0 {
                    self.d[i] = (self.d[i] + per).clamp(0, (self.cap[i] as i32) << 8);
                }
            }
        }
        self.step_spray();
    }

    fn render(&mut self, wfb: &mut WatchFb, now: &WallTime,
              batt: Option<u8>, elapsed_ms: u32) {
        let t1 = (elapsed_ms / 40) as i32;           // aurora scroll
        let t2 = (elapsed_ms / 55) as i32;
        // 1) clear last frame's footprint
        let bb = self.last_bbox;
        {
            let fb = wfb.buf_mut();
            fill_rect_black(fb, bb.0 as i32, bb.1 as i32, bb.2 as i32, bb.3 as i32);
        }
        let (mut nx0, mut ny0, mut nx1, mut ny1) = (W, H, 0, 0);
        {
            let fb = wfb.buf_mut();
            // 2) body cells + meniscus
            for i in 0..N_COLS {
                if self.cap[i] == 0 || self.d[i] == 0 { continue; }
                let x = CX - (N_COLS as i32*CW)/2 + i as i32*CW;
                let surf = (((self.yf[i] as i32) << 8) - self.d[i]) >> 8;
                let floor = self.yf[i] as i32;
                let uc = self.u[i.min(N_COLS-2)];
                let un = self.u[i.saturating_sub(1)];
                let speed = ((uc + un) / 2).abs();
                let mut cy = surf & !3;                // align to 4-px grid
                if cy < surf { cy += 4; }
                let mut first = true;
                while cy <= floor {
                    let depth_px = cy - surf;
                    let mut idx = SURF_IDX
                        - ((depth_px * K_DEPTH) >> 5)
                        + ((speed * K_SPD) >> 8)
                        + (((NOISE_A[((i as i32 + t1) & 255) as usize] as i32
                           + NOISE_B[((cy + 512 - t2) & 255) as usize] as i32)
                           - 256) * K_AUR >> 8)
                        + BAYER4[(cy & 3) as usize][(x & 3) as usize];
                    if first { idx = MENISCUS_IDX; first = false; }
                    let (hi, lo) = WATER_LUT[idx.clamp(0,255) as usize];
                    fill_cell(fb, x, cy, 3, hi, lo);
                    cy += 4;
                }
                nx0 = nx0.min(x-1); nx1 = nx1.max(x+3);
                ny0 = ny0.min(surf-1); ny1 = ny1.max(floor+1);
            }
            // 3) spray droplets
            for k in 0..SPRAY_MAX {
                if self.slife[k] == 0 { continue; }
                let px = (self.sx[k] as i32) >> 6;
                let py = (self.sy[k] as i32) >> 6;
                let idx = 150 + (self.slife[k] as i32).min(105);   // fade
                let (hi, lo) = WATER_LUT[idx.clamp(0,255) as usize];
                fill_cell(fb, px, py, 3, hi, lo);
                nx0 = nx0.min(px-1); nx1 = nx1.max(px+3);
                ny0 = ny0.min(py-1); ny1 = ny1.max(py+3);
            }
            // 4) status clock stamped topmost — never overwritten by liquid
            wheel::draw_status(fb, now, batt);
        }
        // 5) full-frame damage → one full flush; remember footprint to clear
        self.last_bbox = (nx0.max(0) as i16, ny0.max(0) as i16,
                          nx1.min(W-1) as i16, ny1.min(H-1) as i16);
        wfb.mark_rect(0, 0, W-1, H-1);
    }
}

/// Stamp an s×s block with one RGB565-BE colour (the render primitive).
#[inline]
fn fill_cell(fb: &mut [u8], x0: i32, y0: i32, s: i32, hi: u8, lo: u8) {
    for dy in 0..s {
        let y = y0 + dy;
        if y < 0 || y >= H { continue; }
        let row = (y * W) * 2;
        for dx in 0..s {
            let x = x0 + dx;
            if x < 0 || x >= W { continue; }
            let idx = (row + x * 2) as usize;
            if idx + 1 < fb.len() { fb[idx] = hi; fb[idx + 1] = lo; }
        }
    }
}
```

`apply_jerk` (kick every face + emit droplets from the fastest crests) and
`step_spray` (ballistic integrate, re-absorb below surface with a small
depth splash-back so volume is conserved) follow the §3d sketch; both are
< 1 k ops/frame. `NOISE_A/NOISE_B` are `lock::NOISE_A/NOISE_B`;
`trig::lut_sin_cos_q14`, `trig::TAU_Q14`, `ACC_LSB_PER_G`, `R2 = 51076`,
`BREATHE_AMP`, `K_SPACE` are consts/imports declared with the tunables.
```
```
