# Water — adversarial review: PHYSICS & STABILITY

Lens: will it blow up, leak through the round wall, pile to a singularity,
strobe, or fail to level? Focus on boundary reflection, the
incompressibility/repulsion pass, damping, and the jerk impulse.

Reviewed: `docs/water/IMPL-SPEC.md`, `docs/water/water_draft.rs`,
`docs/water/integration.md` (the `PUSH_LUT`/`WATER_LUT` generator).

**Verdict: fix-then-ship.** No unbounded blow-up and no hard wall leak — the
`VMAX` clamp after damping and the hard positional wall projection are sound and
genuinely make numeric explosion and tunnelling unrepresentable. But the
**repulsion kernel is non-monotonic**: the `8192` clamp on `PUSH_LUT` inverts the
pair impulse below ~2.6 px separation, creating a collapse/co-location zone that
directly defeats the "never piles to a singularity / levels like liquid"
guarantee and seeds surface boil/strobe. That is a real defect reachable under
exactly the stress cases this lens cares about (deep pool bottom, hard tilt into
the wall, slosh). Two lesser issues below.

---

## Finding 1 (HIGH) — Non-monotonic repulsion: the `PUSH_LUT` clamp inverts the force below ~2.6 px, so compressed particles collapse to contact / co-location

**Location:** `integration.md` `generate_water_lut()` — `(v as i32).clamp(0, 8192)`
on `PUSH_LUT` (STIFF=1600, HP=16); consumed in `water_draft.rs` `relax()`
lines 472–474 (`push = PUSH_LUT[d2]; ax += (push*dx) >> PUSH_SHIFT`).

**The mechanism.** `PUSH_LUT[d2] = clamp(STIFF*(H−d)/d, 0, 8192)`, and the pair
impulse is `(PUSH_LUT[d2]·dx) >> 7`. The `1/d` in the LUT is meant to normalise
`dx` to a unit direction so the impulse comes out `∝ (H−d)` — monotonic,
strongest at contact. It does exactly that **only while the LUT is unclamped.**
The clamp bites when `STIFF*(H−d)/d > 8192` ⇒ `d < 2.61` (i.e. `d2 ≤ 6`). In
that region `PUSH_LUT` is pinned at 8192, so the impulse degenerates to
`(8192·dx) >> 7 = 64·dx`, which **grows with dx** — i.e. the separating impulse
now *decreases* as the pair gets closer.

Head-on pair impulse magnitude (before the `ax>>1` halving), taking `dx≈d`:

| d (px) | 1 | 2 | 2.45 | **2.65** | 3 | 4 | 8 | 12 | 16 |
|---|---|---|---|---|---|---|---|---|---|
| impulse (Q6) | 64 | 128 | 156 | **166 (peak)** | 162 | 150 | 100 | 49 | 0 |

The force rises to a peak at `d≈2.6` then **falls back toward 0 as d→0.** Below
2.6 px the kernel is an inverted spring (negative stiffness): the closer two
particles get, the *less* they repel.

**Why it triggers (concrete scenario).** Gravity at 1 g is `8192>>6 = 128` Q6 of
downward velocity **per frame** (`GRAV_SHR=6`). The peak *per-pair* separating
impulse after halving is `166/2 ≈ 83` Q6/frame — already **less than one frame of
1 g gravity.** A particle in the pool bulk is loaded by the cumulative weight of
the column above it (several layers × 128 Q6). Once that load pushes a pair
inside 2.6 px, each further px of approach *weakens* the repulsion, so gravity
wins and drives the pair toward `d=1` (impulse only `64/2 = 32` Q6/frame ≈
0.5 px/frame) and then to `d=0`. At `d2==0` the pass does `continue`
(`water_draft.rs:467`) → **zero** mutual repulsion → the two particles co-locate
on one integer pixel and never separate. Under a hard sustained tilt or a shake
(velocities to 16 px/frame), pairs are routinely forced through the 2.6 px zone,
so co-located clumps accumulate at the pool bottom and against the far wall.

