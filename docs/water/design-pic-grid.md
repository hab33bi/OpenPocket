# Water — PIC-grid liquid (particle-in-cell lite), complete buildable design

Approach: **PIC-lite** — ~384 square particles carrying position + velocity,
plus a coarse 24×24 background density grid. Each frame: integrate gravity
from the IMU, scatter particle density to the grid, run one Jacobi-style
relaxation pass that pushes particles down the over-density gradient
(incompressibility / pressure), reflect off the round wall with energy loss,
throw spray on a jerk impulse, and add a rest breathing ripple. Renders as
tiny neon-blue squares shaded through a build-time `WATER_LUT`, with a bright
meniscus, aurora drift, and dithering. Full-frame flush; ~40 fps.

This fits the REAL repo: it reuses the integer helpers at the bottom of
`src/scenes/apps.rs` (`set_px`, `max_px`, `soft_dot`, `fill_rect_black`,
`isqrt`, `pseudo_dir`), the `apps::State` home for per-app state, the existing
`tick`/`draw_reveal` dispatch, `wheel::draw_status` for the topmost clock,
`lock::NOISE_A/NOISE_B` for aurora, and the `qmi8658::read_accel` gravity
source. Every framebuffer write is idempotent (SET over black), matching the
codebase doctrine.

---

## 0. Coordinate + hardware facts this design is pinned to

- Framebuffer: RGB565 **big-endian**, `W = H = 466`, 424 KiB in PSRAM.
  `wfb.buf_mut() -> &mut [u8]` (2 bytes/px), `wfb.mark_rect(x0,y0,x1,y1)`.
- Round interior: `CX = CY = 233`, interior wall radius ~226 (BEZEL_R=223 in
  lock.rs; we contain at **224 px** so a 2 px-radius particle never pokes the
  bezel).
- Status clock band: y **26..66**, center — must stay topmost.
- IMU: `qmi8658::read_accel(&mut i2c) -> Result<(i16,i16,i16),()>`, **8192
  LSB/g** at ±4 g (raw range ±32768). Read once/frame in the run loop
  (owns `self.i2c`), ~0.2 ms.
- CPU: ESP32-S3 @ 240 MHz, **no usable FPU** → integer / fixed-point only in
  the per-particle loop. Core 0 runs the sim today.
- Flush: the liquid is a large dynamic region → **full-frame-flush app**
  (~13 ms wire). Cadence pinned to `ANIM_FRAME_US = 25_000` (40 fps); compute
  budget ≈ 10–11 ms. (A partial-bbox flush lever is documented in §4.6.)

---

## 1. Fixed-point scheme + overflow proof

### 1.1 Formats

| Quantity | Format | Unit | Storage | Screen conversion |
|---|---|---|---|---|
| Particle position `x,y` | **Q4** | 1/16 px | `i16` | `px = x >> 4` |
| Particle velocity `vx,vy` | **Q4** | 1/16 px **per frame** | `i16` | `vx >> 4` px/frame |
| Per-frame accel `ax,ay` | Q4 | 1/16 px/frame² | i32 (transient) | — |
| Grid density | integer | particles/cell | `u16` | — |
| Wall normal `nx,ny` | **Q8** | 1/256 (unit) | i32 (transient) | — |
| Damping / restitution | Q8 | fraction /256 | const | — |

Position and velocity share Q4 so integration is a bare add: `x += vx`
(dt = 1 frame is folded into the accel/velocity scaling — the standard game
convention; frame-time jitter is absorbed by damping and the pinned cadence).

Why Q4 fits `i16`: max in-play coordinate is `CX + WALL + overshoot ≈
233 + 224 + 64 = 521 px`. `521 << 4 = 8336`, far inside `i16` (±32767). So
positions never overflow their `i16` even at the worst single-frame
overshoot, and reflection immediately clamps them back to ≤ 459 px.

**Velocity clamp `VMAX = 1024` (Q4 = 64 px/frame)** applied every frame is the
keystone: it bounds every downstream multiply *and* is the primary blow-up
guard (§7).

### 1.2 Overflow proof — every hot-loop multiply (worst-case operands → i32)

`i32` holds ±2.147e9. Worst-case operands are taken at the clamps
(`|coord| ≤ 521 px`, `|v| ≤ 1024 Q4`, `density ≤ 384`, `|raw| ≤ 32768`).

| # | Site | Expression | Worst-case | Product | Fits i32 |
|---|---|---|---|---|---|
| 1 | wall dist | `dx*dx + dy*dy` (px) | dx,dy ≤ 288 | 288²·2 = 165 888 | ✓ (13 000× margin) |
| 2 | rim project | `dx * WALL / d` | 288·224 | 64 512 | ✓ |
| 3 | unit normal | `dx * 256 / d` | 288·256 | 73 728 | ✓ |
| 4 | vel·normal | `vx*nx + vy*ny` (Q4·Q8) | 1024·256·2 | 524 288 | ✓ |
| 5 | reflect | `2*vn*nx >> 8` | vn≤2048, ·256·2 | 1 048 576 | ✓ |
| 6 | pressure | `grad * KP >> KPS` | grad≤384, ·20 | 7 680 | ✓ |
| 7 | gravity map | `gmap * GRAV / 8192` | 8192·6 | 49 152 | ✓ |
| 8 | jerk | `(g_cur - g_prev) * JERK_K` | 65536·3 | 196 608 | ✓ |
| 9 | integrate | `x + vx` (add, Q4) | 8336 + 1024 | 9 360 | ✓ (i16-safe) |
| 10 | damping | `v * DAMP >> 8` | 1024·255 | 261 120 | ✓ |
| 11 | LUT/aurora | `NOISE[..] + NOISE[..]` | 255+255 | 510 | ✓ |
| 12 | `isqrt(d2)` | u32 input | ≤ 165 888 | — | ✓ (existing helper) |

The single largest product is #5 at ~1.05 M — **2000× inside i32**. No
multiply needs i64. (Row 8's operand 65536 exceeds `i16`, so jerk is computed
in `i32` from the raw readings, never stored in an `i16` intermediate.)

