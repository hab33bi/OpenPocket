# Judge — Water liquid designs through the FEASIBILITY & FIT lens

Lens (only this): does the design *truly* hit ~10 ms compute at 40 fps on
Xtensa integer math at its stated count; is the SRAM budget honest; does it
integrate into `apps::State` / `apps::tick` / the run-loop IMU read / `build.rs`
cleanly and minimally? Reward buildable, well-fitted, honestly-budgeted work.
NOT judged here: how convincing the liquid looks, 1-axis vs 2-axis slosh,
past-vertical behaviour — those are simulation-quality axes, a different lens.

Grounding facts I re-verified in the repo before scoring (so nothing below is
taken on the designs' word):

- `flush_dirty` (`src/app.rs:1001`): `partial = !dmi.overflowed() && dirty_bytes
  < byte_count*3/4`, where `dirty_bytes = Σ (x1-x0+1)*2` over spans. So marking
  **one** union bbox → partial flush when the pool is compact (~452×190 →
  ~172 KB < 326 KB) and full when it sheets across the disc. The adaptive-flush
  story every design leans on is REAL, and it also means the whole app hinges on
  a *correctly shrinking* damage bbox.
- `FRAME_US=50_000` (20 fps), `ANIM_FRAME_US=25_000` (40 fps) (`app.rs:33,41`).
  Pinning Water needs one match arm → `ANIM_FRAME_US`. `Scene` derives
  `PartialEq` (`app.rs:81`), so both `scene == Scene::App(apps::WATER)` and
  `matches!(...)` compile.
- `app_state = apps::State::new()` is a **stack local of `run(mut self) -> !`**
  (`app.rs:175`); `State::new()` is `const fn` (`apps.rs:66`). Inline arrays in
  `State` therefore grow the run-loop stack frame and must be const-initializable.
  This is the crux of the SRAM-placement scoring.
- `crate::trig::lut_sin_cos_q14` + `TAU_Q14` exist (`src/trig.rs:10,18`);
  `ACC_LSB_PER_G=8192`, `read_accel -> Result<(i16,i16,i16),()>`
  (`qmi8658.rs:25,48`); `open_app` with a TIME reset (`app.rs:1568,1583`);
  `lock::NOISE_A/NOISE_B`, the bottom-of-`apps.rs` helpers, and the
  `generate_wheel_assets` RGB565-BE emit idiom are all as the designs describe.
- All five bake `WATER_LUT` as pre-packed RGB565-BE and stamp raw bytes per
  pixel (no per-pixel `/255`). This is correct and load-bearing (see Adopt #1),
  but it is a *shared* decision, not a differentiator.

---

## Ranking (feasibility & fit only)

### 1. heightfield — 90/100 — WINNER on this lens

The best technical fit to *this* codebase and the most honestly-budgeted by a
wide margin. Physics is **116 columns + 64 droplets ≈ 3.7 k int ops ⇒ <0.2 ms**
— not "fits the budget" but "uses ~2 % of it." Render is deliberately
cell-granular: one `WATER_LUT` index per 3×3 cell, raw-byte `fill_cell`, index
dither = one add → ~5 000 worst-case cells ≈ 4–6 ms incl. the tracked-bbox
clear. Frame = 0.2 + ~5 + 13 flush ≈ **18 ms**, the *largest* stability margin
of any design.

- **SRAM: 2.05 KiB, honest and best-fitting.** All arrays are `[0i32;116]` /
  `[0i16;116]` / `[0u8;64]` — const-initializable, a clean `pub water: WaterSim`
  field in `State`, squarely inside the brief's "a few KB" and safe on the run
  stack. It is the ONLY design whose data placement needs zero apology.
- **Integration is minimal and buildable.** One `frame_us` arm, one
  `else if idx==apps::WATER` tick branch reading the IMU in the loop, one
  `use`, plus one `open_app` reset line (an extra site vs some, but a valid one —
  `open_app`/TIME reset verified at `app.rs:1568/1583`). `build.rs` generator
  mirrors the emit idiom exactly. The `trig` breathing call is grounded in a real
  API, not invented. The code sketch is internally coherent and compiles in the
  head.
- **Incompressible + mass-conserving by construction** (leveling term + `d ∈
  [0,cap]` clamp), so "never blows up / leaks / piles to a singularity" is a
  *structural* guarantee, not a tuning outcome — the cheapest possible route to
  the brief's hard stability constraints. Overflow proof is clean; the worst
  multiply (upwind flux `u·d_up`, 118 M) sits at 5.5 % of i32 with the two caps
  (`U_MAX`, `d≤cap`) doing double duty as CFL-stability + overflow guard.

Honest deductions on THIS lens: the extra `open_app` edit is one more touch
point than pic-grid's reveal-only seeding; and stamping `draw_status` every
frame costs a fixed ~0.4 ms (cheap insurance, fine). Its real weaknesses
(1 slosh axis, no past-vertical) are simulation-quality, explicitly out of scope
here — and its authors are the most forthright about them.

### 2. pic-grid — 80/100

The plan's recommended PIC-lite, and the design that best matches repo *doctrine*
(SET-over-black idempotent writes, direct reuse of `set_px`/`max_px`/`soft_dot`/
`fill_rect_black`/`isqrt`/`pseudo_dir`). **4.2 KiB SoA fields inline in `State`**
— honest, const-init, safe on the run stack, honors "fields in State" literally.
Compute ~3.5 ms (clear 384×7×7 ≈ 0.9 ms, draw 384×7×7 ≈ 1.6 ms, scatter + 1
Jacobi pressure pass <0.5 ms) leaves ~8 ms slack. Overflow proof is the most
rigorous of the five (12 sites tabulated; worst product ~1.05 M, 2000× inside
i32). Buildable, coherent, minimal app.rs diff via a `water_feed` helper that
keeps the shared `tick` signature untouched.

Concrete deductions:
- **The partial-flush "head-room lever" it advertises is defeated by its own
  bbox update.** `st.wa_bbox = union(this-frame tight, previous *stored* bbox)`
  (§8, `water_draw`) unions with the already-accumulated previous, so the bbox is
  **monotonically non-shrinking** — after the first big slosh it stays maximal
  and every subsequent frame full-flushes. Correct (over-marking is safe) but the
  advertised win never materializes. pairwise-hash/flip-lite do the right thing
  (`last_bbox = cur` tight; mark `union(cur,last)`).
- **Per-pixel clock-band branch in the hot draw loop** (`if (26..=66).contains(&y)
  && (CX-110..=CX+110).contains(&x)`) adds a compare to every one of ~19 k drawn
  pixels — a real, avoidable tax vs the clip-to-y≥70 approach.
- Scattered 384×(7×7) clears are more setup overhead than one streaming bbox
  fill, and require threading `batt` for the self-repair path.

Solidly buildable and honestly budgeted; the bbox defect is a perf-honesty ding,
not a correctness/stability one.

### 3. pairwise-hash — 74/100

Well-fitted and fully buildable: a `pub wa: Water` sub-struct honors "fields in
State", the code sketch is complete and coherent (real CSR count-sort, real wall
projection), and it contributes the single best `build.rs` idea across all five —
a **`PUSH_LUT[d2]` repulsion table** that bakes `1/d` + linear falloff so the
neighbour loop is **sqrt-free AND divide-free** (one load + two muls + two
shifts per pair). The damage bbox is done *correctly* (shrinks). Overflow proof
is thorough (worst ~42.5 M, 2 % of i32).

Deductions:
- **SRAM 8.3 KiB is borderline** against "a few KB" — the CSR machinery
  (`cell_start`+`cursor`+`order` ≈ 4.2 KB on top of particles) roughly doubles
  the state vs pic-grid for a comparable pool. Honest about it, offers the static
  escape hatch, but it's a weaker fit than heightfield/pic-grid.
- **The relax cost is the optimistic spot.** 320×~19 candidates×~14 ops ≈ 85 k
  ops claimed at "~4–6 cyc/op" → 2.5 ms. But each candidate does 4 random SRAM
  loads (`px/py/vx/vy[j]`) plus the impulse+viscosity math; realistic ~3–4 ms.
  Still inside 10 ms, but the headline "~2.5 ms" is a touch generous.

### 4. falling-sand — 70/100

The most clearly-inside-budget *compute* story and the sharpest single perf
insight in the whole set: it names explicitly that the repo's `set_px`/`blend_px`
do ~6 `/255` divides per pixel (~30 cyc each on Xtensa) and that a per-pixel
divide in a ~55 k-px loop alone costs ~7 ms — hence a fully **divide-free
per-pixel path** (index-add dither + raw `WATER_LUT[u16]` fetch/store). CA
physics is ~0.9 ms (3 400 water×50 cyc + 6 800 empty×6 cyc). Incompressible and
leak-proof *by construction* (one unit/cell; move gated by a `wall_lo/hi` bounds
compare), which nails the stability constraints trivially.

Deductions on FIT:
- **SRAM 13.8 KiB — the second-worst fit.** The `[u8;12996]` grid blows past "a
  few KB" and, as the design itself states, is too big for the run stack → must
  live behind a module `static`/box with `State` holding `Option<Water>` and a
  `get_or_insert` dance. Honest, but a real deviation from the "fields in State"
  doctrine and the most data-placement rework of the mid pack.
- **The render sketch is not buildable as written** — the surface test is a
  literal placeholder (`!self.cell_in_wall(cx-... , ..) /* up-neighbor empty */`)
  and a couple of helpers are described, not coded. Fine as a design, but lower
  code-fidelity than heightfield/pic-grid/pairwise-hash.

Excellent feasibility, middling fit; the SRAM footprint and the incomplete
sketch are what hold it below pairwise-hash on the combined lens.

### 5. flip-lite — 62/100

Algorithmically the most ambitious (a genuine staggered-MAC Jacobi projection
with FLIP feedback) and admirably honest that "the flush is the wall." It *does*
fit the budget on paper — but it is the worst fit and the least buildable of the
five, which is exactly what this lens penalizes.

- **SRAM 16.4 KiB in a `static mut WATER` + `unsafe`** — the largest footprint
  and the biggest departure from "fields in `State`" (only a 16-byte `WaterHead`
  stays in `State`). The design is upfront that an 18 KB struct on the run stack
  is an overflow risk, so the static is justified — but `static mut` + unsafe
  accessors are a code-smell the rest of this codebase avoids, and it's the most
  invasive integration.
- **Under-weights the Jacobi divide cost.** 24 iterations × ~452 cells each do
  `(Σ4·p_nbr − div)/kk` — that's ~10.8 k **integer divides/frame**; at ~30 cyc
  that's ~1.3 ms *on top of* the tabulated ops, so the claimed "~2.5–3 ms
  physics" is optimistic (realistically ~4–5 ms). Still under 11 ms, but the
  least margin of the five, and the one budget where the estimate materially
  understates Xtensa reality.
- **Lowest code fidelity.** The `grid_velocity` step in the sketch is a
  self-admitted garbled line ("real code: gu = momentum/mass; shown compactly")
  and several helpers (`scatter`, `gather`, `enforce_solid_faces`,
  `column_surface_px`) are stubs. More of the design is "trust me" than the
  others.

Ships premium at 40 fps if implemented carefully, but on *feasibility & fit* it
is the highest-risk, heaviest, least-buildable-as-written option.

---

## Scoreboard

| Design | Compute honesty (10 ms/40 fps) | SRAM honesty & State-fit | Codebase/build.rs fit + buildability | Score |
|---|---|---|---|---|
| **heightfield** | <0.2 ms phys, ~5 ms render — huge margin | 2.05 KiB, const-init, clean field-in-State | minimal diff, coherent sketch, grounded trig | **90** |
| pic-grid | ~3.5 ms, ~8 ms slack | 4.2 KiB inline, honors doctrine | reuses repo helpers best; bbox-growth defect; per-px clock branch | 80 |
| pairwise-hash | ~2.5 ms claimed (likely 3–4) | 8.3 KiB, borderline | complete sketch; **PUSH_LUT** build.rs gem; correct bbox | 74 |
| falling-sand | ~0.9 ms phys, divide-free render | 13.8 KiB, needs static/box | best perf insight; incomplete render sketch | 70 |
| flip-lite | fits but heaviest; Jacobi divides under-counted | 16.4 KiB `static mut`+unsafe | ambitious; garbled/stubbed sketch | 62 |

---

## What the synthesis MUST adopt (best ideas from ANY design)

1. **Pre-baked RGB565-BE `WATER_LUT` + raw-byte per-pixel store; dither the
   INDEX, never the channels** (all five; falling-sand argues it best). The
   repo's `set_px`/`blend_px` do ~6 `/255` divides per pixel; a per-pixel divide
   across ~50 k lit px is ~7 ms and would blow the budget by itself. One add
   (Bayer on the index) + one LUT fetch + two stores per pixel is the only
   affordable path.

2. **Correct, *shrinking* damage bbox** (pairwise-hash / flip-lite): store
   `last_bbox = THIS frame's tight bbox`, mark `union(this, last)`. This is what
   makes `flush_dirty`'s 3/4 rule deliver partial-when-pooled / full-when-sloshed
   adaptivity. Do NOT union with the accumulated previous bbox (pic-grid's bug,
   which pins the app to permanent full-flush after the first slosh).

3. **Clear only the tracked region, never memset 424 KiB** (heightfield /
   pairwise-hash): a full-frame black fill alone is ~5 ms. Use `fill_rect_black`
   row-spans over the union bbox (streaming PSRAM writes).

4. **Velocity hard-clamp (`VMAX`) every frame after damping + a hard *positional*
   wall projection onto the rim, independent of the velocity reflect** (pic-grid /
   pairwise-hash / flip-lite). One mechanism delivers three brief-mandated
   guarantees at once: no blow-up (energy bounded + dissipative), no leak
   (position is inside the circle at every frame end regardless of the reflect),
   and the i32 overflow bound (every hot multiply is proven at the clamp).

5. **First-flash IMU tunables as consts (per-screen-axis source + sign) + a
   rest-bias captured on open, clamped to ±~0.1 g** (pairwise-hash's
   `REST_BIAS_CLAMP`). The clamp removes only the sensor's zero offset and can
   never cancel a real tilt, so a flat watch reads in-plane g≈0 and *breathes*
   while tilt makes it run — physically correct and the exact ball-game recipe
   the plan cites. Plus dead-IMU fallback to a fixed down-vector.

6. **`build.rs`-generated `PUSH_LUT[d2]`** that bakes `1/d` + falloff
   (pairwise-hash) — the enabling trick that makes any pairwise/particle repulsion
   loop sqrt-free and divide-free. Keep this idiom for whatever particle layer the
   synthesis uses.

7. **Clock-topmost by clip-to-`y≥70` or an unconditional `draw_status` restamp
   after the liquid** (heightfield restamps; the clip is cheapest). Avoid
   pic-grid's per-pixel band branch inside the draw loop.

8. **The minimal, uniform `app.rs` hook** (all five converge): one `frame_us`
   arm pinning WATER to `ANIM_FRAME_US`; read `read_accel` at the tick call site
   (run loop owns `self.i2c`) and pass the raw triple into a WATER-specific tick,
   keeping `apps` I2C-free. Reuse `flush_dirty` unchanged.

9. **Bolted-on ballistic droplet pool for flick-spray** (heightfield /
   falling-sand): neither a height field nor a CA throws detached water
   emergently, and even the particle designs benefit from an explicit surface-lift
   → ballistic → re-absorb pool so "the wave breaks and throws spray" is real.

Winner on this lens: **heightfield** — cheapest and safest compute, smallest and
most honest SRAM, cleanest and most-buildable integration. The synthesis should
take heightfield's rendering/compute economy and structural stability as the
spine, then layer #4–#6 + #9 (a small clamped particle/spray layer) on top if the
product wants true 2-axis slosh and detached spray beyond what a 1-D field gives.
