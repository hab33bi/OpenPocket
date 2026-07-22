# Judge — LIQUID REALISM & PREMIUM FEEL on a tilt watch

Lens: which design produces the most *convincing water* — pooling/leveling,
running downhill, climbing the far wall, breaking into spray on a flick, a
believable meniscus, aliveness at rest. Reward visual quality and
responsiveness; penalize anything that reads as **bouncing dots**, **grainy
sand**, or a **stiff bar**.

Single winner on this lens: **`flip-lite`** (real FLIP/PIC with a genuine
pressure projection), with `pairwise-hash` a very close second. The two of them
are the only designs that combine *genuine incompressibility* with *genuine
momentum* while responding to **arbitrary 2-D tilt** — which is the heart of
"convincing tilt-driven water." The other three each surrender one of those
three pillars.

---

## The decisive axis: momentum × incompressibility × 2-D responsiveness

Convincing water is not one behavior, it is the *coupling* of several: it
pools and **levels** (incompressibility), it **runs, climbs, overshoots and
sloshes back** (momentum), and it does this **whichever way you tilt**
(2-D). Scoring each design on all three pillars at once is what separates them:

| Design | Incompressible | Momentum / slosh | 2-D tilt | Net |
|---|---|---|---|---|
| `flip-lite` | **real** (divergence-free projection) | **real** (95% FLIP feedback) | **yes** | strongest |
| `pairwise-hash` | particle-scale (16 px repulsion) | real (MPS velocity network) | **yes** | strong |
| `pic-grid` | coarse (20 px integer density) | real (particles) | **yes** | grainy |
| `heightfield` | **perfect** (mass-conserving PDE) | real **but 1 axis only** | **NO (roll only)** | stiff bar |
| `falling-sand` | **perfect** (1 cell = 1 unit) | **faked** (one global vector) | yes (8-way) | grainy sand |

`heightfield` and `falling-sand` each fail a pillar outright — a one-axis field
is the literal "stiff bar" on the pitch gesture, and a momentum-less CA is the
literal "grainy sand" — the two failure modes the lens is written to punish.

---

## 1. `flip-lite` — WINNER (88/100)

