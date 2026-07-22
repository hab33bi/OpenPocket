# Water — Judge report: NUMERICAL RIGOR (overflow / precision / stability)

Lens: audit each design's fixed-point scheme for **i32 overflow**, **precision
collapse**, and **stability** (blow-ups, wall leaks, singularity piling,
jitter). Reward proofs that are real and stability that is guaranteed *by
construction*; penalize hand-waving. `i32::MAX = 2_147_483_647`.

Signatures were re-verified against the repo before scoring:
`read_accel -> Result<(i16,i16,i16),()>`, `ACC_LSB_PER_G = 8192`,
`isqrt(u32)->u32` (Newton, `isqrt(0)=0` guard), `lut_sin_cos_q14 -> Q14`
(±16384), `NOISE_A/NOISE_B: pub static [u8;256]` with values in ≈[40,215]
(a periodic-noise range, **not** 0..255 — a shading nit for the aurora math,
not a stability issue), `set_px` performs ~6 integer divides per pixel.

---

## Verdict / ranking (this lens only)

| Rank | Design | Score | One-line reason |
|---|---|---:|---|
| 1 | **heightfield** | 92 | The only *provably* stable scheme (CFL≤1 upwind) with **exact** mass conservation; overflow proof ties the load-bearing caps to the stability caps; least jitter. |
| 2 | **falling-sand** | 86 | Three failure modes are **structurally impossible** (can't blow up / leak / pile), smallest overflow surface; dinged for CA surface-boil (jitter) and a *faked* momentum heuristic. |
| 3 | **pairwise-hash** | 80 | Best overflow headroom (i32 Q8 positions, worst product 2% of i32), blow-up bounded by construction incl. the critical `d→0` PUSH_LUT clamp; incompressibility only *bounded*, boils. |
| 4 | **flip-lite** | 74 | Most *sophisticated correct* stability arguments (staggered-MAC no-checkerboard, 5% PIC kills FLIP ringing, per-iter pressure clamp); dinged for under-converged solve, tightest margin, internal shift/const inconsistencies + one admitted-broken code line. |
| 5 | **pic-grid** | 70 | Clean blow-up/leak by construction (VMAX keystone + hard positional wall clamp); dinged for coarsest Q4 precision, steppy integer-density velocity-kick (most jitter of the particle designs), and an **undetected `gx as i16` overflow edge** in a proof that claims completeness. |

**Single best on this lens: `heightfield`.** Caveat stated plainly below: it wins
*numerical rigor* partly because it solves an easier (1-D) problem; its
one-slosh-axis limitation is a *modeling/fidelity* cost that other lenses must
weigh. The synthesis should transplant heightfield's rigor techniques into
whatever model it ultimately picks.

---

## 1. heightfield — score 92 (WINNER on this lens)

**Fixed-point.** depth `d` Q8/i32, flux `u` Q8/i32 clamped `±U_MAX=1024`
(4 px/frame), surface `ys` Q8/i32. All hot state i32 with large headroom.

**Overflow proof — real and self-aware.** 9 multiplies enumerated; the binding
one is correctly identified:

- #5 upwind flux `F = (u·d_up)>>8`, worst `u=1024`, `d_up ≤ cap<<8 = 452·256 =
  115_712` → `1024·115_712 = 118_489_088` (5.5% of i32, ~18× margin). Verified.
- Continuity `d += (F[i-1]−F[i])>>2`: intermediate `(F[i-1]−F[i]) ≤ 2.37e8`,
  `>>2 = 5.9e7`, added to `d ≤ 1.16e5`, then clamped to `[0, cap<<8]`. No i32
  overflow mid-add. Verified.

The design explicitly states the two caps (`U_MAX`, `d ≤ cap`) are **load-bearing
for the overflow proof *and* the stability guarantee — one mechanism, two
payoffs.** That is exactly the "stability guaranteed by construction" the lens
rewards, and it is the only design that names the coupling.

**Stability — the strongest, and it is *provable*, not merely bounded:**

1. `u` clamped `±U_MAX` gives **CFL = U_MAX·dt/dx = 4/4 = 1.0** → first-order
   upwind advection is a monotone, unconditionally stable scheme at CFL≤1. This
   is a textbook convergence guarantee, not a clamp-and-hope.
2. `d` clamped `[0, cap]` → no negative depth, no depth past the physical chord.
   The **round wall *is* the clamp**, and it is simultaneously the
   anti-singularity guarantee (no cell can pile beyond its chord height).
3. `DAMP < 256` every frame → energy strictly decreases absent forcing → always
   settles. Breathing is a bounded ±1 px forcing, swamped by damping.
4. **Closed flux topology + reflecting edge faces** → water only ever moves
   between two valid interior faces; edges are forced to reflect. The disc is a
   sealed vessel → *cannot leak*, and mass is **conserved exactly by the flux
   form** (up to clamp rounding, which the additive volume-nudge corrects).

I checked the one hidden stability risk a novice would miss: the leveling term
`K_LEVEL·(ys[i+1]−ys[i])` is a surface-gravity-wave (centered-difference) term
that could in principle ring if mistuned. It cannot diverge here because the
`u`-clamp is a **universal CFL backstop** on that wave too (transport is capped
at 1 cell/frame regardless of how large the leveling accel gets), and `DAMP`
bleeds it. So even under const mistuning it degrades to bounded ringing, never
divergence. Staggered (face-velocity / centre-depth) upwind has no
checkerboard/odd-even null space. This is the cleanest stability object of the
five.

**Precision.** Q8 depth/flux; leveling produces `≥40 Q8` flux accel per 1 px
surface tilt — well above rounding, no precision collapse. Least jitter of all
five (a smooth deterministic field; no grid-quantized density noise, no CA boil,
no explicit-spring chatter).

**Honest weaknesses (fidelity, not numerics).** One slosh axis (screen-x roll);
pitch only stiffens; single-valued surface cannot go past vertical. These are
*modeling* limitations the design states up front — they do **not** cost it on
the numerical lens, but the synthesis must weigh them under other lenses.

**Minor nit:** `R_WALL=226` sits right at the interior wall with no 2 px margin
for the 3 px render cells, so a crest can bleep 1–2 px toward the bezel (223).
Cosmetic, not a leak.

---

## 2. falling-sand — score 86

**Fixed-point.** The bulk CA is cell-indexed with **no fractional position** —
the whole point. Fixed-point is confined to the Q8 gravity/momentum control
vector and the Q8 droplets. This gives the **smallest overflow surface of any
design**: there is almost no per-cell arithmetic to overflow.

**Overflow proof — correct, small.** Largest intermediate ≈ `1.05e7`
(gravity-from-raw `(raw−rest)·256`), verified; droplet `isqrt(d2<<8)` with
`d2≤108_578` → `27.8e6 < u32::MAX`, verified; wall test `226²·2 = 102_152`
precomputed at seed (not hot), verified. Nothing needs i64. The design also makes
the sharpest *perf-correctness* observation of the five: `set_px` does ~6
`/255` divides/pixel (I confirmed), so a divide-free per-pixel path
(pre-packed LUT + **index-add** dither) is mandatory — a real ~7 ms saving in a
55 k-px loop.

**Stability — three structural impossibilities (the strongest form):**

- **Never piles to a singularity** — a cell holds exactly 0 or 1 unit; over-
  density is not *representable*. This is stronger than every particle design's
  "restoring force resists compression" — it is an impossibility, not a bound.
- **Never leaks** — a move is a bounds compare against precomputed
  `wall_lo/wall_hi` per row; water cannot address an out-of-circle cell.
- **Never blows up** — bulk cells move ≤1 cell/frame; there is no bulk velocity
  to integrate to infinity. Mass is a swap-invariant (full↔empty), and spray
  removes exactly one cell / landing re-adds one → integer mass is conserved
  exactly.

**Why it ranks below heightfield.** Two real dings on *this* lens:

1. **Jitter.** The classic CA left-right waterline **boil/flicker** (their
   weakness #1). Serpentine scan + slosh-bias + energy tie-break tame it but do
   not remove it; heightfield's field has none.
2. **The "physics" is partly faked.** A positional CA carries no momentum, so
   genuine slosh/waves/spray are *not emergent* — momentum is a single global
   `s` vector biasing lateral flow. That is provably bounded (`±512`, decay
   ×0.898) but it is a heuristic animation of inertia, not a rigorous scheme; the
   RNG tie-break + serpentine + slosh-bias interplay is ad-hoc. (Also: the sketch
   never shows the `rng` seed; an unseeded xorshift stuck at 0 would bias the
   tie-break — a one-line fix, but flagged.)

Numerically extremely safe; it earns 2nd because its safety comes partly from
having little dynamical system to be unstable, and it retains a real jitter mode.

---

## 3. pairwise-hash — score 80

**Fixed-point.** Position **Q8 in i32** (the most headroom of any design),
velocity Q8 in i16 clamped `±VMAX_Q8=4608` (18 px/frame).

**Overflow proof — the most headroom, correct including the tricky shift.** 12
multiplies; largest is render `spd2 = vx²+vy² = 4608²·2 = 42_467_328` (2% of
i32), verified. Crucially they **correctly account for the `<<8` in the wall
projection** `((dx·WALL_R)<<8)/r = 13_639_680` (0.6%) — I re-derived the
projected coordinate lands at ≤455 px, inside, no overflow. The `PUSH_LUT[d2]·
dxpx>>7` pair impulse is `≤131_072`, summed over ≤20 neighbours in i32 then
re-clamped. All verified. This is the roomiest scheme; overflow is a non-issue.

**Stability — blow-up bounded by construction, leak impossible:**

- `v` hard-clamped `±VMAX_Q8` every frame (a CFL ≈1-cell bound); `DAMP=250/256`
  bleeds energy.
- **`PUSH_LUT` is build-time clamped to 8192 *and* clamped for `d→0`** — the
  single best "bounded force by construction" idea in the field: overlapping
  particles get a strong but *bounded* push, so no singular force exists, and the
  hot pair loop is sqrt-free and divide-free (the 1/d normalisation is baked into
  the table). This is the anti-singularity guarantee.
- Wall is a **positional projection to exactly r=WALL_R** for every OOB particle
  (not a thin collider); `VMAX_PX=18 < WALL_R` margin → no tunnelling; nothing is
  outside the circle at frame end.
- Hash rebuilt every frame before relax; `cell = H_PX` so all in-radius
  neighbours are inside the 3×3 block by construction; `cell_of` clamps indices
  to `[0,W-1]` → no OOB array access even for a briefly-escaped particle.

**Why it ranks below falling-sand.** Incompressibility is an **explicit
short-range repulsion**, not a hard constraint: it is *bounded* (good) but only
*approximately* incompressible — one pass under-relaxes, the pool is slightly
compressible under strong gravity, and the surface **boils** (their weaknesses
#1–3). So it shares the jitter mode without falling-sand's structural
impossibilities. The half-impulse-per-pair explicit spring is soft enough
(≈0.25 px/frame per pair) that it does not oscillate violently, and `DAMP`
+ `VMAX` backstop it — bounded, but a bounded dynamical system rather than an
impossibility. Solid, roomy, honest; loses on the "impossible vs bounded"
distinction and the residual boil.

---

## 4. flip-lite — score 74

**Fixed-point.** Position/velocity Q6 i16 (`V_MAX=1536`=24 px/frame), grid
momentum Q12/i32, grid mass Q6/i32, pressure/div i16 clamped `±30000`.

**Overflow proof — the most exhaustive (14 items), with one deliberate smart
reduction.** They compute the radius test in **whole px, not Q6**, precisely to
keep `d2 = dxp²+dyp² ≤ 131_072` small (noting Q6 would be `5.4e8` — still fits
but wasteful). That is exactly the right instinct: shrink the overflow surface on
purpose. P2G momentum `mu += w·vx` worst-cases at `640·98_304 = 6.3e7` even if
*all* 640 particles land on one node (34× margin) — verified safe.

**Stability — the most *physically correct* and the most *sophisticated
arguments*:**

- A **real Jacobi pressure projection** → divergence-free grid velocity →
  genuine incompressibility (the pool spreads and levels, doesn't pile). This is
  a stronger *model* of incompressibility than pairwise/pic explicit repulsion.
- **Staggered MAC grid has no checkerboard null-space** → pressure cannot grow an
  odd/even mode. This is a real, advanced numerical-stability insight no other
  design demonstrates.
- **5% PIC in the FLIP blend + `DAMP=254/256`** continuously remove the
  high-frequency energy that makes pure FLIP ring and explode — the standard,
  correct FLIP stabiliser.
- Pressure **clamped `±30000` each iteration** → the solve cannot diverge even
  fed pathological divergence. Two independent leak barriers (grid solid-face
  Neumann wall + per-particle rim projection).

**Why it ranks below the top three despite the best *arguments*.** Rigor of
*execution* is where it loses, and this lens penalises exactly that:

1. **Under-converged solve.** 24 Jacobi iterations on a 24-wide grid is ≈one
   traversal; Jacobi needs O(N²) sweeps for full convergence. A vertical column
   (watch upright) is under-solved → *slightly compressible* → the far-wall climb
   is soft. Not a divergence (pressure clamped, PIC damps) but the
   incompressibility guarantee is *approximate*, and the design admits it.
2. **Internal inconsistency.** The reflect shift is `>>16` in the §1 proof but
   `>>14` in the §8 code, and `REST` is "≤512 Q8" in the proof but `=90` in the
   code — one of the two stated versions is wrong. The proof is conservative so
   still *safe* (worst product `4.0e8`, ~5× margin — the **tightest of the safe
   designs**), but a proof that disagrees with its own code is a rigor ding.
3. **An admitted-broken sketch line.** The grid-velocity divide (line 542) is
   garbled and annotated "(real code: gu = momentum/mass; shown compactly)" —
   concrete hand-waving in the one place the arithmetic matters.
4. **Largest surface for a subtle bug** — P2G/solve/G2P/FLIP-blend is the most
   moving parts of any design.

Most rigorous in *intent*; least clean in *execution* among the serious
contenders.

---

## 5. pic-grid — score 70

**Fixed-point.** Position & velocity **Q4 in i16** (the coarsest precision of the
field), `VMAX=1024`=64 px/frame.

**Overflow proof — thorough (12 rows) and it catches a real subtlety.** Largest
product is the reflect `2·vn·nrx ≈ 1.05e6` (~2000× inside i32 — the roomiest
*relative* margins, because Q4 keeps operands small). It explicitly notes row 8's
jerk operand (`65536`) exceeds i16 so jerk must be computed in i32 — a genuine
catch.

**Stability — blow-up and leak are airtight by construction:**

- **The `VMAX` clamp applied *after* damping every frame is the keystone**:
  kinetic energy is strictly bounded and net-dissipative; no accel term
  (gravity ≤6, pressure ≤240, bounded jerk) survives past one frame.
- **Hard positional wall clamp `nx = (CX+dx·WALL/d)<<4` is independent of the
  velocity reflect** — a particle is repositioned inside every frame it crosses,
  so *a leak is impossible even if the reflect math were disabled*. Decoupling
  containment (position) from energy (velocity) is the right instinct.
  `d ≥ WALL = 224 > 0` so the `/d` never divides by zero. Verified.

**Why it ranks last on this lens.** Three concrete dings:

1. **An undetected overflow edge in a proof that claims completeness.** The code
   stores `st.wa_px = gx as i16` where `gx = IMU_XSGN * raw`. With `IMU_XSGN=-1`
   and a pegged `raw=-32768`, `gx = +32768`, which **does not fit i16** and wraps
   to `-32768`. The §1.2 table asserts every hot value is bounded, but this
   mapped-raw store is a real (if rare, ±4g-pegged) hole. `pairwise-hash` and
   `flip-lite` avoid it by keeping the mapped/jerk value in i32; the synthesis
   must too.
2. **Weakest incompressibility of the "solve" designs.** Pressure is a velocity
   kick from a **steppy integer density gradient** (`over(c)=dens−REST`, a cell
   holding only ~3–5 particles at rest), single Jacobi pass. It is *bounded*
   (clamped, damped) but the coarse integer gradient makes the surface shimmer at
   the ~20 px cell scale and can transiently over-compress under sustained tilt
   (their weaknesses #1–2). Most jitter of the particle designs.
3. **Coarsest precision.** Q4 = 1/16 px; the rest-breathing term
   `((btri−128)·BREATHE)>>8` bottoms out at ≈1 Q4 = 1/16 px/frame — it works, but
   there is little precision margin below it. Q8 (pairwise) or even Q6 (flip)
   would carry slow motion more smoothly.

Airtight where it counts (no blow-up, no leak) but the coarse Q4 + steppy
integer-density pressure + the missed sign-flip edge make it the least clean
numerical object.

---

## Cross-cutting findings the synthesis MUST carry

Ordered by how much they buy on overflow-safety / stability-by-construction:

1. **Couple the overflow bound to the stability bound (heightfield).** Pick the
   velocity/flux clamp at the CFL limit (≤1 cell/frame) and state explicitly that
   the *same* clamp bounds the worst-case multiply. Whichever cap makes the proof
   pass must be the cap that makes the sim stable — name the load-bearing clamps.

2. **Prefer *structural impossibility* over *bounded restoring force* wherever
   affordable (falling-sand):** a hard per-cell occupancy/density ceiling makes
   "pile to a singularity" un-representable, not merely resisted. Even a particle
   scheme can hard-cap deposits per cell. And a **precomputed per-row
   `wall_lo/wall_hi` mask** is a leak-proof, divide-free containment (bounds
   compare) that is cheaper and more certain than per-particle projection.

3. **`PUSH_LUT` with the `d→0` clamp (pairwise-hash):** bake `1/d` + falloff into
   a build-time table clamped for `d→0`, so any repulsion is *bounded by
   construction* (anti-singularity) and the hot loop is sqrt-free and
   divide-free. If the synthesis keeps particles, this is the repulsion to use.
   Also: **Q8 positions in i32** for maximal overflow headroom.

4. **If any grid/pressure solve is used (flip-lite):** staggered MAC (no
   checkerboard), **5% PIC in the FLIP blend** to kill FLIP ringing, and a
   **per-iteration pressure clamp** so the solve can't diverge. And do the
   **radius test in whole px, not the fine fixed-point**, to shrink `dx²+dy²`.

5. **Exact-conservation hygiene (heightfield):** a cheap **volume-conservation
   nudge** that redistributes accumulated clamp-rounding drift, so the pool never
   slowly drains or floods from wall-clamp rounding over minutes of runtime.

6. **Decouple containment from energy (pic-grid):** a **hard positional wall
   clamp independent of the velocity reflect**, so a leak is impossible even if
   the reflect is disabled. Apply the **velocity clamp *after* damping** every
   frame (the keystone).

7. **Fix the mapped-raw / jerk i16 hole (pic-grid's bug, avoided by
   pairwise/flip):** never store a sign-flipped or differenced *raw* IMU value in
   an i16 (`-1·-32768` overflows). Keep mapped gravity and jerk in i32.

8. **Divide-free per-pixel render (falling-sand):** `set_px` costs ~6 `/255`
   divides/pixel; use a pre-packed RGB565-BE LUT with **index-add Bayer
   dithering** — a real multi-ms saving that also removes a per-pixel arithmetic
   surface.

**Winner on this lens: heightfield** — the only design whose stability is a
theorem (CFL) rather than a clamp, with exact mass conservation and the least
jitter, and whose overflow proof explicitly couples its bounds to its stability.
The synthesis should adopt heightfield's rigor discipline (items 1, 5) and
falling-sand's structural impossibilities (item 2) even if it chooses a 2-D
particle model (for which items 3, 6, 7 make the particle path as close to
provably-stable as integers allow).