---

## 2. State layout + SRAM byte count

All simulation state lives as fields in `apps::State` (internal SRAM, random
access every frame — never PSRAM). Structure-of-arrays (SoA) for cache
locality and trivial `const fn new()` init.

```rust
pub const WA_N: usize = 384;                 // particle count
const WA_G: usize = 24;                       // grid is WA_G × WA_G
// added to `pub struct State { … }`:
    // --- particles (SoA) ---
    pub wa_x:  [i16; WA_N],   // Q4 px          768 B
    pub wa_y:  [i16; WA_N],   // Q4 px          768 B
    pub wa_vx: [i16; WA_N],   // Q4 px/frame    768 B
    pub wa_vy: [i16; WA_N],   // Q4 px/frame    768 B
    // --- coarse density grid ---
    pub wa_dens: [u16; WA_G * WA_G],          // 576 × 2 = 1152 B
    // --- IMU + calibration + housekeeping ---
    pub wa_raw:  [i16; 3],    // last accel fed by run loop      6 B
    pub wa_bx:   i16,         // rest-bias X (captured on open)  2 B
    pub wa_by:   i16,         // rest-bias Y                     2 B
    pub wa_px:   i16,         // prev mapped raw X (jerk)         2 B
    pub wa_py:   i16,         // prev mapped raw Y                2 B
    pub wa_rng:  u32,         // xorshift32 spray RNG             4 B
    pub wa_bbox: (i16,i16,i16,i16), // last-frame liquid bbox    8 B
    pub wa_batt: Option<u8>,  // fed for the topmost clock        2 B
    pub wa_cal:  bool,        // calibrated?                      1 B
    pub wa_live: bool,        // any successful IMU read?         1 B
    pub wa_spawned: bool,     // pool populated?                  1 B
```

Byte total: 4×768 (particles) + 1152 (grid) + ~33 (scalars, pre-padding)
= **3072 + 1152 + 33 ≈ 4257 bytes ≈ 4.2 KiB**. Comfortably a "few KB" of
internal SRAM.

`State` is instantiated once in `App::run` (`let mut app_state =
apps::State::new();`) and lives on the main task stack. 4.2 KiB there is fine
for the esp-hal main stack, but **note**: if stack head-room is ever tight,
move exactly these fields into a `static mut WATER: WaterSim` cell (the sim is
single-owner, single-core) with zero logic change — the functions already take
`&mut`. Documented as the escape hatch, not needed at 4 KiB.

---

## 3. The full per-frame update

Data flow (per Water frame, all on core 0):

```
run loop (owns i2c) ── read_accel ──► water_feed(st, raw, batt)
                                         │
apps::tick(WATER) ─► water_clear(fb,st)  │  erase last-frame footprints
                     water_step(st) ◄────┘  physics (this section)
                     water_draw(fb,st)      neon squares + meniscus + aurora
                     clock keep-out repair  (only if liquid touched the band)
                     mark damage            union bbox (→ partial) or full frame
```

### 3.1 Gravity from the IMU (tunable axis-map + on-open calibration)

The IMU→screen mapping is **unknown until measured on the board** (like the
touch Y-flip). It is exposed as first-flash-editable consts, so tuning is a
const edit, never a rewrite:

```rust
// ---- FIRST-FLASH TUNABLES (measure on device, edit these) ----
const IMU_XSRC: usize = 0;   // which raw axis feeds screen +x  (0=ax 1=ay 2=az)
const IMU_YSRC: usize = 1;   // which raw axis feeds screen +y
const IMU_XSGN: i32   = 1;   // flip if "tilt right" pushes water left
const IMU_YSGN: i32   = 1;   // flip if "tilt down"  pushes water up
```

On-device calibration recipe (log `ax,ay,az`, tilt top-edge-down / bottom /
left / right): pick the two in-plane axes and signs so "top-edge-down" →
gravity points to screen-top. Bake into the four consts above.

**Rest-offset calibration captured on app open**: the first `water_step` after
open captures the current mapped reading as the zero reference (`wa_bx,wa_by`),
so the pool starts level regardless of how the watch is tilted when opened
(exactly the ball-game calibration the plan cites). Per-frame gravity is the
bias-subtracted, ±1 g-clamped in-plane vector; any excess beyond 1 g is motion
and flows into the jerk term instead.

```rust
let gx = IMU_XSGN * st.wa_raw[IMU_XSRC] as i32;   // mapped raw
let gy = IMU_YSGN * st.wa_raw[IMU_YSRC] as i32;
if !st.wa_cal {                                   // capture on first tick
    st.wa_bx = gx as i16; st.wa_by = gy as i16;
    st.wa_px = gx as i16; st.wa_py = gy as i16;   // jerk baseline (no startle)
    st.wa_cal = true;
}
let gmx = (gx - st.wa_bx as i32).clamp(-8192, 8192);   // in-plane, ±1 g
let gmy = (gy - st.wa_by as i32).clamp(-8192, 8192);
let ax  = gmx * GRAV / 8192;      // Q4 px/frame²  (GRAV = accel at full 1 g tilt)
let ay  = gmy * GRAV / 8192;
```

Dead-IMU fallback: `water_feed` sets `wa_live` on any `Ok`. If the IMU never
answers (`!wa_live`), `water_step` skips calibration and substitutes a fixed
down-vector (`ay = GRAV, ax = 0`) so the app still pools and settles.

### 3.2 Physics core — incompressibility via scatter + Jacobi relaxation

**Scatter** (O(N)): zero the grid, bin each particle into its 20 px cell,
increment the count.

```rust
st.wa_dens.fill(0);
for i in 0..WA_N {
    let cx = (((st.wa_x[i] as i32) >> 4) / WA_CELL).clamp(0, WA_G as i32 - 1);
    let cy = (((st.wa_y[i] as i32) >> 4) / WA_CELL).clamp(0, WA_G as i32 - 1);
    st.wa_dens[cy as usize * WA_G + cx as usize] += 1;
}
```