**Consequences (all on this lens):**
- *Piles to a singularity* — the headline guarantee "never piles to a
  singularity" fails: N particles stack on one pixel, unrecoverable because the
  separation metric is whole-pixel (`dx=(px_i>>6)−(px_j>>6)`) and `d2==0` is
  skipped.
- *Fails to level* — the bottom layer settles at the neutral-stiffness peak
  (`∂I/∂d≈0` at d≈2.6) or collapses past it, so it transmits lateral pressure
  poorly; the pool packs into a dense clod instead of a level puddle.
- *Strobe/boil* — near the peak the restoring stiffness is ~0 and just inside it
  is negative, so compressed particles jitter frame-to-frame (the "boil" the
  spec hand-waves is caused here, not merely a surface aesthetic).

**Fix.** The `8192` clamp is both the cause **and** unnecessary. For every
`d2 ≥ 1`, `STIFF*(H−d)/d ≤ 1600*15/1 = 24000 < i16::MAX (32767)`, so the LUT fits
`i16` with no clamp, and the resulting impulse is then bounded and monotonic:
`12.5*(16−d)`, max `≈187` Q6 at `d=1`, `0` at `d=16`.
1. Raise the ceiling to `≥24000` (or drop the clamp; keep the `d<1 ⇒ STIFF*(H−1)`
   guard so `d2=0` still maps to a finite value even though it is skipped). This
   alone restores strong-at-contact monotonic repulsion — at `d=1` the impulse
   jumps from the buggy 64 back to 187, so pairs self-separate before reaching
   `d=0`.
2. Give gravity headroom: peak per-pair impulse after halving (`~93` with the
   fix) is still below the 128 Q6/frame of 1 g, so a heavily loaded interface can
   still be driven to contact. Either raise `STIFF` (note `PUSH_LUT[1]=15·STIFF`
   must fit its storage — 1600 is safe, `>2184` overflows `i16`, so widen
   `PUSH_LUT` to `i32` if you push stiffness up) **or** soften gravity
   (`GRAV_SHR=7` → 64 Q6/frame at 1 g) so the per-pair repulsion comfortably
   exceeds the per-particle gravity load. Retune `VMAX` if you change `GRAV_SHR`.
3. Belt-and-suspenders: when `d2==0`, instead of `continue`, apply a fixed
   deterministic separation nudge (e.g. push along `(i-j)` parity) so exact
   co-locations can't be a permanent absorbing state. Secondary once (1)+(2)
   land.

---

## Finding 2 (MEDIUM) — Spurious whole-body slosh + spray on the first live tick (opening the app while tilted)

**Location:** `water_draft.rs` `new()` lines 216–218 (`last_ax/ay/az = 0`),
`calibrate()` lines 269–278, jerk block lines 321–337.

**Mechanism.** Jerk is `jx = rmx − lrmx`, where `lrmx` is derived from
`self.last_a*`, which `new()` initialises to `(0,0,0)`. On the **first** tick the
jerk is therefore `rmx − 0 = rmx` — the *full mapped in-plane acceleration*, not a
real frame-to-frame delta. Meanwhile the calibration tick runs first
(`need_calib` → `calibrate()`), which sets `ever_live = true` on that same tick,
so the spray gate `self.ever_live && jmag > JERK_TH` is already armed.

**Scenario.** A user raises their wrist and opens Water while the watch face is
tilted toward them (a normal 20–40° viewing angle). In-plane gravity at 30° is
`sin(30°)*8192 ≈ 4096` LSB per tilted axis, so `jmag ≈ 4096 > JERK_TH = 2600`
(the threshold is only ~18° of tilt). The gate fires on frame 1: `sx/sy` inject a
whole-body slosh and every surface particle gets a `vy -= jmag>>8` up-kick — the
freshly-seeded pool visibly jumps/sprays the instant it appears. It is bounded
(everything is `VMAX`-clamped, no blow-up), but it is a wrong, repeatable startup
transient on a common gesture.

