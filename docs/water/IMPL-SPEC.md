# Water — canonical implementation spec (SYNTHESIS)

Status: DEFINITIVE. This is the build-of-record for the Water app, synthesised
from the five designs (`design-*.md`) and the three judge reports
(`judge-*.md`). It resolves every conflict and grafts every must-adopt idea
that fits the chosen model. The reference module is
[`water_draft.rs`](water_draft.rs); the exact wiring diffs are in
[`integration.md`](integration.md).

Grounded in the real repo as of this commit: `apps.rs` integer helpers
(`set_px`/`max_px`/`isqrt`/`soft_dot`/`fill_rect_black`), `qmi8658::read_accel`
(8192 LSB/g, `-> Result<(i16,i16,i16),()>`), `lock::NOISE_A/NOISE_B`
(`pub static [u8;256]`, values ~[40,215]), `WatchFb::buf_mut/mark_rect`
(RGB565-BE, W=H=466), the tick call site (`app.rs:324`), `frame_us`
(`app.rs:457`), `flush_dirty` (`app.rs:1001`, 3/4 partial rule), `open_app`
(`app.rs:1568`), and the `generate_noise_luts` RGB565-BE emit idiom in
`build.rs`.

---

## 1. Chosen model + why

**A 2-D pairwise-hash particle liquid** — ~448 neon squares carrying position +
velocity, a uniform spatial hash, and sqrt-free/divide-free short-range
repulsion for incompressibility — **grafted with every surface, stability, and
integration idea the judges flagged as must-adopt.**

### Why this model (resolving the split verdict)

The three lenses disagreed, so the synthesis lead must *resolve*, not obey the
loudest voice:

| Lens | 1st | Key finding |
|---|---|---|
| realism-feel | flip-lite 88 (**pairwise 85, "coin-flip … safer, more robust"**) | wants a genuine incompressible-AND-momentum 2-D core |
| integration-perf | heightfield 90 (**flip-lite LAST, 62**) | flip-lite = heaviest SRAM, `static mut`+unsafe, under-counted Jacobi divides, least buildable |
| overflow-correctness | heightfield 92 (**flip-lite 74, pic-grid 70**) | pairwise = "roomiest headroom, overflow a non-issue" |

Raw aggregate across all three lenses: **heightfield 256 > pairwise 239 >
flip-lite 224 > pic-grid 220 > falling-sand 214.**

- **Heightfield is disqualified as the spine.** It wins two lenses on technical
  merit, but the realism judge is decisive and correct: a 1-D column field is a
  literal **"stiff bar"** — it has *one* slosh axis (roll), pitch does nothing,
  and it cannot go past vertical. That is a direct failure of the product goal
  ("runs downhill and climbs the far wall on tilt" — on a wrist, in every
  direction). The integration judge itself says heightfield's limits are
  "simulation-quality" and recommends layering a particle/spray layer on top for
  true 2-axis slosh. We take its *techniques*, not its spine.
- **Flip-lite is rejected as the spine.** It is the realism winner, but *two of
  three* judges — the two that own stability and feasibility — rank it last/4th:
  16.4 KB in `static mut` + `unsafe` (against codebase doctrine), Jacobi divides
  under-counted (~1.3 ms unbudgeted), the most bug-prone P2G/G2P/solve surface,
  a self-admitted garbled code line, and "finicky to tune." The brief says
  **"prefer correctness and stability over cleverness"** and this is a
  **hardware-in-the-loop first-flash** context where the IMU mapping is unknown.
  Shipping the most fragile option here is the wrong bet. The realism judge
  itself calls pairwise a **coin-flip** on realism and **"the safer, more robust
  design."**
- **Pairwise-hash is the highest-aggregate model that satisfies the 2-axis
  product goal (239),** it is the realism runner-up and coin-flip equivalent, it
  ranks *above* flip-lite on both stability lenses (74 / 80), and — critically —
  it **natively owns most of the must-adopt surface ideas** (viscosity term,
  SET-overlap sheet, per-particle meniscus, the `PUSH_LUT` build.rs gem, a
  correct shrinking bbox, Q6/Q8 overflow headroom, a complete buildable sketch).
  It is a normal struct in `apps::State` — **no `static mut`, no `unsafe`.**

### How the must-adopts are honoured (conflict resolutions)

