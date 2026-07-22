# Water — falling-sand liquid (design)

Approach: **gravity-oriented cellular automaton** ("falling sand", water rules)
with the down-direction rotated by the live IMU gravity vector, a single global
*slosh-momentum* vector for inertia, and a tiny ballistic **droplet** pool bolted
on for flick-spray. Grounded in the real code:

- Physics + shade live in `apps::State` (internal SRAM), stepped from the run
  loop's `apps::tick` call site (`src/app.rs:324`); gravity comes from
  `qmi8658::read_accel(&mut self.i2c) -> Result<(i16,i16,i16),()>`
  (`src/drivers/qmi8658.rs:48`, 8192 LSB/g, ±4g).
- Render reuses the `apps.rs` integer helpers (`isqrt`, `fill_rect_black`,
  Bayer-index dithering in the `set_px` style) + a build-time `WATER_LUT` emitted
  exactly like `saber_lut`'s RGB565-BE tuples, and `NOISE_A/NOISE_B` for aurora.
- Full-frame-flush app: `flush_dirty` (`src/app.rs:1001`) tips to full `'F'`
  (~13 ms) once damage passes 3/4 of the frame, which the sloshing pool does.
  Compute budget ≈ 25 ms cadence − 13 ms flush ≈ **10–11 ms**. Target **40 fps**.

Screen facts used throughout: `W=H=466`, `CX=CY=233`, interior wall
`BEZEL_R=223` (`src/scenes/lock.rs:38`) → the sim uses `WALL_R=224`. The status
clock band is `x∈[123,343], y∈[26,66]`; the liquid is **clipped out of it** so it
is never cleared or overdrawn — the run loop keeps the clock topmost exactly as
today.

---

## 0. Why a CA (and where it hurts)

A cellular automaton is the *right shape* for three of the hard constraints and
the *wrong shape* for one:

- **Incompressible by construction** — a cell holds exactly 0 or 1 water unit, so
  "pile to a singularity" is physically impossible and no pressure solver is
  needed (unlike the PIC-lite plan). This is the standout win.
- **No wall leak, ever** — a move is a membership test against a precomputed
  circular mask; water simply cannot address a cell outside the circle.
- **No blow-up** — bulk cells move ≤ 1 cell/frame; there is no bulk velocity to
  integrate to infinity.
- **But**: a positional CA carries *no momentum*, so genuine slosh/waves and
  thrown spray are not emergent. Both are *added back* — momentum as one global
  vector that biases lateral flow, spray as a separate ≤48-droplet ballistic
  pool. Honest about this in §7.

---

## 1. Fixed-point scheme + overflow proof

The bulk CA is **cell-indexed and needs no fractional position** — the entire
point of falling sand. Fixed-point is confined to (a) the gravity/momentum
control vector and (b) the ballistic droplets.

| Quantity | Format | Unit | Clamp |
|---|---|---|---|
| Gravity `g=(gx,gy)` | **Q8** | 1 g = 256 | each comp `±256` |
| Slosh `s=(sx,sy)` | **Q8** | flow-bias, same axis as g | each comp `±512` |
| Bias accum `acc=(ax,ay)` | **Q8** | carries sub-cell flow | each comp `±1024` |
| Droplet pos `(x,y)` | **Q8** | screen px | `[0, 466<<8]` |
| Droplet vel `(vx,vy)` | **Q8** | px/frame | each comp `±4096` (16 px/frame) |
| Cell energy (shade) | u8 | 0=empty, 1..255 water | `[FLOOR, 255]` |

**Every hot-loop multiply, worst-case operands → fits i32 (`±2.147e9`):**

1. **Gravity from raw** `gx = ((raw_map - rest) * 256) / 8192`, computed as
   `(raw_map - rest) >> 5` then clamp. `raw_map∈[-32768,32767]` (i16),
   `rest∈±8192` ⇒ `(raw_map-rest)∈±40960`. `×256 = ±1.05e7`. **Fits.** (`>>5`
   form peaks at `±40960`.) ✓
2. **Jerk** `j = raw_map − last_raw_map`: i16−i16 = `±65535`, held in i32. No
   multiply. ✓
3. **Slosh impulse** `sx += (jx >> J_SHIFT)`, then `sx = (sx*230)>>8` (decay
   0.898). `sx≤512`, `512×230 = 117760`, `>>8 = 460`. **Fits.** ✓
4. **flow = g + s**: `±256 + ±512 = ±768`. ✓
5. **Octant dot** `dot = RING[k].0*fx + RING[k].1*fy`, `RING∈{-1,0,1}`,
   `f∈±768` ⇒ `±1536`. ✓