**Why it wins the lens.** It is the only design with *both* a true
divergence-free pressure solve (24 Jacobi iterations on a 24×24 staggered MAC
grid) *and* real momentum carried by a 95% FLIP / 5% PIC blend. That coupling
is exactly what makes water read as water: the projection makes the mass
**spread and level instead of heaping** (§7: "high-density cells develop high
pressure whose gradient pushes particles out; they cannot collapse to a
point"), while the FLIP feedback makes it **run downhill, climb the far wall,
overshoot, and slosh back** with real inertia rather than sliding to a stop.
None of the other four gets both.

- **Spray is the most convincing of the five — because it is *emergent*, not
  bolted on.** §3.4: on a flick, particles nearest the leading wall "exceed the
  local pack and the pressure solve can't hold them, so they arc as droplets."
  `heightfield` and `falling-sand` both spawn a separate ballistic pool (an
  admitted "bolt-on"); `flip-lite` throws real fluid that the solver physically
  ejects and then re-absorbs. That is the single most premium flick beat here.
- **Densest pool: 640 particles**, more than any other design (pairwise 320,
  pic-grid 384) — the pool reads full, and there is ~7 ms of compute headroom
  (§6) to push to ~900 or +iterations. Density is what keeps a particle liquid
  from reading as dots.
- **Correct rest behavior**: gravity is `raw − cal`, and the pool sits denser
  at the bottom "natural from gravity + incompressibility" (§4), with a
  breathing standing wave gated to only fire when flat & still (§3.5).
- **Stability is method-appropriate, not hand-waved**: staggered MAC has "no
  checkerboard null-space," pressure clamped ±30000/iter, 5% PIC + DAMP≈0.992
  kill FLIP ringing (§7). These are the textbook FLIP-stability mitigations,
  correctly named.

**Honest weaknesses that keep it at 88, not higher** — and they are all about
the *surface*, not the *dynamics*:
- **Coarse 20 px grid + low particles/cell.** 640 particles over ~452 fluid
  cells is ~1.4/cell average (only ~4/cell inside the resting pool). FLIP wants
  4–8/cell; below that, P2G/G2P transfers get noisy and the free surface can
  look sparse/flickery. §7 admits "the pool reads as a body of glowing points,
  not photoreal water" and "grainy at low density near the surface/crests."
- **The far-wall climb is *under-solved*.** §7: 24 Jacobi iterations propagate
  pressure ~24 cells, so "a fully-vertical column is under-solved and slightly
  compressible → the far-wall climb is a touch soft." That is a *direct hit* on
  one of the lens's named behaviors. Red-black Gauss-Seidel is the noted fix.
- **Highest buildability/tuning risk.** The P2G/G2P transfer code sketch is the
  most complex of the five and visibly hand-waved in one spot (the compact
  `grid_velocity` line 542 is acknowledged pseudo). FLIP is finicky to tune.
  (Buildability is not the lens, but it bears on whether the premium look
  actually ships.)

The winning bet: the surface weakness is *exactly* the thing the other four
designs solve well, so the synthesis can graft their surface treatment onto
this superior dynamic core. The dynamics — real incompressible, momentum-
carrying, emergent-spray flow in full 2-D — are the hardest part and only
`flip-lite` truly has them.

## 2. `pairwise-hash` — very close runner-up (85/100)

**The strongest *surface* of any fully-2-D design, and the most explicitly
premium particle look.** It is MPS-style: 320 particles, a spatial hash, and a
sqrt-free `PUSH_LUT` repulsion that enforces incompressibility at **16 px
particle scale** — *finer* than `flip-lite`'s or `pic-grid`'s 20 px grid. Three
ideas here are best-in-class and should survive into the final build:

- **A viscosity term** (nudge `v_i` toward neighbour `v_j`, §3.2) that
  deliberately "smooths the grainy surface." This is the single best idea in any
  design for curing particle-liquid boil, and neither FLIP nor pic-grid has it.
- **Body squares are SET so overlaps merge into "a solid dithered cyan sheet,
  i.e. *liquid*, not dots"** (§4.1). This is the only design that *names and
  designs against* the bouncing-dots failure mode. Premium and correct.
- **Per-particle surface detection via neighbour count `nbr[i]`** gives a
  precise meniscus that "sparkles where the surface is exposed or spraying"
  (soft_dot on surface particles), far finer than a grid-cell surface flag —
  with the honest note that it needs hysteresis to stop meniscus flicker (§7.4).

It also nails the **rest-bias clamp to ±0.1 g** (§3.1): a flat watch reads
in-plane g≈0 (physically correct — gravity is along z), so the pool *sits and
breathes*, and tilt *always* dominates because calibration can never cancel a
real tilt. This is the most correct "runs on tilt / rests when flat" behavior of
the five.

**Why it's 2nd, not 1st.** Its incompressibility is local repulsion, not a
global projection, so §7 honestly lists "slight compressibility," "no hard
volume conservation," and a "grainy / boiling surface" from a single relaxation
pass (viscosity mitigates but doesn't eliminate it). And at 320 particles it is
"a chunky fluid." `flip-lite`'s real projection gives cleaner global leveling
and its 640 particles read denser. But this is genuinely a coin-flip on
realism, and it is the safer, more robust design.

## 3. `heightfield` — most premium *surface*, fatally narrow (74/100)

**The best-looking still water of all five — and a stiff bar the moment you
tilt the wrong way.** As a mass-conserving shallow-water PDE it produces a
**glassy, perfectly level, artifact-free surface** with the **best meniscus in
the field**: §4 forces the top cell-row of every column to `MENISCUS_IDX`, a
"continuous bright water line that tilts with the surface." Nothing else here
gives a *continuous* light-catching waterline; the particle methods give
dots/segments. On the roll axis it is "the money shot": runs downhill, climbs
the far wall, overshoots, settles — and it is rock-solid (four bounded
invariants, §7) and essentially free on CPU (<0.2 ms physics), leaving the whole
budget for render.

**Why it drops to 3rd despite the prettiest surface.** §0 states the fatal
limitation plainly: it has **exactly one slosh axis**. Tilt the watch *toward or
away from you* (pitch) and the water **does not run** — pitch "only modulates
wave stiffness." On a wrist watch the user tilts in every direction and will
read the pitch-inertness as broken half the time. It also **cannot go past
vertical** (single-valued surface "saturates into a filled edge wedge rather
than pouring over"), and §7 admits the body "can read as luminous gel/matrix
rather than granular water" — the "tiny squares" come from the render grid, not
the physics. A one-axis filled wave is precisely the **"stiff bar"** the lens
penalizes. Its surface *ideas*, though, are must-adopts for the winner.

## 4. `pic-grid` — real 2-D liquid, but grainy and dot-prone (70/100)

Full 2-D, momentum-carrying particles, real-ish incompressibility via a density
grid + one Jacobi relaxation pass, genuine surface-kick spray, aurora/LUT — it
does *everything* the lens asks, in any tilt direction. The problem is *how* it
looks doing it. §7 is candid: at 384 particles over a 24×24 grid a resting cell
holds only ~3–5 particles, "density is integer, so the pressure gradient is
slightly steppy and the surface can shimmer at the cell scale (~20 px)"; a
single Jacobi pass "under-relaxes," so a hard tilt "can transiently
over-compress against the low wall before leveling"; and — the direct lens hit —
"a thin/fast sheet can read as **separated dots** rather than a continuous
film." Leveling driven by a 20 px integer-density gradient is inherently
coarser than either `flip-lite`'s projection or `pairwise-hash`'s 16 px
repulsion, and it has neither a viscosity term nor a continuous meniscus to hide
the shimmer. It reads as a real liquid with a grainy, steppy surface — better
than sand, short of premium.

## 5. `falling-sand` — the failure mode the lens names (58/100)

**Incompressible and rock-solid, but it is literally grainy sand with faked
slosh.** As a cellular automaton it wins two constraints cleanly (1 cell = 1
unit ⇒ "pile to a singularity is physically impossible"; a bounds-compare wall
⇒ no leak) and handles arbitrary 2-D tilt via an octant argmax. But it
surrenders the pillar that matters most for *convincing* water: **momentum**.
§0/§7 admit "a positional CA carries no momentum" — inertia is *one global slosh
vector* biasing lateral flow, so "the liquid can't carry a genuine traveling
wave or a real standing sloshing mode; climb the far wall and slosh back is
**approximated, not simulated**." On top of that it lists "**surface flicker /
boil** — the classic CA left-right shimmer at the waterline," "**8-way
anisotropy**" staircasing at ~45° tilt, a "**blocky meniscus** — a stair-stepped
cell edge, not the glassy line a height-field gives," and bolted-on spray. This
is the exact triad the lens tells us to penalize: it reads as **grainy sand**,
its slosh is a stiff global fake, and its surface boils. Stability is its only
virtue on this lens, and stability is table stakes that every other design also
meets.

---

## What the synthesis MUST adopt (from any design)

The final build should be a **`flip-lite` dynamic core** (real projection +
FLIP momentum + emergent spray, full 2-D) with the surface treatment of the
better-surfaced designs grafted on to cure FLIP's one real weakness — a
coarse/sparse free surface:

1. **`flip-lite`: the real FLIP/PIC core** — divergence-free Jacobi projection
   (upgrade to red-black Gauss-Seidel so the vertical-column far-wall climb is
   not "a touch soft") + 95% FLIP / 5% PIC blend + DAMP≈0.992. This is the
   incompressible-*and*-momentum coupling that makes it read as water. Keep the
   640-particle count (denser reads less dotty); spend headroom on iterations.
2. **`flip-lite`: emergent spray** — droplets are particles the pressure solve
   can't hold at the leading wall, that arc and re-absorb. Do NOT fall back to a
   separate bolted-on ballistic pool.
3. **`pairwise-hash`: the viscosity term** (nudge `v_i` toward neighbours) to
   smooth the free surface / kill boil — the best single premium-surface idea in
   the field, and the direct cure for FLIP's grainy crests.
4. **`pairwise-hash`: SET-overlap body squares that merge into a solid dithered
   sheet** ("liquid, not dots") — the explicit defense against the bouncing-dots
   failure mode; keep `max_px` only for crest/meniscus glow.
5. **`pairwise-hash`: per-particle surface detection (neighbour count) for a
   sparkly meniscus, WITH hysteresis** on the surface threshold to stop meniscus
   flicker.
6. **`heightfield`: the continuous meniscus water line** — connect the topmost
   fluid per screen-column into a single bright, tilting cyan line laid *over*
   the particle swarm, for the glassy light-catching waterline particles alone
   can't give.
7. **`heightfield`: a strict volume-conservation nudge** as a safety net so the
   pool can neither drain nor flood from accumulated rounding (FLIP volume loss
   is real).
8. **`pairwise-hash`: rest-bias clamp to ±0.1 g** so a flat watch reads g≈0 and
   *breathes*, while tilt always dominates (calibration can never cancel a real
   tilt) — the most correct pool/tilt behavior.
9. **Shared look, from all five**: build-time `WATER_LUT` (deep-indigo → neon
   blue → cyan → white) indexed by speed↑ / depth↓, aurora drift via
   `lock::NOISE_A/NOISE_B`, and a Bayer offset on the LUT *index* (lit pixels
   only) to kill RGB565 banding.
10. **Rest breathing scaled DOWN as |g| grows** (`pairwise-hash` / `flip-lite`)
    so aliveness-at-rest never fights a real tilt.
11. **Damage/flush**: union-bbox partial flush when pooled, auto full-frame
    (~13 ms) under slosh via `flush_dirty`'s ¾ rule; clip liquid to `y ≥ 70` so
    `wheel::draw_status` stays topmost — the shared, correct integration story.