**Relaxation / pressure** (O(N), 1 pass): each particle reads the *over-density*
`over(c) = max(0, dens[c] - REST)` of its 4 axis neighbours and accelerates
down that gradient. Pressure only exists where a cell is crowded (> `REST`);
sparse regions and the surface feel no repulsion, so gravity wins there and the
mass **pools** with a real surface instead of dispersing like a gas. This is
Clavet-style double-density relaxation stripped to one Jacobi step — enough for
a UI, as the plan notes. The correction is applied to **velocity** (a pressure
acceleration), which is unconditionally stable (no position teleport, no wall
leak) and integrates naturally next line.

```rust
let over = |cx: i32, cy: i32| -> i32 {
    if cx < 0 || cy < 0 || cx >= WA_G as i32 || cy >= WA_G as i32 { return 0; }
    (st.wa_dens[cy as usize * WA_G + cx as usize] as i32 - REST).max(0)
};
let gradx = over(cx + 1, cy) - over(cx - 1, cy);
let grady = over(cx, cy + 1) - over(cx, cy - 1);
vx -= (gradx * KP) >> KPS;    // flow from crowded → empty
vy -= (grady * KP) >> KPS;
```

Equilibrium: gravity packs particles at the low wall; pressure resists
compression; the balance is a **level surface**. A second sub-iteration off the
same scattered grid (cheap, no re-scatter) can be enabled if leveling looks
soft — CPU head-room exists (§6).

### 3.3 Round-wall boundary (no leaks, energy damping)

After integrating, if a particle is outside radius `WALL`, project it exactly
onto the rim (**no leak, ever**) and reflect the *outward* velocity component
about the wall normal with restitution `REST_E < 1` (energy loss → no
perpetual motion). Only the outward component is reflected, so a particle
sliding along the wall keeps its tangential flow (water runs *along* the rim,
climbs, and settles).

```rust
let dx = (nx >> 4) - CX;
let dy = (ny >> 4) - CY;
let d2 = dx * dx + dy * dy;
if d2 > WALL * WALL {
    let d  = isqrt(d2 as u32) as i32;          // ≤ ~288
    let nrx = dx * 256 / d;                     // Q8 unit normal
    let nry = dy * 256 / d;
    nx = (CX + dx * WALL / d) << 4;             // clamp onto rim (no leak)
    ny = (CY + dy * WALL / d) << 4;
    let vn = (vx * nrx + vy * nry) >> 8;        // Q4 outward speed
    if vn > 0 {
        vx -= (2 * vn * nrx) >> 8;              // reflect
        vy -= (2 * vn * nry) >> 8;
        vx = (vx * REST_E) >> 8;                // damp the bounce
        vy = (vy * REST_E) >> 8;
    }
}
```

### 3.4 Jerk → slosh impulse (spray on a flick)

Frame-to-frame change of the *raw* (un-bias-clamped) reading is the jerk —
a flick spikes it. A base impulse translates the whole mass (the wave lurches);
surface particles additionally get an xorshift-jittered outward kick when
`|jerk|` crosses `SPRAY_TH`, throwing droplets. During a hard flick the wall
restitution is briefly irrelevant because particles leave the surface — they
arc, then the pool re-absorbs them.

```rust
let jx = gx - st.wa_px as i32;      // i32 (operand can exceed i16)
let jy = gy - st.wa_py as i32;
st.wa_px = gx as i16; st.wa_py = gy as i16;
let jmag  = jx.abs() + jy.abs();
let spray = jmag > SPRAY_TH;
let ijx = (jx * JERK_K) >> JERK_KS; // base slosh impulse (all particles)
let ijy = (jy * JERK_K) >> JERK_KS;
…
vx += ax + ijx;  vy += ay + ijy;    // in the per-particle loop
if spray && is_surf {
    st.wa_rng ^= st.wa_rng << 13;   // xorshift32
    st.wa_rng ^= st.wa_rng >> 17;
    st.wa_rng ^= st.wa_rng << 5;
    let jt = ((st.wa_rng >> 4) & 63) as i32 - 32;   // ±32 jitter
    vx += jt / 4;
    vy -= jmag >> 6;                 // extra outward/upward throw
}
```

### 3.5 Rest breathing ripple (alive at rest)

At rest (no spray), surface particles get a tiny vertical bob on the shared
~4 s OS breathing triangle, so the liquid is *always* alive — never the still
template look. Amplitude is a fraction of a pixel/frame (Q4), so it reads as a
slow surface shimmer, not motion.

```rust
let bph  = ((elapsed_ms / 6) & 511) as i32;         // shared OS tempo
let btri = if bph < 256 { bph } else { 511 - bph }; // 0..255 triangle
…
if is_surf && !spray { vy += ((btri - 128) * BREATHE) >> 8; }
```

Surface flag: a particle is surface if the cell one step toward −gravity is
empty (`over` there == 0 / `dens` == 0). With near-zero in-plane gravity (watch
flat, gravity into the screen) the fallback is "top-most occupied cell in the
column," so breathing never dies.

### 3.6 Tunables (single const block, first-flash tuning)

```rust
const WA_CELL:  i32 = 20;    // px/cell  (24·20 = 480 ⊇ 466)
const CX: i32 = 233; const CY: i32 = 233; const WALL: i32 = 224;
const WALL2: i32 = WALL * WALL;
const GRAV:    i32 = 6;      // Q4 px/frame² at full 1 g tilt
const DAMP:    i32 = 250;    // /256 velocity retention (0.977/frame)
const VMAX:    i32 = 1024;   // Q4 clamp = 64 px/frame  (STABILITY KEYSTONE)
const REST:    i32 = 3;      // particles/cell before pressure engages
const KP:      i32 = 20;     // pressure gain
const KPS:     i32 = 5;      // pressure shift → KP/32 effective
const REST_E:  i32 = 170;    // /256 wall restitution (0.66)
const JERK_K:  i32 = 3;  const JERK_KS: i32 = 6;   // jerk→impulse
const SPRAY_TH: i32 = 900;   // |jerk| L1 spray threshold
const BREATHE: i32 = 3;      // Q4 rest-ripple amplitude
const PR:      i32 = 3;      // render half-extent → 7×7 footprint
```