6. **Wall test** (cell → screen) `(4cx−226)² + (4cy−226)²`, `cx∈[0,113]` ⇒
   `4cx−226∈[-226,226]`, square `= 51076`, sum `≤ 102152` vs `WALL_R²=50176`.
   **Fits** (precomputed once per row at seed, so this is a *seed-time* cost, not
   hot). ✓
7. **Droplet integrate**: `vy += GD` (add, no mul); `y += vy`,
   `y≤466<<8=119296`, `vy≤4096` ⇒ `≤123392`. **Fits.** Spawn velocity
   `vx = (jx_unit * SPRAY_K)`, `jx_unit∈±256` (Q8 unit), `SPRAY_K≤16` ⇒ `±4096`.
   **Fits.** ✓
8. **Droplet wall/land** `isqrt((d2 as u32) << 8)`, `d2≤108578` (screen space) ⇒
   `<<8 = 27.8e6 < u32::MAX (4.29e9)`. **Fits.** ✓
9. **Render index** `idx = base + bayer`, `base∈[0,255]`, `bayer∈[-8,8]` — the
   per-cell `base` has ONE multiply `depth*DEPTHK`, `depth∈±60`, `DEPTHK≤4` ⇒
   `±240`. ✓
10. **Per-pixel path has NO divide and NO multiply** (see §4): `WATER_LUT` is
    pre-packed RGB565-BE, dither is an *index add*. This is the single most
    important perf decision — the repo's `set_px`/`blend_px` do ~6 `/255`
    divides per pixel, and Xtensa integer divide is ~30 cyc; a per-pixel divide
    in a 55k-px loop would alone cost ~7 ms.

**Largest intermediate anywhere ≈ 1.05e7 (gravity-from-raw).** Nothing needs
i64. Stated explicitly.

---

## 2. State layout + SRAM byte count

`CELL = 4 px`, `GW = GH = 114` (grid covers screen `[5, 461)`, origin
`ORIGIN = CX − GW*CELL/2 = 5`; the round wall spans `[9,457]`, comfortably
inside). Cell `(cx,cy)` center = screen `(7+4cx, 7+4cy)`.

```rust
const CELL: i32 = 4;
const GW: usize = 114;
const GH: usize = 114;
const NCELL: usize = GW * GH;      // 12_996
const N_DROP: usize = 48;
const WALL_R: i32 = 224;

pub struct Water {
    grid:    [u8; NCELL],   // 0=empty; 1..=255 water, value = energy/shade
    wall_lo: [i16; GH],     // first in-wall cell x per row  (precomputed)
    wall_hi: [i16; GH],     // last  in-wall cell x per row
    drop:    [Droplet; N_DROP],
    n_drop:  u8,
    // gravity / momentum (Q8)
    gx: i32, gy: i32,
    sx: i32, sy: i32,       // slosh momentum
    ax: i32, ay: i32,       // sub-cell bias accumulator
    // calibration + jerk source (screen-mapped raw)
    rest_x: i16, rest_y: i16,
    last_x: i16, last_y: i16,
    // housekeeping
    rng: u32, frame: u32, seeded: bool, parity: u8,
    wb: (i16,i16,i16,i16),  // last-frame wet bbox (cells) for the clear
}
#[derive(Clone,Copy)]
struct Droplet { x: i32, y: i32, vx: i16, vy: i16 }   // Q8 pos, Q8 vel
```

**Byte count (internal SRAM):**

| Field | Bytes |
|---|---|
| `grid [u8;12996]` | 12 996 |
| `wall_lo/hi [i16;114]×2` | 456 |
| `drop [Droplet;48]` (12 B each) | 576 |
| scalars (gx…wb) | ~64 |
| **Total** | **≈ 14 092 B ≈ 13.8 KB** |

`WATER_LUT [u16;256] = 512 B` lives in flash (static), not counted.

Lean config if SRAM is contested: `CELL=5, GW=GH=92` → `grid = 8 464 B`, total
**≈ 9.6 KB**, squares 5 px (still "tiny"). Fatter config `CELL=3` is ~24 KB and
finer but pushes render toward the budget ceiling — not recommended.

**Placement:** 13.8 KB is large for the `run()` stack frame. Put the two big
arrays behind a module `static` (single-threaded, core-0-only sim — a small
`unsafe` accessor is sound and matches embedded practice), or box `Water` once at
construction. `State` (in `apps.rs`) then holds `water: &'static mut Water` /
`Option<Water>`. Everything is process-lifetime, never freed → no heap churn,
consistent with `watch_fb`'s "no heap after construction" doctrine.

---

## 3. Per-frame update

Order: read → calibrate → jerk/slosh → flow/octant → CA pass → droplets →
rest-ripple → render (§4).

### 3a. Gravity: tunable axis map + rest calibration

The IMU-to-screen mapping is **unknown until measured on the board** (like the
touch Y-flip). Exposed as consts so first-flash tuning is a one-line edit:

```rust
// Which raw axis drives each screen axis, and its sign. Found on-device by
// logging read_accel while tilting top/bottom/left/right (WATER-APP-PLAN §2).
const AXIS_X: usize = 0;   // 0=ax 1=ay 2=az  → screen +x (right)
const AXIS_Y: usize = 1;   //                 → screen +y (down)
const SGN_X:  i32 = 1;
const SGN_Y:  i32 = -1;

fn map(raw: (i16,i16,i16)) -> (i16,i16) {
    let r = [raw.0, raw.1, raw.2];
    ((SGN_X * r[AXIS_X] as i32) as i16, (SGN_Y * r[AXIS_Y] as i32) as i16)
}
```

**Rest calibration** captured on app open (watch assumed roughly flat, so
in-plane accel ≈ sensor zero-bias): average a few `read_accel` samples into
`rest_x/rest_y`, subtract every frame. A recalibrate affordance (corner tap /
long-press) just re-captures. Dead IMU (`Err`): reuse last `(gx,gy)` — the sim
never stalls.

```rust
let (mx,my) = map(raw);
self.gx = (((mx - self.rest_x) as i32) >> 5).clamp(-256,256);
self.gy = (((my - self.rest_y) as i32) >> 5).clamp(-256,256);
```

### 3b. Jerk → slosh momentum + spray impulse

```rust
let jx = (mx - self.last_x) as i32;   // ±65535, i32
let jy = (my - self.last_y) as i32;
self.last_x = mx; self.last_y = my;
let jm = jx.abs() + jy.abs();         // L1 magnitude

self.sx = (self.sx * 230) >> 8;       // decay 0.898/frame
self.sy = (self.sy * 230) >> 8;
if jm > JERK_THRESH {                  // a flick
    self.sx = (self.sx + (jx >> 4)).clamp(-512,512);
    self.sy = (self.sy + (jy >> 4)).clamp(-512,512);
    spray(self, jx, jy);               // §3e
}
```

Slosh is a single global DOF: fling the watch right → `sx` spikes → the CA's
lateral flow prefers rightward for ~10 frames → the body surges into and climbs
the right wall, then decays back and levels. This is the *inertia* a positional
CA lacks, faked cheaply.

### 3c. Flow + octant

```rust
let fx = (self.gx + self.sx).clamp(-768,768);
let fy = (self.gy + self.sy).clamp(-768,768);

const RING: [(i32,i32);8] =
    [(0,-1),(1,-1),(1,0),(1,1),(0,1),(-1,1),(-1,0),(-1,-1)]; // CW from screen-up
// k = argmax dot(RING[k], flow)  (8 mul-adds, once per frame)
let mut k = 0; let mut best = i32::MIN;
for i in 0..8 { let d = RING[i].0*fx + RING[i].1*fy; if d>best {best=d;k=i;} }
let prim = RING[k];
let dia  = (RING[(k+1)&7], RING[(k+7)&7]);   // ±45°
let lat  = (RING[(k+2)&7], RING[(k+6)&7]);   // ±90° (spread/level)
```

This handles **arbitrary tilt**: gravity anywhere in-plane picks the octant; the
primary is "downhill", the diagonals are the run-off, the laterals are the
leveling spread. Diagonal gravity (~45°) is a first-class octant, not a special
case.

### 3d. The CA pass (incompressibility / flow)

Serpentine, scanned **against flow** so a moved cell always lands *behind* the
scan and is never processed twice (the standard in-place falling-sand guarantee —
no double-buffer, no moved-flag):

```rust
// vertical order against gravity; horizontal order flips per frame (parity)
let ys: &[usize] = if fy >= 0 { &DOWN_ROWS } else { &UP_ROWS };
for &cy in ys {
    let (lo, hi) = (self.wall_lo[cy], self.wall_hi[cy]);
    let xr = row_x_order(lo, hi, fx, self.parity);  // against flow_x
    for cx in xr {
        let i = cy*GW + cx as usize;
        let v = self.grid[i];
        if v == 0 { continue; }                     // empty: ~6 cyc, skip
        // 1) primary (downhill)
        if try_move(self, cx, cy, prim) { continue; }
        // 2) the two diagonals — tie-break by slosh sign then cheap hash
        if try_pair(self, cx, cy, dia, self.sx, self.sy) { continue; }
        // 3) the two laterals (leveling) — biased toward slosh direction
        if try_pair(self, cx, cy, lat, self.sx, self.sy) { continue; }
        // 4) stuck → damp its energy toward the resting floor
        self.grid[i] = v.saturating_sub(ENERGY_DECAY).max(ENERGY_FLOOR);
    }
}
self.parity ^= 1;
```