**Fix.** Seed `last_ax/ay/az` from the first live sample so the first jerk is
zero: set them in `calibrate()` (or `open()`), e.g.
`self.last_ax = acc.0; self.last_ay = acc.1; self.last_az = acc.2;` when `live`,
and/or suppress the jerk/spray path on the calibration tick
(`if self.need_calib_was_set_this_tick { skip jerk }`).

---

## Finding 3 (LOW) — Candidate cap is filled in a spatially biased scan order, so under compression it skips the nearest neighbours and biases the net push

**Location:** `water_draft.rs` `relax()` lines 456–484 (`K_CAND=24`, the
`'scan` loop counts only in-radius pairs and `break`s at the cap).

**Mechanism.** The 3×3 block is scanned `gy` outer (`cy−1..=cy+1`), `gx` inner —
so the **centre cell** (which holds the closest, highest-repulsion neighbours) is
the 5th of 9 cells visited, and `count` increments on every in-radius pair. If
the earlier (top/left) cells already contain ≥24 in-radius neighbours, the loop
`break`s **before** ever processing the centre cell. Two effects: (a) the
strongest, nearest repulsions are dropped exactly where crowding is worst, and
(b) the retained subset is directionally biased (top-left heavy) → a small
systematic net push toward bottom-right, i.e. a slow drift rather than a
symmetric pressure.

**Why reachable.** At a pooled rest spacing of ~5.5 px there are already
`π·16²/5.5² ≈ 27` neighbours inside `H_PX`, over the cap of 24; any compression
(Finding 1, or a hard tilt packing the wall) pushes local counts well past 24, so
the cap bites in steady state, not just transiently.

**Fix.** Visit the centre cell first (nearest-first ordering), or accumulate all
pairs then keep the K strongest, or simply raise `K_CAND` / verify the pool never
sustains >24 in-radius once Finding 1 is fixed (a properly spread pool sits at
~18 neighbours, under the cap). Cheapest correct change: reorder the 3×3 walk to
centre-out.

---

## Checked and OK (no defect on this lens)

- **Blow-up:** every velocity is hard-clamped to `±VMAX` after integrate, after
  relax, and after damping (`DAMP=254/256<1` is strictly dissipative). Kinetic
  energy is L∞-bounded each frame; sustained shake saturates at `VMAX`, it does
  not diverge. No path produces NaN/Inf/overflow (all hot multiplies verified
  `< 3.4 M`, matching the spec's proof).
- **Wall leak / tunnelling:** the boundary is a **hard positional projection**
  onto `r=WALL_R` every frame, independent of the reflect, so a particle cannot
  end a frame outside the disc. Max one-frame displacement is `VMAX=16 px ≪ WALL_R
  interior margin`, and `r ≥ WALL_R = 222 > 0` guarantees no divide-by-zero in the
  projection/reflect. (Minor cosmetic note, not a leak: `dx,dy` are whole-pixel
  and `isqrt` floors, so the projected radius can land ~1 px past `WALL_R`; with
  the 5 px square this can bleed 1–2 px onto `BEZEL_R=223`. Tighten `WALL_R` by
  ~2 px if the bezel edge shows. No particle escapes.)
- **Reflect energy:** restitution `(256+REST_E)/256 ≈ 1.10` correctly removes the
  outward normal and adds a small inward component (`v' = v − (1+e)·vn·n`), only
  when `vn>0`. The post-reflect velocity is briefly un-clamped (can exceed `VMAX`)
  but is re-clamped in the next frame's integrate before it moves any position, so
  it cannot cause tunnelling; magnitude stays well inside `i16`.
- **Hash/relax consistency:** `build_hash` runs on the post-integrate positions
  and `relax` reads the intact `cell_start` (only `cursor` was mutated); positions
  are not modified during relax, so neighbour separations are consistent. The
  `clamp(0,GRID_W-1)` in both `cell_of` and `relax` keeps the (rare) out-of-wall
  post-integrate positions in-range and in agreement.
- **Dead-IMU / bus-fault fallback:** `imu=None` reuses the last sample; a
  never-live IMU falls back to `(0, DOWN_G)` so the pool still settles. Jerk uses
  the reused sample, so a dropped read injects no spurious impulse (jerk ≈ 0).