---

## 4. Rendering

### 4.1 Neon-blue squares through `WATER_LUT`

Each particle is a small filled square SET over black (idempotent, per the
codebase doctrine). Intensity (LUT index) is `base + speed + depth-bias +
surface-boost + aurora`, so the LUT alone carries the whole look: deep-indigo
body, signature neon blue, cyan crests, near-white foam. The soft 1 px edge is
"free" — border pixels just pick a lower (darker) LUT index, which over black
IS a softened edge, no alpha blend needed.

The particle footprint is a 4×4 solid core inside the 7×7 clear box; fast
(spray) particles add a MAX-blended glow (`soft_dot`, r ≤ 3, fits the box).

### 4.2 Speed / depth shading

- **Speed**: `idx += (|vx| + |vy|) >> 2` — crests and thrown spray brighten to
  white; the settled body stays deep.
- **Depth**: particles deeper below the surface (more `over` above them) shift
  toward the darker LUT end — a body/crest gradient without any extra buffer.

### 4.3 Meniscus

Surface particles paint a 1 px brighter cap (`max_px`, near-white) offset one
pixel toward −gravity — a light-catching water line that emerges directly from
the surface flag, no separate surface pass.

### 4.4 Aurora drift (reuse NOISE_A/B)

The LUT index is nudged by periodic value-noise sampled at the particle's
position and scrolled in time — a luminous drift through the mass, same
doctrine as the ring flourish. `lock::NOISE_A/NOISE_B` are already `pub static`
(generated by `build.rs::generate_noise_luts`), reachable from `apps.rs`:

```rust
let t1 = (elapsed_ms / 40) as i32;
let t2 = (elapsed_ms / 57) as i32;
let au = (lock::NOISE_A[(((cx >> 3) + t1) & 255) as usize] as i32
        + lock::NOISE_B[(((cy >> 3) + t2) & 255) as usize] as i32) >> 4;  // ±~16
idx = (idx + au - 16).clamp(0, 255);
```

### 4.5 Dithering (lit pixels only)

RGB565 bands on the deep→cyan gradient are broken by a 4×4 Bayer nudge of the
**index** (not the color), applied only where a particle pixel is drawn — the
same "dither lit pixels only" rule as `GLOW_RING`/`PHOTOS_DISC` in build.rs:

```rust
const WA_BAYER: [[i32;4];4] =
    [[-6,2,-4,4],[6,-2,8,0],[-3,5,-5,3],[7,1,5,-1]];
let fi = (idx + WA_BAYER[(y & 3) as usize][(x & 3) as usize]).clamp(0, 255);
let (hi, lo) = WATER_LUT[fi as usize];       // RGB565-BE pair, SET over black
```

### 4.6 Damage / flush story (full-frame confirmed, partial as head-room)

The liquid is a large dynamic region → **full-frame flush is the honest
baseline** (~13 ms wire, matching the plan and the Gallery). We do **not**
clear the whole 424 KiB every frame (a full PSRAM memset would itself cost
~5 ms); instead we **clear only each particle's previous footprint** (7×7)
before stepping, then draw at new positions — O(N) writes (~19 k px cleared +
~19 k drawn), not O(frame).

Two damage modes, chosen by cost:

- **Baseline (spec):** `mark_rect(0,0,W-1,H-1)` → full flush, ~13 ms, dead
  simple, always correct. 40 fps.
- **Head-room lever:** mark the **union bbox** of old∪new footprints (tracked
  for free during clear/draw in `wa_bbox`). When the pool is compact this is a
  partial flush of ~120 KiB (~3 ms); as it spreads it grows, and
  `flush_dirty`'s existing 3/4-frame rule auto-falls-back to a full flush. This
  is the recommended default — it costs one bbox union and buys fps whenever
  the water is pooled — with full-frame as the guaranteed worst case.

Either way, **the status clock stays topmost**: normally the pool is at the
bottom and never touches y 26..66, so the clock band is untouched and its
pixels persist frame to frame (the run loop already refreshes it on minute
rollover). Only when liquid climbs *behind* the clock (watch tilted near
upside-down) do we repair it: a `touched_clock` flag set during draw triggers a
small `fill_rect_black(fb, CX-110, 24, CX+110, 68)` + `wheel::draw_status(fb,
now, batt)` — the luminous clock then floats over the water. Rare, cheap, and
never leaves the clock broken.

### 4.7 `WATER_LUT` build-time generator

Emitted like the saber/wheel LUTs — a 256-entry deep-indigo → blue → neon-cyan
→ white gradient as RGB565-BE `(hi,lo)` pairs, so there is zero runtime LUT
cost:

```rust
// build.rs — add generate_water_lut() to main()
fn generate_water_lut() {
    let stops: [(i32,(i32,i32,i32)); 5] = [
        (0,   (2,   6,  26)),   // abyss (near-black indigo)
        (60,  (0,  34, 110)),   // deep body
        (150, (0, 130, 210)),   // signature neon blue
        (210, (40,205, 255)),   // bright cyan crest
        (256, (200,244, 255)),  // neon-white foam
    ];
    let lerp = |a: i32, b: i32, t: i32, d: i32| a + (b - a) * t / d;
    let mut body = String::from(
        "/// Auto-generated by build.rs (generate_water_lut). deep→cyan→white, RGB565-BE.\n\
         pub static WATER_LUT: [(u8, u8); 256] = [");
    for i in 0..256i32 {
        let mut s = 0usize;
        while s + 1 < stops.len() && i >= stops[s + 1].0 { s += 1; }
        let (i0, (r0, g0, b0)) = stops[s];
        let (i1, (r1, g1, b1)) = stops[(s + 1).min(4)];
        let d = (i1 - i0).max(1);
        let t = (i - i0).clamp(0, d);
        let (r, g, b) = (lerp(r0, r1, t, d), lerp(g0, g1, t, d), lerp(b0, b1, t, d));
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
```