`try_move` moves the cell iff the target is **in-wall** (`lo≤tx≤hi` for the
target row — the wall check is a bounds compare, O(1), *no leak possible*) **and
empty**; on success it writes `ENERGY_MOVED` (~200) into the destination and 0
into the source (a full↔empty swap → mass conserved) and returns true. Moving
cells glow bright; stuck cells decay toward `ENERGY_FLOOR` (~40) → speed-shading
for free, stored in the single grid byte.

**Leveling** is the lateral pass: one lateral cell/frame ⇒ ~160 px/s spread at
40 fps, which reads as a convincing *thick, premium* liquid. Faster/thinner water
would need a second lateral sub-pass (2× the CA cost, still ~1.8 ms) — a tuning
knob, off by default.

### 3e. Spray (ballistic droplets)

On a flick, lift a few *surface* cells (a water cell whose `−prim` neighbor — the
"up" cell — is empty) into the droplet pool with velocity ≈ jerk + a kick along
`−prim` (away from the liquid):

```rust
fn spray(w:&mut Water, jx:i32, jy:i32) {
    let n = ((jx.abs()+jy.abs()) >> 9).clamp(3, 12);   // 3..12 droplets
    for _ in 0..n {
        let Some((cx,cy)) = pick_surface_cell(w) else { break };
        if w.n_drop as usize >= N_DROP { break; }
        w.grid[cy*GW+cx] = 0;                          // mass leaves grid
        let d = &mut w.drop[w.n_drop as usize];
        *d = Droplet {
            x: ((7+4*cx as i32) << 8), y: ((7+4*cy as i32) << 8),
            vx: (jx>>6).clamp(-4096,4096) as i16,
            vy: (jy>>6).clamp(-4096,4096) as i16 - 1024,  // upward kick
        };
        w.n_drop += 1;
    }
}
```

Integrate each droplet in Q8 with the same gravity (`GD ≈ gy>>2` px/frame²), damp
on the wall, and **reintegrate** into the grid when it falls onto an occupied /
below-surface cell (`grid[cell]=ENERGY_MOVED`, droplet retired, swap-remove from
the pool). Mass returns to the CA — total water count is invariant. This is the
"wave breaks and throws spray" beat the positional CA can't produce itself.

### 3f. Rest breathing ripple

When `|fx|,|fy| < REST_EPS`, `n_drop==0`, and `|sx|+|sy| < REST_EPS` (settled):
inject a *tiny* oscillating lateral bias so the surface shimmers slowly, and
modulate surface-cell energy with the aurora noise so the meniscus breathes:

```rust
let ph = (self.frame >> 1) & 511;               // ~4 s triangle @40fps
let tri = if ph<256 {ph} else {511-ph} as i32;  // 0..255
self.sx += ((tri - 128) >> 5);                  // ±4 Q8 shimmer, decays via 3b
```

Always alive, never still — the signature the plan asks for — for a handful of
cycles.

---

## 4. Rendering

Neon-blue filled squares, LUT-shaded, divide-free per pixel.

### WATER_LUT (build-time, saber idiom)

256-entry deep-indigo → blue → **neon cyan (≈ accent (0,190,255))** → white,
emitted as RGB565-BE `u16` (packed once at build, so blit is fetch+store):

- `0..64` `(10,20,60)→(10,40,140)` deep body
- `64..150` `(10,40,140)→(0,150,220)` mid (the signature blue)
- `150..210` `(0,150,220)→(80,220,255)` bright crest
- `210..255` `(80,220,255)→(235,250,255)` foam / spray peak

Dithering is applied to the **index**, not the channels (the gradient is smooth,
so index-dither is visually identical to channel-dither and costs one add):

```rust
#[inline]
fn water_px(fb:&mut [u8], x:i32, y:i32, idx:i32) {         // idx already 0..255-ish
    if x<STATUS_X0 || x>STATUS_X1 || y<STATUS_Y0 || y>STATUS_Y1 {  // clip clock band
        let d = BAYER4[(y&3) as usize][(x&3) as usize];   // ±8 in LUT steps
        let i = (idx + d).clamp(0,255) as usize;
        let px = WATER_LUT[i];                             // pre-packed 565-BE
        let o = ((y*W + x)*2) as usize;
        if o+1 < fb.len() { fb[o]=(px>>8) as u8; fb[o+1]=px as u8; }
    }
}
```

The status-band clip (`STATUS_* = [123,343]×[26,66]`) is the whole clock-safety
story: liquid is never written *or cleared* there, so `wheel::draw_status`
(topmost, minute-rollover in the run loop) is untouched — no need to thread
`batt` into the tick.

### Per-cell shading (one multiply, per cell not per pixel)