- **"Adopt flip-lite's FLIP core / divergence-free projection" (realism #1).**
  Resolved by transplanting the *purpose* (global incompressibility + momentum),
  not the fragile MAC/P2G/G2P/Jacobi machinery: pairwise repulsion gives local
  incompressibility at a *finer* 16 px scale than flip-lite's 20 px grid;
  momentum is carried on the particles + the viscosity velocity-network; and
  **`DAMP≈0.992` is taken verbatim.** We consciously do **not** import the
  projection — the two stability judges rank that machinery last and the brief
  prioritises correctness. If on-device leveling looks soft, the tuning knob is
  a second relax pass (bounded cost), not a rewrite.
- **"Emergent spray, not a bolted-on pool" (realism #2) vs "bolt-on spray is
  needed" (integration #9).** Resolved elegantly by the pairwise mechanism:
  on a flick, *surface particles* (already flagged) get an up/out velocity kick
  — they are **real fluid particles** that arc under gravity and re-absorb via
  repulsion. Same array, no separate droplet pool, mass conserved. This is
  emergent (satisfies realism) *and* an explicit surface-lift→ballistic→re-absorb
  path (satisfies integration).
- **"Strict volume-conservation nudge" (realism #7 / overflow #5).** In a
  particle system mass is conserved *by construction* — particle count is
  invariant, so the pool can neither drain nor flood. This is **stronger** than a
  height-field nudge (an impossibility, not a correction), per the overflow
  judge's own "prefer structural impossibility" doctrine. Adopted as the
  guarantee via the stronger mechanism.

### Signature look (unchanged from the brief)

A luminous swarm of tiny neon-blue squares on black AMOLED: dense/merged into a
solid dithered cyan sheet in the pooled body, brighter at fast crests, deeper
indigo in the body, a slow aurora drift through the mass, and one bright,
tilting meniscus water-line where it meets air. Always alive (breathing at
rest), rock-solid stable (never blows up, leaks, or piles to a singularity).

---

## 2. Fixed-point scheme + overflow proof

`dt ≡ 1 frame` (folded into the accel/velocity units → `pos += v` and `v += g`
are bare adds; **no `dt` multiply in the hot loop**). No `f32` anywhere in
per-particle code.

| Quantity | Format | Unit | Storage | Range used |
|---|---|---|---|---|
| Particle pos `px,py` | **Q6** | 1/64 px | `i16` | 0..471 px → 0..30 144 |
| Particle vel `vx,vy` | **Q6** | 1/64 px/frame | `i16` | clamp ±`VMAX`=1024 (16 px) |
| Gravity/slosh `gx,gy,sx,sy` | Q6 | 1/64 px/frame² | `i32` | clamp ±`GMAX`=512 / ±VMAX |
| Mapped raw / jerk | integer | LSB (8192=1 g) | **`i32`** | ±(32767+820), ±65534 |
| Rest bias `bias_x,y` | integer | LSB | `i32` | clamp ±820 (±0.1 g) |
| `d2` (pair sep²) | integer | px² | `i32` | 0..255 after H2 reject |
| `PUSH_LUT[d2]` | Q-scalar | build-time | `i16` | 0..8192 (clamped) |

**Why Q6 `i16` positions fit.** Worst on-screen coordinate =
`CX + WALL_R + VMAX_PX = 233 + 222 + 16 = 471 px`. `471 << 6 = 30 144 <
32 767` ✔ — even the one-frame overshoot before the wall projection stays inside
`i16`. The **VMAX velocity clamp is the keystone** that guarantees this (and
every downstream multiply). Q6 (chosen over pairwise's Q8-i32) halves position
storage to honour the "few KB" SRAM budget; flip-lite's proof already showed
Q6-i16 positions are safe, and Q6 = 1/64 px carries the slow breathing motion
smoothly.

### Overflow proof — every hot-loop multiply (worst case → i32, max 2 147 483 647)

The mapped raw and the jerk are kept in **`i32`, never stored sign-flipped in
`i16`** (this is the overflow judge's item #7 — the `-1 * -32768` hole that sank
pic-grid; avoided here).

1. **Gravity map** `g = (rawmap − bias) >> 6`. `|rawmap−bias| ≤ 32767+820 =
   33 587`. It is a **shift, not a multiply** (no product). `>>6 → ≤525`, clamped
   ±512. ✔
2. **Jerk → slosh** `s = (jx) >> 6`, `jx = rmx − lrmx`, `|jx| ≤ 65 534`
   (i32). Shift, no product. `>>6 → ≤1023`, clamped ±VMAX. ✔
3. **Integrate / advect** `px += vx`, `vx += gx + sx` — adds only, operands ≤
   `VMAX + GMAX + VMAX ≈ 2560`. No multiply. ✔
4. **Damping** `v*DAMP >> 8`. `|v| ≤ 1024`, `DAMP = 254` → `1024·254 =
   260 096`. ✔
5. **Pair separation²** `d2 = dx·dx + dy·dy` in **whole px** (`dx = (px_i>>6) −
   (px_j>>6)`, the deliberate reduction). 3×3 block of 16 px cells spans 48 px →
   `|dx| ≤ 48`. `48² = 2304`, `d2 ≤ 4608` (< H2 reject leaves 0..255). ✔
6. **Repulsion impulse** `(PUSH_LUT[d2]·dx) >> 7`. After reject `|dx| < 16`,
   `PUSH_LUT ≤ 8192` → `8192·16 = 131 072`. Summed over ≤`K_CAND`=24 in i32 →
   `≤ 24 576` per axis, then re-clamped to ±VMAX. ✔
7. **Viscosity** `((vj − vi)·VISC_K) >> 8`. `|vj−vi| ≤ 2·VMAX = 2048`,
   `VISC_K ≤ 64` → `2048·64 = 131 072`. ✔
8. **Wall dist²** `r2 = dx·dx + dy·dy` in px, `|dx| ≤ 239` →
   `239²·2 = 114 242`. ✔ (input to `isqrt`.)
9. **Wall projection** `((dx·WALL_R) << 6) / r`. `|dx|·WALL_R ≤ 239·222 =
   53 058`; `<<6 = 3 395 712`; `/r` (`r ≥ WALL_R = 222`). **Largest intermediate
   anywhere = 3.4 M — 0.16 % of i32.** ✔ Result lands at ≤455 px = 29 120 Q6,
   inside `i16`. ✔
10. **Wall reflect** `vn = (vx·dx + vy·dy)/r`. `|vx·dx| ≤ 1024·239 = 244 736`,
    ×2 = 489 472, `/r → |vn| ≤ 2205`. `k = (vn·282) >> 8 ≤ 2429`; `k·dx ≤
    2429·239 = 580 531`. ✔
11. **Render speed** `spd2 = vx·vx + vy·vy = 1024²·2 = 2 097 152` (u32
    `isqrt`). ✔
12. **Cell index** `(y>>4)·30 + (x>>4) ≤ 29·30 + 29 = 899 < 900`. ✔

Every multiply is < 3.4 M — **≥ 630× inside i32; no `i64` anywhere.** The two
load-bearing clamps (`VMAX` on velocity, `WALL_R` on the projected radius) are
simultaneously the **stability** caps and the **overflow** caps — the coupling
the overflow judge rewards (item #1).

---

## 3. State layout + SRAM byte count

A single `Water` sub-struct held as `pub wa: Water` **in `apps::State`**
(internal SRAM, random-access every frame, `const fn new()`). **No `static
mut`, no `unsafe`** — the design's structural advantage over flip-lite/falling-sand.

| Buffer | Elems | Bytes |
|---|---|---|
| `px,py : i16` | 448×2 | 1 792 |
| `vx,vy : i16` | 448×2 | 1 792 |
| `nbr : u8`, `flags : u8` | 448×2 | 896 |
| `cell_start : u16` | 901 | 1 802 |
| `cursor : u16` | 901 | 1 802 |
| `order : u16` | 448 | 896 |
| `surf_top : i16` (meniscus columns) | 64 | 128 |
| scalars (bias, last accel, rng, phase, bbox, flags) | — | ~40 |
| **Total** | | **≈ 9 148 B ≈ 8.9 KB** |

`WATER_LUT` (`[(u8,u8);256]`, 512 B) and `PUSH_LUT` (`[i16;257]`, 514 B) are
build-time `.rodata` in flash — **not** SRAM state.

8.9 KB is comfortable on the S3's 512 KB SRAM. `apps::State` is a `run()` stack
local, so this grows the run-loop frame by 8.9 KB — fine on the esp-hal main
stack, but **if first-flash stack head-room is ever tight, promote the single
`Water` field to a module `static` with zero logic change** (every method
already takes `&mut self`). This is the documented escape hatch; the default is
fields-in-`State` per the brief and the doctrine both non-realism judges reward.

---

## 4. Constants (the single tuning block)

All are first-flash-editable. Values chosen and justified; on-device tuning is a
const edit, never a rewrite.

```
// vessel
WALL_R      = 222     // 1 px inside BEZEL_R=223 (no bezel bleed)
CLOCK_Y1    = 70      // liquid clipped below the status band (~26..66)

// particles
NW          = 448     // denser than pairwise 320; worst-slosh frame still < 25 ms
GRID_W      = 30      // hash 30x30, CELL_PX = 16 (>= H_PX -> 3x3 exact)
H_PX        = 16      // interaction radius (finer than the 20 px grids), H2 = 256
K_CAND      = 24      // candidate cap/particle -> bounded relax cost

// fixed point (Q6)
VMAX_PX     = 16      // velocity clamp = 1024 Q6 (CFL ~ 1 cell; overflow keystone)

// dynamics
DAMP        = 254     // 0.992/frame (flip-lite) -> always settles
REST_E      = 26      // wall restitution e ~ 0.10
PUSH_SHIFT  = 7       // repulsion impulse >> 7
VISC_K      = 40      // viscosity strength /256 (surface smoothing)

// IMU map (MEASURE ON DEVICE)
IMU_X_SRC=0 IMU_X_SGN=+1   IMU_Y_SRC=1 IMU_Y_SGN=+1
GRAV_SHR    = 6       // raw>>6 -> 128 Q6 (2 px/frame^2) at 1 g
GMAX        = 512     // +-4 g planar clamp
DOWN_G      = 128     // dead-IMU down-vector (1 g)
REST_BIAS_CLAMP = 820 // +-0.1 g  (removes zero offset only, never a tilt)

// jerk / spray
JERK_TH     = 2600    // flick threshold (~0.32 g L1)
SPRAY_SHR   = 8       // surface up-kick = jmag >> 8

// breathing
BREATHE_G_TH = 40     // breathing fades out by ~0.3 g planar
BREATHE_AMP  = 48     // peak Q6 accel (a shimmer)

// surface / meniscus (hysteresis)
SURF_LO = 3   SURF_HI = 5

// render
SQ          = 5       // body square side (SET; overlaps -> sheet)
SQ_MARGIN   = 4       // clear/bbox half-extent (covers square + r3 glow)
NMENISC     = 64      // meniscus-line columns

// WATER_LUT stops (deep-indigo -> neon-blue -> cyan -> white), RGB565-BE:
//   0:(2,4,22)  72:(0,46,130)  150:(0,150,235)  208:(120,226,255)  255:(232,250,255)
// PUSH_LUT[d2] = clamp( STIFF*(H_PX - sqrt(d2)) / sqrt(d2), 0, 8192 ), STIFF=1600
```

---

## 5. Per-frame algorithm (step by step)

Cadence pinned to `ANIM_FRAME_US` = 25 ms (40 fps). One frame:

**0. Gravity + control (once, O(1)).**
   - `acc = imu.unwrap_or(last)`; `live = imu.is_some()`.
   - First tick: `open()` seeds the pool; `need_calib` captures the rest bias
     (clamped ±0.1 g) on the first *live* sample.
   - `rmx = IMU_X_SGN·axis(acc,IMU_X_SRC)` (i32); `gx = ((rmx−bias_x) >> 6)`
     clamped ±GMAX (dead IMU → `(0, DOWN_G)`).
   - Jerk `jx = rmx − last_rmx` (i32); if `|jx|+|jy| > JERK_TH`: `sx = (jx>>6)`
     clamped ±VMAX, `spray = true`.
   - Breathing triangle scaled by `(BREATHE_G_TH − |g|)/BREATHE_G_TH`, zero once
     tilted.

**1. Integrate + advect (per particle).**
   `v += g + slosh`; surface particles also `+= breathe` and, on `spray`, an
   up/out kick (`vy −= jmag>>8` + rng jitter). Clamp `|v| ≤ VMAX`. `pos += v`.

**2. Build hash.** CSR count-sort into `order` by 16 px cell (rebuilt on the
   *new* positions so the relax neighbour search is exact).

**3. Relax (per particle, candidate-capped).** Scan the 3×3 cell block; for each
   neighbour with `d2 < H2`:
   - **repulsion** `(PUSH_LUT[d2]·(dx,dy)) >> 7` (sqrt-free, `1/d` baked in) —
     incompressibility + anti-singularity;
   - **viscosity** nudge `v_i` toward `v_j` by `VISC_K/256` — surface smoothing;
   - increment the in-radius count `n`.
   Apply *half* the accumulated impulse (each pair seen from both ends),
   re-clamp `v`. Latch the **hysteresis surface flag** (`surface` if `n<SURF_LO`,
   clears at `n>SURF_HI`).

**4. Damp, then clamp.** `v = (v·DAMP)>>8`, re-clamp `|v| ≤ VMAX` — energy
   strictly dissipative and bounded every frame (blow-up impossible).

**5. Round wall (per particle).** If `r² > WALL_R²`: `r = isqrt(r²)`; **hard
   positional projection** onto `r = WALL_R` (no leak, independent of the
   reflect); if the normal speed is outward, reflect it with restitution
   `(1+e)`, keeping the tangential (water slides along glass, loses energy).

**6. Render.** Compute this frame's tight bbox; clear `union(this, last_bbox)`
   via row spans; splat 5×5 SET squares through `WATER_LUT` (index-dithered,
   aurora-shaded); sparkle surface particles (`max_px`); lay the continuous
   meniscus line over the swarm; set `last_bbox = this tight bbox`; `mark_rect`
   the union. (§7.)

---

## 6. IMU axis-map + calibration (the one true unknown)

The IMU→screen mapping and rest bias are unknown until measured on the board
(exactly like the touch Y-flip). They are exposed as consts + a capture-on-open
offset so first-flash tuning is a **const edit, not a rewrite.**

**Tunable consts:** `IMU_X_SRC`/`IMU_X_SGN`, `IMU_Y_SRC`/`IMU_Y_SGN` (which raw
axis drives each screen axis, and its sign).

**On-device tuning recipe:**
1. Temporarily `println!("{ax} {ay} {az}")` from the run-loop read.
2. Hold the watch flat, face-up. Confirm `az ≈ ±8192` and `ax, ay ≈ 0` (gravity
   is along z → in-plane g ≈ 0, the pool should sit still and breathe).
3. Tilt **top-edge down**: the axis whose magnitude grows is the screen-**y**
   source; set `IMU_Y_SRC`. Sign so the water runs to screen-top → `IMU_Y_SGN`.
4. Tilt **right-edge down**: gives `IMU_X_SRC` / `IMU_X_SGN` the same way.
5. Bake the four consts. Done — no logic changes.

**Rest calibration:** on the first live tick after open, `bias = mapped raw`
**clamped to ±0.1 g (`REST_BIAS_CLAMP=820`)** and subtracted every frame. The
clamp is deliberate and load-bearing: it removes only the sensor's zero offset,
so a flat watch reads in-plane g ≈ 0 (breathes) while any real tilt *always*
dominates (calibration can never cancel it) — the most correct pool/tilt
behaviour (pairwise §3.1). **Dead-IMU fallback:** if `read_accel` never
succeeds, gravity falls back to a fixed down-vector `(0, DOWN_G)` so the app
still pools and settles; a transient bus error reuses the last vector.

---

## 7. Rendering

- **Body squares.** Each particle is a **5×5 SET** of a pre-baked `WATER_LUT`
  RGB565-BE pair — **raw byte store, divide-free per pixel** (the LUT is
  pre-packed; `set_px`/`blend_px` do ~6 `/255` per pixel and would cost ~7 ms
  across ~50 k px — forbidden in the hot path). Overlapping squares in the dense
  body **merge into a solid dithered cyan sheet ("liquid, not dots")**; sparse
  surface particles read as individual sparkles. `max_px` is reserved for glow
  only.
- **LUT index (per particle, one `isqrt`):**
  `idx = base(surface?206:150) + (speed>>4) + aurora − depth`, clamped 0..255.
  `speed = isqrt(vx²+vy²)` → crests/spray brighten to white; `depth = nbr·3` →
  deep body toward indigo.
- **Aurora drift.** `aurora = (NOISE_A[(x>>2 + t1)&255] + NOISE_B[(y>>2 −
  t2)&255] − 256) >> 3` (≈ ±22), reusing `lock::NOISE_A/NOISE_B` counter-scrolled
  by `t1 = ms>>5`, `t2 = ms>>6` — a luminous band drifts through the mass, tying
  Water into the OS aurora language. (NOISE is ~[40,215]; `−256` centres the
  pair near zero — a shading choice, not a stability issue, per the overflow
  judge's noise-range note.)
- **Dither.** A 4×4 Bayer value (±7) added to the LUT **index** before lookup,
  per pixel, **lit pixels only** — kills RGB565 banding without touching the
  black background (build.rs doctrine).
- **Meniscus — two layers.** (1) Per-particle: surface particles (hysteresis
  flag) get a `soft_glow` `max_px` sparkle. (2) The **continuous water line**:
  the topmost particle per screen-column (`surf_top[64]`) is connected into one
  bright, tilting cyan line laid *over* the swarm (`max_px`, 2 px) — the glassy
  light-catching waterline particles alone can't produce (heightfield's idea).
- **Clock stays topmost.** All liquid render is **clipped to `y ≥ CLOCK_Y1`
  (70)**; the clear rect's `y0` is clamped the same way. The retained status
  clock (band ~26..66) is never cleared or overdrawn; the run loop restamps it
  on minute rollover. Liquid climbing that high simply clips under the clock,
  which reads correctly as "liquid below the clock." No per-pixel clock-band
  branch in the hot loop (pic-grid's avoidable tax).
- **Damage / flush (adaptive, shrinking bbox).** `last_bbox = THIS frame's TIGHT
  bbox`; `mark_rect(union(this, last))`. `flush_dirty`'s 3/4 rule then does the
  right thing automatically: **partial when pooled** (bbox ~440×90 → ~79 KB <
  ¾-frame → ~2–3 ms), **full-frame (~13 ms) under slosh** when the liquid sheets
  across the disc. Do **not** union with the accumulated previous bbox
  (pic-grid's bug that pins the app to permanent full-flush). Clear **only** the
  tracked region via `fill_rect_black` row spans — never memset the 424 KiB
  canvas (a full black fill alone is ~5 ms).

---

## 8. Per-frame ops / ms budget

240 MHz. Cadence 25 ms − flush ≈ compute budget. All state in internal SRAM.

**Physics (N=448, candidate-capped):**

| Stage | ops | notes |
|---|---:|---|
| gravity/jerk/breathe | ~60 | O(1) |
| integrate + clamp + advect | ~4 500 | 448 × ~10 |
| build hash (count+prefix+scatter) | ~2 250 | 2·448 + 900 |
| **relax (repulsion+viscosity+surface)** | **~150 500** | 448 × 24(cap) × ~14 — dominant, **sqrt/divide-free** |
| damp + clamp | ~1 800 | 448 × 4 |
| wall (dist² all; project ~40 near rim) | ~3 900 | 448×6 + 40×30 |
| **Physics total** | **~163 k ops** | ≈ **~5 ms** |

**Render (PSRAM writes dominate):**

| Stage | cost | notes |
|---|---:|---|
| bbox scan | ~1 800 ops | 448 × 4 |
| clear union bbox | ~1–2 ms typ / ~4 ms worst | row-span fill (79 KB pooled … 343 KB sloshed) |
| splat 448 × 25 px SET+LUT+dither | ~0.7 ms | ~11 k px, divide-free |
| surface glow (~60 × r3 `max_px`) | ~0.3 ms | crest sparkle |
| meniscus line (64 cols) | ~0.1 ms | `max_px` |
| **Render total** | ≈ **~2–3 ms typ / ~5 ms worst** | |

**Frame:** typical (pooled) ≈ 5 (phys) + 2.5 (render) + 2.5 (partial flush) ≈
**~10 ms**; worst (full slosh) ≈ 5 + 5 + 13 (full flush) ≈ **~23 ms** — inside
the 25 ms cadence with ~2 ms margin. **40 fps, flush-bound.** The candidate cap
guarantees the worst-case relax cost; without it a compressed pool could push
relax past budget. (Doubles toward the render bound with perf-plan **P1 async
flush** — Water is the app that most wants it.)

---

## 9. Stability guarantees

**Never blows up.** `|v|` is hard-clamped to ±VMAX **after** damping every frame
(the keystone) and `DAMP < 1` bleeds energy → kinetic energy is bounded and
net-dissipative; absent input the pool always settles. `PUSH_LUT` is build-time
clamped to 8192 **and** clamped for `d→0`, so no unbounded force exists. Every
hot multiply is proven < 3.4 M (§2) — wraparound corruption is not even
representable.

**Never leaks.** The wall does a **hard positional projection** onto exactly
`r = WALL_R` for every out-of-bounds particle each frame, *independent of the
velocity reflect* — a leak is impossible even if the reflect were disabled.
`VMAX_PX = 16 < WALL_R = 222` interior margin ⇒ no tunnelling; nothing is
outside the circle at frame end.

**Never piles to a singularity.** Pairwise repulsion enforces a minimum spacing;
the `d→0` `PUSH_LUT` clamp makes even a direct overlap resolve in a few frames
with a strong but *bounded* push. The mass *spreads and levels* incompressibly
at the UI scale rather than heaping.

**Volume conserved by construction.** Particle count is invariant — the pool can
neither drain nor flood (stronger than a height-field nudge; an impossibility,
not a correction).

**Meniscus never flickers.** Surface detection has hysteresis (`SURF_LO=3`,
`SURF_HI=5`).

**Honest weaknesses (pairwise-specific).** Incompressibility is *bounded local
repulsion*, not a global projection → leveling is slightly softer than flip-lite
and the surface can boil a little under a hard sustained tilt (the viscosity term
+ SET-sheet + meniscus line mitigate; a 2nd relax pass is the knob). Density is
448 (dictated by the relax-cost-scales-with-density reality, not 640) — the
5 px merging squares + shallow pool + meniscus line make it read full. Spray is
droplets, not thin sheets. All are surface/quantity trade-offs, not stability
risks.

---

## 10. BUILD ORDER (for the hardware-in-the-loop implementer)

De-risk the one true unknown first, then layer visible wins. Each step is
independently testable on-device.

1. **Wire the skeleton (no physics).** Add `pub wa: Water` to `apps::State`
   (`Water::new()`); `has_content(WATER)=true`; the `app.rs` `frame_us` arm
   (→ `ANIM_FRAME_US`); the tick call-site branch reading `read_accel` and
   calling `apps::water_tick`; `build.rs::generate_water_lut()` + the `include!`.
   Ship a stub `tick` that just clears a rect and marks it. **Verify:** Water
   opens, runs at 40 fps, clock stays topmost, no panic. (integration.md.)
2. **IMU axis-map + calibration (the de-risk).** `println!` the raw triple; run
   the §6 recipe (flat = in-plane 0; tilt each edge). Bake `IMU_*` consts +
   confirm the ±0.1 g rest-bias capture. **Verify:** the mapped gravity vector
   points the right way for all four tilts; flat reads ~0.
3. **Particle core.** `seed` + integrate + `VMAX` clamp + damp + hard wall
   projection; render flat 5 px squares (single LUT colour). **Verify:** it
   pools at the bottom, runs downhill and climbs the far wall on tilt, never
   leaks the rim, never explodes.
4. **Incompressibility.** `build_hash` + `relax` (PUSH_LUT repulsion, candidate
   cap). **Verify:** the mass spreads and *levels* like liquid instead of
   heaping; measure a frame — relax stays ~5 ms.
5. **Surface + viscosity.** Add the viscosity term and the hysteresis surface
   flag. **Verify:** the free surface calms (boil killed), no meniscus flicker.
6. **Slosh + spray.** Jerk→whole-body slosh + surface-lift kick. **Verify:** a
   flick throws a spray of squares that arc and re-absorb; a hard shake never
   blows up (VMAX holds).
7. **The look.** Full `WATER_LUT` shading (speed↑/depth↓), aurora drift, Bayer
   index dither, per-particle sparkle, the continuous meniscus line. **Verify:**
   premium neon-blue sheet, glassy tilting waterline, luminous drift.
8. **Damage/flush + polish.** Confirm the shrinking-bbox partial flush when
   pooled and full-frame under slosh (log `flush_dirty`'s P/F + ms). Add the
   `draw_reveal` fill-in on open and the breathing-scaled-by-|g| rest ripple.
   **Verify:** pooled frames partial-flush (higher fps), sloshed frames full;
   the pool fills in on open and shimmers at rest.
9. **Tune (const edits only).** `STIFF`/`H_PX` (rest spacing), `GRAV_SHR` (tilt
   speed), `DAMP` (settle time), `VISC_K` (surface calm), `JERK_TH`/`SPRAY_SHR`
   (spray feel), `BREATHE_*` (rest life). If leveling looks soft, enable a 2nd
   relax pass; if a frame overruns 25 ms under violent shake, drop `NW` to 400.