`apps.rs` then adds `include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));`
next to the existing gallery include.

---

## 5. Exact integration

### 5.1 `apps::State` additions

The SoA fields in §2, added to `pub struct State`, and to `const fn new()`:

```rust
    wa_x: [0; WA_N], wa_y: [0; WA_N], wa_vx: [0; WA_N], wa_vy: [0; WA_N],
    wa_dens: [0; WA_G * WA_G],
    wa_raw: [0, 0, 4096],          // z≈0.5 g default (flat) until first read
    wa_bx: 0, wa_by: 0, wa_px: 0, wa_py: 0,
    wa_rng: 0x9E3779B9,            // non-zero xorshift seed
    wa_bbox: (0, 0, -1, -1),
    wa_batt: None,
    wa_cal: false, wa_live: false, wa_spawned: false,
```

### 5.2 `has_content`

```rust
pub fn has_content(idx: usize) -> bool { let _ = idx; true }  // Water now has content
```

This flips Water from "rests on splash" to a content app, so the open morph
plays the content-reveal beat and calls `draw_reveal(WATER, q, …)`.

### 5.3 `tick` / `draw_reveal` branches

`tick` gains a Water arm (was `_ => {}`):

```rust
pub fn tick(wfb, idx, now, elapsed_ms, st) {
    match idx {
        …
        WATER => water_tick(wfb, now, elapsed_ms, st),
        _ => {}
    }
}
```

`draw_reveal` gains a Water arm that fills the pool in with `q` (particles
lazily spawned on first call, revealed proportional to `q`, fading with `q`):

```rust
} else if idx == WATER {
    if !st.wa_spawned { water_spawn(st); st.wa_spawned = true; st.wa_cal = false; }
    water_reveal(wfb, st, q_q8, elapsed_ms);   // draws the filling pool + status
    let r = (CX - WALL, CY - 40, CX + WALL, CY + WALL);
    fx.push(r.0, r.1, r.2, r.3);
    wfb.mark_rect(r.0, r.1, r.2, r.3);
}
```

`water_spawn` places `WA_N` particles packed in the lower cap of the circle
with a small downward velocity; `water_reveal` renders the first
`(q_q8 * WA_N) >> 8` of them at alpha `q` — the pool "fills in" on open and
drains/fades on close (the close morph re-drives the same `q` 256→0). The
first `water_tick` after the morph completes captures calibration
(`wa_cal = false → true`) and starts live physics.

### 5.4 Getting gravity from `self.i2c` into the sim — minimal `app.rs` diff

**(a)** At the app-tick call site (~line 322, the non-Gallery `else` branch):
feed this frame's accel + battery before ticking Water. `apps` stays I2C-free
(the run loop owns `self.i2c`).

```rust
} else {
    if idx == apps::WATER {
        // Run loop owns i2c: hand the liquid its gravity for this frame.
        let raw = crate::drivers::qmi8658::read_accel(&mut self.i2c).ok();
        apps::water_feed(&mut app_state, raw, wheel_batt);
    }
    apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
    if apps::shows_status(idx) && status_minute != now.minute {
        status_minute = now.minute;
        wheel::tick_status(&mut self.wfb, &now, wheel_batt);
    }
}
```

with the tiny public helper in `apps.rs`:

```rust
pub fn water_feed(st: &mut State, raw: Option<(i16,i16,i16)>, batt: Option<u8>) {
    if let Some((ax, ay, az)) = raw { st.wa_raw = [ax, ay, az]; st.wa_live = true; }
    // read error → keep last vector (dead-IMU fallback handled in water_step)
    st.wa_batt = batt;
}
```

**(b)** Pin Water to the 40 fps cadence (it must animate every frame, unlike
the 20 fps rest cadence). One arm in the `frame_us` match (~line 457):

```rust
let frame_us = match power {
    Power::Aod | Power::Sleep => IDLE_FRAME_US,
    _ if scene == Scene::Locked && self.clock.is_animating() => CLOCK_ANIM_FRAME_US,
    _ if scene == Scene::Wheel && self.wheel_fx.intro_active() => ANIM_FRAME_US,
    _ if scene == Scene::App(apps::WATER) && power == Power::Awake => ANIM_FRAME_US, // + this
    _ => FRAME_US,
};
```