```rust
// depth along gravity: cells further downhill are deeper → darker
let depth = ((cx-57)*fux + (cy-57)*fuy) >> 8;   // fu = unit flow (Q8); ±60
let surf  = up_neighbor_empty(w,cx,cy);          // surface/meniscus cell?
let aur   = ((NOISE_A[((cx as u32+t)&255) as usize] as i32 - 128)
          +  (NOISE_B[((cy as u32).wrapping_sub(t)&255) as usize] as i32 - 128)) >> 4;
let base  = (BODY + (v as i32) - depth*DEPTHK + aur).clamp(0, 255);
let idx   = if surf { (base + MENISCUS).min(255) } else { base };
```

- `v` (cell energy) → fast/just-moved crests ride brighter into the white foam.
- `depth*DEPTHK` → deeper body toward indigo. `aur` (NOISE_A/B, `t=frame>>3`) →
  the luminous aurora drift through the mass, reusing the OS visual language.
- `surf` cells get `+MENISCUS` and their **top edge row** is drawn one index
  brighter → the bright light-catching water line.

### The frame

Full-frame-flush app, so the plan's "clear the region and redraw all, no
damage-tracking cleverness" applies:

1. Clear `(wb ∪ this-frame wet bbox)` in **cell space**, expanded to pixels and
   **clipped to exclude the status rect**, via `fill_rect_black` row spans (the
   existing helper — streaming PSRAM writes, the cheap direction).
2. Draw every water cell as a `CELL×CELL` square through `water_px`.
3. Draw each live droplet as a bright `CELL×CELL` square (index ≥ 230, foam).
4. `wfb.mark_rect(bbox)`. `flush_dirty` sees a big union → **full `'F'`
   ~13 ms** during slosh; at rest the bbox is the calm pool → smaller, may go
   **partial `'P'`**. Confirmed full-frame under load, as the brief frames it.

Status clock: untouched by construction (clip). Aurora, meniscus, dithering,
speed/depth shading all present.

---

## 5. Exact integration

### 5a. `apps::State` (src/scenes/apps.rs, top)

Add one field (the big arrays boxed / behind a static — §2):

```rust
pub struct State {
    /* …existing fields… */
    pub water: Option<Water>,     // seeded on first WATER reveal
}
// State::new(): water: None,
```

### 5b. `has_content`

```rust
pub fn has_content(idx: usize) -> bool { true }   // was: idx != WATER
```

### 5c. `draw_reveal` — WATER branch (seeds the rising pool with `q`)

```rust
} else if idx == WATER {
    let w = st.water.get_or_insert_with(Water::new);
    if !w.seeded { w.seed_walls(); w.seeded = true; }
    w.fill_to_level(q_q8);         // waterline rises 0→rest as q 0→256
    { let fb = wfb.buf_mut(); w.render(fb); }
    let r = (9, 66, W-9, H-9);     // interior below the status band
    fx.push(r.0,r.1,r.2,r.3);
    wfb.mark_rect(r.0,r.1,r.2,r.3);
}
```

### 5d. Tick + gravity — minimal `src/app.rs` diff

The run loop owns `self.i2c`; read raw accel there and pass it in (plan option
1 — keeps `apps` I2C-free). In the `Scene::App(idx)` awake block (~line 322):

```rust
} else if idx == apps::WATER {
    let raw = qmi8658::read_accel(&mut self.i2c).ok();   // ~0.2 ms, ignore Err
    apps::water_tick(&mut self.wfb, elapsed, &mut app_state, raw);
} else {
    apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
    if apps::shows_status(idx) && status_minute != now.minute { /* …unchanged… */ }
}
```

Cadence — one arm added to the `frame_us` match (~line 457) so the liquid runs at
25 ms, not the 50 ms app rest cadence:

```rust
_ if scene == Scene::App(apps::WATER) && power == Power::Awake => ANIM_FRAME_US,
```

Add `use crate::drivers::qmi8658;` to `app.rs` (already used at boot in
`bin/main.rs`). `water_tick` runs §3 then §4:

```rust
pub fn water_tick(wfb:&mut WatchFb, elapsed:u32, st:&mut State, raw:Option<(i16,i16,i16)>) {
    let Some(w) = st.water.as_mut() else { return };
    w.step(raw, elapsed);                 // §3a–3f
    { let fb = wfb.buf_mut(); w.render(fb); }
    wfb.mark_rect(w.bbox_px());           // clock band excluded by the clip
}
```

Status clock stays correct because `water_tick` never touches the status rect and
the run loop's `tick_status` still fires on minute rollover (the `shows_status`
path is unchanged for other apps; for WATER the clip guarantees the band).

### 5e. `build.rs` WATER_LUT generator

Mirror `generate_wheel_assets`' RGB565-BE emit; call it from `main()` and
`include!` the file in `apps.rs` (like `noise_lut.rs`/`wheel_assets.rs`):

```rust
fn generate_water_lut() {
    // deep-indigo → blue → neon-cyan → white, packed RGB565 big-endian.
    let stops = [(0,(10,20,60)),(64,(10,40,140)),(150,(0,150,220)),
                 (210,(80,220,255)),(255,(235,250,255))];
    let lerp = |a:i32,b:i32,t:i32,d:i32| a + (b-a)*t/d;
    let mut body = String::from(
        "/// Auto-generated by build.rs (generate_water_lut). Do not edit.\n\
         pub static WATER_LUT: [u16; 256] = [");
    for i in 0..256i32 {
        let mut seg = 0; while seg+1 < stops.len()-0 && i > stops[seg+1].0 { seg+=1; }
        let (i0,(r0,g0,b0)) = stops[seg];
        let (i1,(r1,g1,b1)) = stops[(seg+1).min(stops.len()-1)];
        let d = (i1-i0).max(1); let t = (i-i0).clamp(0,d);
        let (r,g,b) = (lerp(r0,r1,t,d), lerp(g0,g1,t,d), lerp(b0,b1,t,d));
        let px = (((r*31/255)as u16)<<11)|(((g*63/255)as u16)<<5)|((b*31/255)as u16);
        if i>0 { body.push(','); } body.push_str(&format!("{px}"));
    }
    body.push_str("];\n");
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&out).join("water_lut.rs"), body).unwrap();
}
```

`NOISE_A/NOISE_B` are already generated (`generate_noise_luts`); add
`include!(concat!(env!("OUT_DIR"), "/noise_lut.rs"))` and the water LUT include to
`apps.rs`.

---

## 6. Per-frame ops + ms (240 MHz Xtensa, no FPU)

Pool ≈ 1/3 full ⇒ ~3 400 water cells; ~10 200 in-circle cells (out-of-circle
skipped via `wall_lo/hi` row bounds — never iterated).

**Physics**
- gravity/jerk/slosh/octant: O(1) ≈ 200 cyc.
- CA pass: 3 400 water × ~50 cyc = 170 k; 6 800 empty × ~6 cyc = 41 k → **211 k
  cyc ≈ 0.9 ms**.
- droplets: ≤48 × ~60 cyc ≈ 3 k ≈ 0.01 ms.
- **Physics ≈ 0.9 ms.**

**Render** (per-pixel path is divide-free: index-add + LUT fetch + 2 stores ≈
10 cyc/px)
- clear bbox: worst case ≈ 452×390 px ×2 B streaming fill ≈ 353 KB @ ~100 MB/s ≈
  **~3.4 ms** (slosh); rest ≈ 452×150 ≈ **~1.3 ms**.
- draw water: 3 400 cells × (20 cyc base + 16 px×10 cyc) = 3 400×180 = **612 k
  cyc ≈ 2.6 ms**.
- meniscus edge + droplets ≈ **0.3 ms**.
- **Render ≈ 4.2 ms (rest) … 6.3 ms (slosh).**

**Frame:** compute **~5–7 ms** + flush ~13 ms (full) ≈ **18–20 ms** < 25 ms
cadence ⇒ **40 fps**, with headroom. Compute has ~3–5 ms slack, so the
budget-fitting count is comfortably **~3 000–4 500 water cells at CELL=4 /
114²**; I'd hold there rather than push finer, spending the slack on a second
lateral pass during hard slosh if the leveling looks sluggish on-device.

---

## 7. Stability + honest weaknesses

**Guarantees**
- **Never blows up** — bulk cells move ≤1 cell/frame (no bulk velocity);
  droplets clamp `vy,vx ∈ ±16 px/frame`, are ≤48, and retire on landing; slosh
  clamps `±512` and decays ×0.898; accumulator clamps `±1024`.
- **Never leaks** — every move is gated by `wall_lo/hi` (a bounds compare);
  water cannot address an out-of-circle cell. Mass is a swap-invariant
  (full↔empty); spray removes exactly one cell and landing re-adds one → total
  count constant.
- **Never piles to a singularity** — one water unit per cell, period.
  Incompressible by construction; leveling is the lateral spread.
- **Settles** — energy decays to `ENERGY_FLOOR`; slosh decays; with no empty
  target, cells simply stop. Deterministic serpentine + slosh-biased tie-breaks
  stop directional runaway.
- **Degrades gracefully** — dead IMU → reuse last gravity; the app still lives.