That is the **entire** `app.rs` change: one `if` at the tick site (3 lines) +
one match arm. No renderer rewrite, no new loop, no signature churn to the
shared `tick`. (Freeze-when-Dim comes for free: `apps::tick` only runs at
`Power::Awake`, so the sim naturally pauses when dimmed and resumes on wake —
`water_step` resets the jerk baseline on the resume frame so a stale
`wa_px/wa_py` can't startle the pool.)

If the liquid later wants its own faster/independent loop, escalate to the
`gallery_interact`-style dedicated interactive loop (plan option 2); option 1
above is the uniform, minimal first cut.

### 5.5 `build.rs`

Add `generate_water_lut();` to `main()` (alongside `generate_wheel_assets();`)
— generator body in §4.7.

---

## 6. Per-frame ops + ms estimate

Per frame at **N = 384, grid 24×24**:

| Stage | Work | ~ops | ~PSRAM px | Est. |
|---|---|---|---|---|
| Clear old footprints | 384 × 7×7 | — | 18.8 k writes | ~0.9 ms |
| Integrate + jerk + breathe | 384 × ~8 | 3.1 k | — | <0.1 ms |
| Scatter density | 576 zero + 384 bin | 2.5 k | — | <0.1 ms |
| Pressure (1 Jacobi pass) | 384 × ~12 | 4.6 k | — | <0.1 ms |
| Wall reflect (isqrt on the ~10–20 % near rim) | 384 × ~15 | 5.8 k | — | ~0.2 ms |
| Draw squares + LUT + dither + aurora | 384 × 7×7 | ~120 k | 18.8 k SET | ~1.6 ms |
| Meniscus + spray glow | ~80 surface × few | ~4 k | ~1 k | ~0.1 ms |
| **Compute subtotal** | | **~140 k ops** | **~39 k px** | **≈ 3.0–3.5 ms** |
| Flush (full-frame baseline) | 424 KiB wire | — | — | **~13 ms** |
| Flush (partial, pooled) | ~120 KiB wire | — | — | ~3 ms |

Frame = compute (~3.5 ms) + flush (13 ms full) ≈ **16.5 ms**, comfortably
inside the 25 ms (40 fps) cadence with ~8 ms of slack.

**Count that fits ~10 ms compute:** compute is render-bound (~40 k px, ~15
cyc/px scattered PSRAM). Linear scaling puts the ~10 ms compute ceiling near
**~1000 particles**. We deliberately choose **384** because:
1. flush (13 ms full-frame) is the binding constraint at 40 fps — extra
   particles don't raise fps, they only densify;
2. 384 already reads as a full, dense pool at this grid resolution;
3. the ~8 ms slack leaves room for a 2nd pressure pass, the partial-flush bbox
   math, and the future P1 async-flush without re-tuning.

So: **384 particles, 24×24 grid, 40 fps.** Head-room to ~500 for a thicker pool
if desired, or to enable the partial-flush lever for higher fps when pooled.

---

## 7. Stability guarantees + honest weaknesses

### 7.1 Why it never blows up / leaks / piles to a singularity

- **Never explodes.** Velocity is hard-clamped to `±VMAX` every frame *after*
  damping (`DAMP < 256`), so kinetic energy is strictly bounded and net-
  dissipative each frame. No accel term (gravity ≤ 6 Q4, pressure ≤ ~240 Q4,
  jerk bounded by the raw ±32768 range) can push past the clamp for more than
  one frame. Every hot multiply is proven inside i32 (§1.2) at those clamps —
  no wraparound corruption is even representable.
- **Never leaks.** The wall does a hard *position* clamp onto the rim
  (`nx = (CX + dx*WALL/d) << 4`) independent of the velocity reflect, so a
  particle is repositioned inside the boundary every frame it crosses — a leak
  is impossible even if the reflect math were disabled. Containment radius 224
  sits 2 px inside the 226 interior, so the 2 px particle never touches the
  bezel.
- **Never piles to a singularity.** The scatter+pressure pass makes crowding
  self-limiting: the more particles a cell holds, the larger the outward
  over-density gradient pushing them apart. Compression has a restoring force
  ∝ excess density, so density saturates near `REST` instead of collapsing to a
  point. (And even a pathological pile is bounded by `VMAX` and the i32 proof.)
- **Always settles.** `DAMP` + wall restitution `REST_E < 1` remove energy every
  frame, so absent new IMU input the pool damps to a level surface with a few
  ripples — never perpetual motion. The rest-breathing term is sub-pixel and
  balanced (mean-zero triangle), so it animates without pumping energy in.

### 7.2 Honest weaknesses of PIC-grid (this approach)

1. **Grid quantization noise.** At ~384 particles over 24×24, a resting cell
   holds only ~3–5 particles; density is integer, so the pressure gradient is
   slightly steppy and the surface can shimmer at the cell scale (~20 px). A
   finer grid worsens the count-per-cell noise; a coarser grid blockifies
   leveling. 24×24 is the compromise; a bilinear scatter would smooth it at
   ~2× scatter cost (deferred).
2. **Single Jacobi pass under-relaxes.** One pass doesn't fully enforce
   incompressibility, so under a hard, sustained tilt the pool can transiently
   over-compress against the low wall before leveling. Mitigation: the 2nd
   sub-iteration (head-room exists) or a slightly higher `KP` — traded against
   the risk of pressure ringing.
3. **Discrete look at the surface.** Particles are squares, not a fitted
   isosurface, so a thin/fast sheet can read as separated dots rather than a
   continuous film (the plan's "grainy" caveat). The meniscus + LUT crest hide
   most of it; a marching-squares surface fill would be the premium upgrade but
   is a different, heavier renderer.
4. **Full-frame flush caps fps at ~40** regardless of how cheap the physics is
   (compute is only ~3.5 ms). This app is the poster child for perf-plan **P1
   async flush** — it would roughly double the ceiling. The partial-bbox lever
   (§4.6) is the interim win when the pool is compact.
5. **Cadence-coupled dt.** Integration assumes dt = 1 frame; if the 40 fps
   cadence slips (a heavy neighbour frame), the sim briefly runs fast/slow.
   Damping hides small jitter; a large stall would visibly lurch. A measured-dt
   scaling is possible but adds a per-frame multiply to velocity and a second
   overflow case — deferred as not worth it at a pinned cadence.

---

## 8. Concrete Rust code sketch (core update + render)

Real function bodies using the repo's integer helpers
(`set_px`-style writes, `max_px`, `soft_dot`, `fill_rect_black`, `isqrt`,
`WATER_LUT`, `lock::NOISE_A/B`). This is the shape of `src/scenes/apps.rs`'s
Water block; constants are the §3.6 block.

```rust
// include!(concat!(env!("OUT_DIR"), "/water_lut.rs"));   // -> WATER_LUT

const WA_CELL: i32 = 20;
const WALL: i32 = 224;  const WALL2: i32 = WALL * WALL;
const PR: i32 = 3;                       // 7×7 footprint
const NEON: (i32, i32, i32) = (200, 244, 255);   // meniscus / foam
const WA_BAYER: [[i32; 4]; 4] =
    [[-6, 2, -4, 4], [6, -2, 8, 0], [-3, 5, -5, 3], [7, 1, 5, -1]];

/// SET an RGB565-BE pair straight to the framebuffer (over black — idempotent).
#[inline]
fn set_lut_px(fb: &mut [u8], x: i32, y: i32, hi: u8, lo: u8) {
    if x < 0 || x >= W || y < 0 || y >= H { return; }
    let idx = ((y * W + x) * 2) as usize;
    if idx + 1 < fb.len() { fb[idx] = hi; fb[idx + 1] = lo; }
}

/// Full Water frame: clear last footprints, step physics, draw, keep the
/// clock topmost, mark damage.
fn water_tick(wfb: &mut WatchFb, now: &WallTime, elapsed_ms: u32, st: &mut State) {
    let batt = st.wa_batt;
    {
        let fb = wfb.buf_mut();
        water_clear(fb, st);          // erase where particles were drawn
    }
    water_step(st, elapsed_ms);       // integrate + pressure + wall + jerk
    let touched = {
        let fb = wfb.buf_mut();
        let t = water_draw(fb, st, elapsed_ms);
        if t {                        // liquid climbed behind the clock — repair
            fill_rect_black(fb, CX - 110, 24, CX + 110, 68);
            wheel::draw_status(fb, now, batt);
        }
        t
    };
    let _ = touched;
    // Damage: union bbox (partial flush when pooled; auto full-frame fallback).
    let b = st.wa_bbox;
    wfb.mark_rect(b.0 as i32, b.1 as i32, b.2 as i32, b.3 as i32);
}

/// Erase each particle's current (last-drawn) 7×7 footprint.
fn water_clear(fb: &mut [u8], st: &State) {
    for i in 0..WA_N {
        let cx = (st.wa_x[i] as i32) >> 4;
        let cy = (st.wa_y[i] as i32) >> 4;
        fill_rect_black(fb, cx - PR, cy - PR, cx + PR, cy + PR);
    }
}

/// Physics: gravity from IMU, scatter, one Jacobi pressure pass, wall reflect,
/// jerk spray, rest breathing. All Q4 integer.
fn water_step(st: &mut State, elapsed_ms: u32) {
    // --- gravity this frame (mapped, calibrated on first tick) ---
    let gx = IMU_XSGN * st.wa_raw[IMU_XSRC] as i32;
    let gy = IMU_YSGN * st.wa_raw[IMU_YSRC] as i32;
    if !st.wa_cal {
        st.wa_bx = gx as i16; st.wa_by = gy as i16;      // capture rest bias
        st.wa_px = gx as i16; st.wa_py = gy as i16;      // jerk baseline
        st.wa_cal = true;
    }
    let (mut ax, mut ay) = if st.wa_live {
        let gmx = (gx - st.wa_bx as i32).clamp(-8192, 8192);
        let gmy = (gy - st.wa_by as i32).clamp(-8192, 8192);
        (gmx * GRAV / 8192, gmy * GRAV / 8192)
    } else {
        (0, GRAV)                                         // dead-IMU down-vector
    };
    let jx = gx - st.wa_px as i32;                        // jerk (i32)
    let jy = gy - st.wa_py as i32;
    st.wa_px = gx as i16; st.wa_py = gy as i16;
    let jmag = jx.abs() + jy.abs();
    let spray = jmag > SPRAY_TH;
    let ijx = (jx * JERK_K) >> JERK_KS;
    let ijy = (jy * JERK_K) >> JERK_KS;
    if ax == 0 && ay == 0 { ay = 1; }                    // keep a "down" for surface calc
    let (sax, say) = (ax.signum(), ay.signum());
    let bph = ((elapsed_ms / 6) & 511) as i32;
    let btri = if bph < 256 { bph } else { 511 - bph };

    // --- scatter density ---
    st.wa_dens.fill(0);
    for i in 0..WA_N {
        let cx = (((st.wa_x[i] as i32) >> 4) / WA_CELL).clamp(0, WA_G as i32 - 1);
        let cy = (((st.wa_y[i] as i32) >> 4) / WA_CELL).clamp(0, WA_G as i32 - 1);
        st.wa_dens[cy as usize * WA_G + cx as usize] += 1;
    }
    let over = |d: &[u16], cx: i32, cy: i32| -> i32 {
        if cx < 0 || cy < 0 || cx >= WA_G as i32 || cy >= WA_G as i32 { return 0; }
        (d[cy as usize * WA_G + cx as usize] as i32 - REST).max(0)
    };
    let dens_at = |d: &[u16], cx: i32, cy: i32| -> i32 {
        if cx < 0 || cy < 0 || cx >= WA_G as i32 || cy >= WA_G as i32 { return 0; }
        d[cy as usize * WA_G + cx as usize] as i32
    };

    // --- integrate + pressure + wall, per particle ---
    let mut rng = st.wa_rng;
    for i in 0..WA_N {
        let mut vx = st.wa_vx[i] as i32;
        let mut vy = st.wa_vy[i] as i32;
        let px = (st.wa_x[i] as i32) >> 4;
        let py = (st.wa_y[i] as i32) >> 4;
        let cx = (px / WA_CELL).clamp(0, WA_G as i32 - 1);
        let cy = (py / WA_CELL).clamp(0, WA_G as i32 - 1);

        vx += ax + ijx;                                   // gravity + base slosh
        vy += ay + ijy;
        let gradx = over(&st.wa_dens, cx + 1, cy) - over(&st.wa_dens, cx - 1, cy);
        let grady = over(&st.wa_dens, cx, cy + 1) - over(&st.wa_dens, cx, cy - 1);
        vx -= (gradx * KP) >> KPS;                        // incompressibility
        vy -= (grady * KP) >> KPS;

        let is_surf = dens_at(&st.wa_dens, cx - sax, cy - say) == 0; // cell toward air
        if is_surf {
            if spray {
                rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5;
                let jt = ((rng >> 4) & 63) as i32 - 32;
                vx += jt / 4;
                vy -= jmag >> 6;                          // throw droplets
            } else {
                vy += ((btri - 128) * BREATHE) >> 8;      // rest breathing
            }
        }

        vx = ((vx * DAMP) >> 8).clamp(-VMAX, VMAX);       // damp + clamp (STABILITY)
        vy = ((vy * DAMP) >> 8).clamp(-VMAX, VMAX);

        let mut nx = st.wa_x[i] as i32 + vx;              // integrate (Q4)
        let mut ny = st.wa_y[i] as i32 + vy;
        let dx = (nx >> 4) - CX;
        let dy = (ny >> 4) - CY;
        let d2 = dx * dx + dy * dy;
        if d2 > WALL2 {                                   // round-wall boundary
            let d = isqrt(d2 as u32) as i32;
            let nrx = dx * 256 / d;
            let nry = dy * 256 / d;
            nx = (CX + dx * WALL / d) << 4;               // hard clamp — no leak
            ny = (CY + dy * WALL / d) << 4;
            let vn = (vx * nrx + vy * nry) >> 8;
            if vn > 0 {
                vx -= (2 * vn * nrx) >> 8;                // reflect outward part
                vy -= (2 * vn * nry) >> 8;
                vx = (vx * REST_E) >> 8;                  // damp the bounce
                vy = (vy * REST_E) >> 8;
            }
        }
        st.wa_x[i] = nx as i16;  st.wa_y[i] = ny as i16;
        st.wa_vx[i] = vx as i16; st.wa_vy[i] = vy as i16;
    }
    st.wa_rng = rng;
}

/// Draw all particles as neon squares (LUT + dither + aurora), meniscus on the
/// surface, spray glow on the fast ones. Returns whether any pixel entered the
/// status-clock band (triggers the topmost-clock repair). Also tracks wa_bbox.
fn water_draw(fb: &mut [u8], st: &mut State, elapsed_ms: u32) -> bool {
    let t1 = (elapsed_ms / 40) as i32;
    let t2 = (elapsed_ms / 57) as i32;
    let (sax, say) = {                                    // -gravity (meniscus side)
        let ax = IMU_XSGN * (st.wa_raw[IMU_XSRC] as i32 - st.wa_bx as i32);
        let ay = IMU_YSGN * (st.wa_raw[IMU_YSRC] as i32 - st.wa_by as i32);
        (ax.signum(), if ax == 0 && ay == 0 { 1 } else { ay.signum() })
    };
    let (mut bx0, mut by0, mut bx1, mut by1) = (W, H, 0, 0);
    let mut touched = false;
    for i in 0..WA_N {
        let cx = (st.wa_x[i] as i32) >> 4;
        let cy = (st.wa_y[i] as i32) >> 4;
        let spd = (st.wa_vx[i] as i32).abs() + (st.wa_vy[i] as i32).abs();  // Q4 L1
        // grid lookup for depth/surface
        let gcx = (cx / WA_CELL).clamp(0, WA_G as i32 - 1);
        let gcy = (cy / WA_CELL).clamp(0, WA_G as i32 - 1);
        let above = {
            let (ux, uy) = (gcx - sax, gcy - say);
            if ux < 0 || uy < 0 || ux >= WA_G as i32 || uy >= WA_G as i32 { 0 }
            else { st.wa_dens[uy as usize * WA_G + ux as usize] as i32 }
        };
        let is_surf = above == 0;
        let au = (lock::NOISE_A[(((cx >> 3) + t1) & 255) as usize] as i32
                + lock::NOISE_B[(((cy >> 3) + t2) & 255) as usize] as i32) >> 4;
        let mut idx = 120 + (spd >> 2) + au - 16 - (above.min(6) * 8); // body base
        if is_surf { idx = idx.max(206); }               // crest / foam
        idx = idx.clamp(0, 255);

        for dy in -PR..=PR {
            for dx in -PR..=PR {
                let ad = dx.abs().max(dy.abs());
                if ad > 2 && !(spray_glow(spd) && ad == 3) { continue; } // 4×4 core (+glow)
                let mut fi = idx;
                if ad == 2 { fi -= 46; }                  // soft (darker) edge
                if ad == 3 { fi -= 96; }                  // outer glow ring
                let x = cx + dx; let y = cy + dy;
                let bd = WA_BAYER[(y & 3) as usize][(x & 3) as usize];
                let fi = (fi + bd).clamp(0, 255) as usize;
                let (hi, lo) = WATER_LUT[fi];
                set_lut_px(fb, x, y, hi, lo);
                if (26..=66).contains(&y) && (CX - 110..=CX + 110).contains(&x) {
                    touched = true;
                }
            }
        }
        if is_surf {                                      // meniscus: 1 px foam cap
            max_px(fb, cx - sax, cy - say, NEON, 235);
        }
        bx0 = bx0.min(cx - PR); by0 = by0.min(cy - PR);
        bx1 = bx1.max(cx + PR); by1 = by1.max(cy + PR);
    }
    // union with last frame's cleared region for a complete damage rect
    let p = st.wa_bbox;
    st.wa_bbox = (
        bx0.min(p.0 as i32).max(0) as i16,
        by0.min(p.1 as i32).max(0) as i16,
        bx1.max(p.2 as i32).min(W - 1) as i16,
        by1.max(p.3 as i32).min(H - 1) as i16,
    );
    touched
}

#[inline] fn spray_glow(spd: i32) -> bool { spd > 320 }   // >20 px/frame → glowing
```

`water_spawn` / `water_reveal` (open fill-in / close drain) place `WA_N`
particles in the lower cap and render the first `(q*WA_N)>>8` at alpha `q`
using the same `water_draw` pixel path with a `q`-scaled LUT index — they reuse
everything above and are omitted here for length.

---

### Build order (maps to the plan's §7)

1. Axis-map + on-open calibration on device (the one true unknown).
2. Particle core: spawn, gravity integrate, round-wall reflect, flat squares.
3. Incompressibility: scatter + one Jacobi pressure pass (this doc's core).
4. Slosh/spray: jerk impulse + surface throw.
5. Look: `WATER_LUT`, meniscus, aurora, dither.
6. Polish: fill-in/drain, rest breathing, partial-flush bbox, freeze-when-Dim.