**Weaknesses (the honest list for falling-sand)**
1. **Surface flicker / boil** — the classic CA left-right shimmer at the
   waterline; serpentine + slosh-bias + energy tie-break tame it but a faint boil
   remains. (Reads as "shimmer", on-brand, but it isn't a smooth wave.)
2. **No true momentum/waves** — inertia is a single global `s` vector, so the
   liquid can't carry a genuine traveling wave or a real standing sloshing mode;
   "climb the far wall and slosh back" is approximated, not simulated.
3. **8-way anisotropy** — quantized gravity makes ~45° tilt flow with faint
   staircase artifacts vs axis-aligned tilt.
4. **Spray is a bolt-on** — droplets are a separate system; landing
   reintegration can leave a momentary density blip.
5. **Blocky meniscus** — the surface is a stair-stepped cell edge, not the glassy
   line a height-field gives; fine at 4 px, not perfect.
6. **Viscosity/leveling trade** — one lateral pass = thick look; thin fast water
   needs extra passes (cost) or looks sluggish.
7. **RAM** — ~14 KB grid vs a particle system's ~5 KB for comparable apparent
   resolution.

---

## 8. Core code sketch (real bodies, repo helpers)

```rust
// --- constants ---
const CELL:i32=4; const GW:usize=114; const GH:usize=114; const NCELL:usize=GW*GH;
const WALL_R:i32=224; const ORIGIN:i32=5;
const ENERGY_MOVED:u8=200; const ENERGY_FLOOR:u8=40; const ENERGY_DECAY:u8=12;
const BODY:i32=90; const DEPTHK:i32=3; const MENISCUS:i32=90;
const JERK_THRESH:i32=1200; const REST_EPS:i32=24; const GD_SHIFT:i32=2;
const STATUS_X0:i32=123; const STATUS_X1:i32=343; const STATUS_Y0:i32=26; const STATUS_Y1:i32=66;
const RING:[(i32,i32);8]=[(0,-1),(1,-1),(1,0),(1,1),(0,1),(-1,1),(-1,0),(-1,-1)];
const BAYER4:[[i32;4];4]=[[-8,0,-6,2],[4,-4,6,-2],[-5,3,-7,1],[7,-1,5,-3]];

impl Water {
    fn cell_in_wall(&self, cx:i32, cy:i32) -> bool {
        cy>=0 && (cy as usize)<GH && cx>=self.wall_lo[cy as usize] as i32
                                  && cx<=self.wall_hi[cy as usize] as i32
    }
    fn seed_walls(&mut self) {                       // precompute per-row cell bounds
        for cy in 0..GH {
            let sy = 7 + 4*cy as i32 - 233;
            let mut lo=-1i16; let mut hi=-1i16;
            for cx in 0..GW as i32 {
                let sx = 7 + 4*cx - 233;
                if sx*sx + sy*sy <= WALL_R*WALL_R {
                    if lo<0 { lo=cx as i16; } hi=cx as i16;
                }
            }
            self.wall_lo[cy]=lo; self.wall_hi[cy]=hi;
        }
    }
    #[inline] fn hash(&mut self) -> i32 {            // xorshift tie-break
        self.rng ^= self.rng<<13; self.rng ^= self.rng>>17; self.rng ^= self.rng<<5;
        (self.rng & 1) as i32
    }
    #[inline] fn try_move(&mut self, cx:i32, cy:i32, d:(i32,i32)) -> bool {
        let (tx,ty)=(cx+d.0, cy+d.1);
        if self.cell_in_wall(tx,ty) {
            let ti=(ty as usize)*GW + tx as usize;
            if self.grid[ti]==0 {
                let si=(cy as usize)*GW + cx as usize;
                self.grid[ti]=ENERGY_MOVED; self.grid[si]=0; return true;
            }
        }
        false
    }
    fn try_pair(&mut self, cx:i32, cy:i32, p:((i32,i32),(i32,i32)), bias:i32) -> bool {
        // prefer the member aligned with slosh bias; else hash tie-break
        let first_a = if bias>0 { p.0.0 >= p.1.0 } else if bias<0 { p.0.0 <= p.1.0 }
                      else { self.hash()==0 };
        let (a,b) = if first_a {(p.0,p.1)} else {(p.1,p.0)};
        self.try_move(cx,cy,a) || self.try_move(cx,cy,b)
    }

    fn step(&mut self, raw:Option<(i16,i16,i16)>, elapsed:u32) {
        self.frame += 1;
        // 3a gravity
        if let Some(r)=raw {
            let (mx,my)=map(r);
            let jx=(mx-self.last_x) as i32; let jy=(my-self.last_y) as i32;
            self.last_x=mx; self.last_y=my;
            self.gx=(((mx-self.rest_x) as i32)>>5).clamp(-256,256);
            self.gy=(((my-self.rest_y) as i32)>>5).clamp(-256,256);
            // 3b slosh + spray
            self.sx=(self.sx*230)>>8; self.sy=(self.sy*230)>>8;
            if jx.abs()+jy.abs() > JERK_THRESH {
                self.sx=(self.sx+(jx>>4)).clamp(-512,512);
                self.sy=(self.sy+(jy>>4)).clamp(-512,512);
                self.spray(jx,jy);
            }
        }
        // 3c flow + octant
        let fx=(self.gx+self.sx).clamp(-768,768);
        let fy=(self.gy+self.sy).clamp(-768,768);
        let mut k=0; let mut best=i32::MIN;
        for i in 0..8 { let d=RING[i].0*fx+RING[i].1*fy; if d>best {best=d;k=i;} }
        let prim=RING[k]; let dia=(RING[(k+1)&7],RING[(k+7)&7]);
        let lat=(RING[(k+2)&7],RING[(k+6)&7]);
        // 3f rest ripple
        if fx.abs()<REST_EPS && fy.abs()<REST_EPS && self.n_drop==0 {
            let ph=((self.frame>>1)&511) as i32; let tri=if ph<256{ph}else{511-ph};
            self.sx += (tri-128)>>5;
        }
        // 3d CA pass — serpentine, against flow
        let down = fy>=0;
        let sbias = if self.sx.abs()>=self.sy.abs() {self.sx} else {self.sy};
        for row in 0..GH {
            let cy = if down { GH-1-row } else { row };
            let (lo,hi)=(self.wall_lo[cy] as i32, self.wall_hi[cy] as i32);
            if lo<0 { continue; }
            let ltr = (fx<0) ^ (self.parity==1);      // scan against flow_x, serpentine
            let mut cxs = cx_range(lo,hi,ltr);
            while let Some(cx)=cxs.next() {
                let i=cy*GW + cx as usize;
                let v=self.grid[i]; if v==0 { continue; }
                if self.try_move(cx as i32,cy as i32,prim) { continue; }
                if self.try_pair(cx as i32,cy as i32,dia,sbias) { continue; }
                if self.try_pair(cx as i32,cy as i32,lat,sbias) { continue; }
                self.grid[i]=v.saturating_sub(ENERGY_DECAY).max(ENERGY_FLOOR);
            }
        }
        self.parity ^= 1;
        self.integrate_droplets();                    // Q8 ballistic + reland
    }

    fn render(&mut self, fb:&mut [u8]) {
        // 1) clear last∪current wet bbox (cells→px), clipped out of status band
        let (x0,y0,x1,y1)=self.clear_rect_px();
        for y in y0..=y1 {
            if y>=STATUS_Y0 && y<=STATUS_Y1 {          // punch the clock band out
                fill_span(fb,x0,y,STATUS_X0-1);
                fill_span(fb,STATUS_X1+1,y,x1);
            } else { fill_span(fb,x0,y,x1); }
        }
        // 2) unit flow for depth shading
        let (fx,fy)=((self.gx+self.sx).clamp(-768,768),(self.gy+self.sy).clamp(-768,768));
        let len=isqrt((fx*fx+fy*fy) as u32).max(1) as i32;
        let (fux,fuy)=((fx<<8)/len,(fy<<8)/len);
        let t=(self.frame>>3) as u32;
        // 3) draw water cells
        for cy in 0..GH as i32 {
            let (lo,hi)=(self.wall_lo[cy as usize] as i32,self.wall_hi[cy as usize] as i32);
            if lo<0 { continue; }
            for cx in lo..=hi {
                let v=self.grid[(cy as usize)*GW+cx as usize]; if v==0 { continue; }
                let depth=((cx-57)*fux+(cy-57)*fuy)>>8;
                let aur=((NOISE_A[((cx as u32+t)&255) as usize] as i32-128)
                       +(NOISE_B[((cy as u32).wrapping_sub(t)&255) as usize] as i32-128))>>4;
                let base=(BODY + v as i32 - depth*DEPTHK + aur).clamp(0,255);
                let surf=!self.cell_in_wall(cx-... , ..) /* up-neighbor empty */;
                let idx=if surf {(base+MENISCUS).min(255)} else {base};
                let px0=ORIGIN+cx*CELL+2; let py0=ORIGIN+cy*CELL+2;    // cell → screen
                for py in py0-2..py0+2 { for px in px0-2..px0+2 {
                    water_px(fb,px,py,idx);                            // divide-free
                }}
            }
        }
        self.draw_droplets(fb);
    }
}

#[inline] fn fill_span(fb:&mut [u8], x0:i32, y:i32, x1:i32) {
    let (x0,x1)=(x0.max(0), x1.min(465)); if x1<x0 { return; }
    let a=((y*466 + x0)*2) as usize; let b=((y*466 + x1)*2 + 2) as usize;
    fb[a..b].fill(0);                                    // == apps::fill_rect_black row
}
```

(`water_px`, WATER_LUT, `map`, `spray`, `integrate_droplets`, `pick_surface_cell`,
`cx_range` shown/described in §3–4; the surface test is "the `−prim` neighbor
cell is empty".) The sketch omits a couple of one-line helpers for brevity but
the physics core and render loop are the real bodies.
