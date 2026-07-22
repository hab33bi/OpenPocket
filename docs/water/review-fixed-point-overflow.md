# Water — adversarial review: FIXED-POINT & OVERFLOW lens

Scope: every multiply/shift in `water_draft.rs` checked for i32/i16 overflow at
worst-case reachable operand magnitudes, plus fixed-point precision loss that
would stall or jitter the sim. Verdict: **fix-then-ship**.

**No reachable integer overflow exists.** I re-derived every hot-loop product
against the actual reachable operand ranges (not the spec's stated ones) and all
stay well inside i32; every `as i16` store stays inside `i16`. The load-bearing
proofs (Q6-i16 positions ≤ 30 144; wall-projection intermediate 3.4 M; d2 index
guarded by `d2 < H2`; hash counts ≤ NW in u16) all hold. Details in the
"verified safe" section.

However, two real **fixed-point precision defects** exist (the second half of
the lens: "flag precision loss that would stall or jitter"), plus one incorrect
overflow-proof bound that is a latent risk. Ranked below.

---

## F1 — MEDIUM — DAMP right-shift rounds toward −∞: small negative velocities never decay (breaks "always settles")

**Location:** `water_draft.rs`, `Water::step` step 4, lines 397–400
(`self.vx[i] = (((self.vx[i] as i32 * DAMP) >> 8).clamp(...)) as i16;` and the
`vy` twin).

**Defect.** `>> 8` on a signed i32 is an *arithmetic* shift (floor toward −∞),
not round-toward-zero. With `DAMP = 254`, the map `v → (v*254) >> 8` has a whole
band of **fixed points on the negative side**: for every `-64 ≤ v ≤ 0`,
`(v*254) >> 8 == v`. Meanwhile positive velocities decay normally
(`+64 → +63`, `+1 → 0`). Verified exhaustively:

```
v = -64  (v*254)>>8 = -64   <-- FIXED POINT (no decay)
v = -10  (v*254)>>8 = -10   <-- FIXED POINT
v =  -1  (v*254)>>8 =  -1   <-- FIXED POINT
v =  +1  (v*254)>>8 =   0       decays
v = +64  (v*254)>>8 = +63       decays
```

`-64 Q6 = -1.0 px/frame`. So any particle carrying a residual velocity of up to
1 px/frame in the screen-negative direction (−x / −y, whichever the IMU sign
consts map) is **never damped** — the very step the spec calls "the keystone …
energy is strictly dissipative … absent input the pool always settles" is a
no-op for half the low-velocity space. IMPL-SPEC §9 ("net-dissipative", "always
settles") is therefore false as written.

**Triggering scenario.** Flat watch at rest (gx,gy ≈ 0 after bias removal). A
body particle picks up `v = -64` from a repulsion kick, a wall reflect, or the
breathing term. DAMP leaves it at −64 forever; it advects at −1 px/frame. The
whole pool translates toward the −x/−y wall as a slab (bounded there only by the
wall reflect flipping the sign, after which the now-positive velocity *does*
damp). Visible result: the "resting" pool creeps off-center and never goes
truly still — directly against the brief's "levels and settles … rock-solid
stable" requirement, and it reads as a slow one-directional drift rather than
the intended symmetric breathing.

**Fix.** Round toward zero so damping is symmetric and strictly contractive for
both signs. Rust integer `/` truncates toward zero:

```rust
self.vx[i] = (((self.vx[i] as i32 * DAMP) / 256).clamp(-VMAX, VMAX)) as i16;
self.vy[i] = (((self.vy[i] as i32 * DAMP) / 256).clamp(-VMAX, VMAX)) as i16;
```

Now `-64 → -63`, `-1 → 0`, `+64 → +63`, `+1 → 0` — both signs decay to zero.
Cost: a signed divide instead of a shift, 896×/frame ≈ a few µs on the 5 ms
physics budget — negligible. (Shift-preserving alternative if that matters:
`(v*DAMP + ((v >> 31) & 0xFF)) >> 8`, which adds a +255 bias only when `v < 0`,
i.e. rounds toward zero without a divide.)

---

## F2 — LOW — Gravity/jerk `>> 6` floor-rounds zero-mean rest noise into a net negative DC bias

**Location:** `water_draft.rs`, `Water::tick`, lines 313–314 (gravity
`(rmx - self.bias_x) >> GRAV_SHR`, same for `y`); the jerk `>> 6` at lines
331–332 shares the class but is transient (flick-only), so this is really about
the gravity path.

**Defect.** Same arithmetic-shift floor. For a small signed input `r`, `r >> 6`
gives **−1 for every `r ∈ [-64,-1]` but 0 for every `r ∈ [0,63]`**:

```
r = -40  r>>6 = -1        r = +40  r>>6 = 0
r =  -1  r>>6 = -1        r =  +1  r>>6 = 0
```

At flat rest `rmx − bias_x` is zero-mean sensor noise (the ±0.1 g bias capture
cancels the offset). Because negatives round to −1 and positives round to 0,
symmetric noise produces an **asymmetric, always-negative** gravity of ≈ −0.5 Q6
per frame on each in-plane axis — a phantom constant tilt toward screen −x/−y
that no real gravity is applying. It integrates into velocity every frame and
compounds F1's creep (and persists even if F1 is fixed, since the fixed DAMP
would then settle to a small nonzero equilibrium velocity feeding on this bias).

**Triggering scenario.** Watch lying flat, IMU live, in-plane raw dithering
around the captured bias by ±a few LSB. Every frame where the residual is
slightly negative injects `g = -1`; the slightly-positive frames inject `0`.
Net: the pool drifts/leans toward one corner at rest instead of sitting centered
and merely breathing.

**Fix.** Round toward zero on the gravity map (and, for consistency, the jerk):

```rust
let gx = ((rmx - self.bias_x) / 64).clamp(-GMAX, GMAX);
let gy = ((rmy - self.bias_y) / 64).clamp(-GMAX, GMAX);
```

`/64` truncates toward zero, so `[-63,63] → 0` symmetrically and the zero-mean
noise no longer has a DC component. (GRAV_SHR is fixed at 6, so `/64` is exact.)

---

## F3 — LOW — Render `vx*vx + vy*vy` operand exceeds VMAX; IMPL-SPEC §2 proof item 11 bound is wrong (latent, not a current overflow)

**Location:** `water_draft.rs`, `Water::render`, line 582
(`let spd = isqrt((vx * vx + vy * vy) as u32) as i32;`), evaluated against
IMPL-SPEC §2 item 11.

**Defect.** `Water::wall` (lines 511–518) is the **only** velocity store that is
not re-clamped to VMAX. `self.vx[i] = (vx - (k * dx) / r) as i16;` with
`k = (vn*(256+REST_E)) >> 8` can produce a per-component magnitude up to
`VMAX + k ≈ 1024 + 1595 = 2619 Q6` (verified: `|vn| ≤ √2·VMAX = 1448`,
`k ≤ 1448·282/256 = 1595`). `render` runs after `step` (which ends on `wall`),
so it reads these post-reflect velocities. IMPL-SPEC §2 item 11 states the
worst case is `spd2 = 1024²·2 = 2 097 152` — that assumes `|v| ≤ VMAX` at render
time, which is **false**. The true reachable worst case is `2619²·2 =
13 718 322`.

**Why it is not a bug today.** 13.7 M is still ~157× inside i32, so
`(vx*vx + vy*vy)` computed in i32 does not overflow, and `as u32`/`isqrt` are
fine. The over-VMAX velocity is also harmless downstream: next frame's stage-1
integrate re-clamps it before advecting and before re-storing (`vx.clamp(±VMAX)`
then `self.vx[i] = vx as i16`), so positions stay in the proven i16 range. So
**no correction is required for correctness right now.**

**Why it is worth recording.** (a) The §2 proof's stated bound is simply wrong
and the "velocity clamp ±VMAX" invariant in the §2 table does not hold at render
time — a reviewer relying on that proof for a future edit is misled. (b) It is a
latent overflow: the i32 square only survives because reachable `|v|` is small.
If a tuning pass raises `VMAX_PX` (§10 does not, but §9 discusses knobs) or
`REST_E`, or adds a second impulse source after `wall`, the render square can
climb toward the i32 ceiling (`|v| > 32767` is impossible in i16, and even the
i16 extreme `2·32768² = 2 147 483 648` overflows i32 by 1). 

**Fix (defensive, cheap).** Either clamp velocity at the end of `wall` so the
documented invariant actually holds —

```rust
self.vx[i] = (vx - (k * dx) / r).clamp(-VMAX, VMAX) as i16;
self.vy[i] = (vy - (k * dy) / r).clamp(-VMAX, VMAX) as i16;
```

— (this also tightens F1's creep at the wall and costs nothing), **or** widen
the render accumulation and correct the proof:
`let s2 = (vx*vx + vy*vy) as u32;` is already u32-safe once you note `vx,vy` are
i16 so `vx*vx ≤ 32767²`; just update §2 item 11 to the real 13.7 M bound and the
§2 table's velocity-range row to "≤ VMAX except one frame post-wall-reflect
(≤ ~2619)".

---

## Verified safe (no defect — checked at reachable worst case)

All operands below are the *reachable* extremes, tighter than or equal to the
spec's, and every product/store is inside range:

- **Positions Q6-i16.** Prev frame ends with all `r ≤ WALL_R = 222` (hard
  projection), integrate adds `|v| ≤ VMAX_PX = 16 px` → `r ≤ 238`, max coord
  `233+238 = 471 → 471<<6 = 30 144 < 32 767`. Wall projection result is
  `≤ CX<<6 + WALL_R<<6 = 29 120` (since `r ≥ |dx|` ⇒ `(dx·WALL_R)/r ≤ WALL_R`).
  Both fit i16. ✔
- **Wall projection intermediate.** `((dx·WALL_R) << 6)` with `|dx| ≤ 238`:
  `238·222 = 52 836`, `<<6 = 3 381 504` — the single largest intermediate in the
  program, 0.16 % of i32. Parenthesization `((dx*WALL_R) << FP) / r` shifts
  before dividing (precision preserved), and `/r` never divides by zero
  (`r2 > WALL_R2 ⇒ r ≥ 222`). ✔
- **Pair `d2`.** Even with overshoot folded into edge cells by the `cell_of`
  clamps, `|dx|,|dy| ≲ 64`, `d2 ≲ 8192` in i32; and `PUSH_LUT[d2]` is only
  indexed after `if d2 >= H2 || d2 == 0 { continue }`, so the index is always
  `1..255` into a `[i16;257]`. No OOB, no d2 overflow. ✔
- **Repulsion / viscosity accumulation.** After reject `|dx| ≤ 15`,
  `push ≤ 8192` ⇒ `(push·dx)>>7 ≤ 960`/term; viscosity `(≤2048·40)>>8 ≤ 320`
  /term; capped at `K_CAND = 24` ⇒ `|ax| ≤ 30 720`; `vix + (ax>>1)` clamped to
  ±VMAX before `as i16`. ✔
- **Wall reflect internals.** `vx·dx+vy·dy ≤ 487 424`, `vn ≤ 1448`,
  `vn·282 ≤ 408 336`, `k·dx ≤ 379 610` — all far inside i32. ✔
- **Damp product.** `1024·254 = 260 096`. ✔ (rounding is F1, magnitude is fine.)
- **Render index math.** `nbr ≤ 24` (candidate cap ⇒ `n ≤ 24`) so `depth ≤ 72`;
  `base·alpha ≤ 556·256 = 142 336`; `idx` clamped `0..255` into `WATER_LUT`. ✔
- **Aurora / NOISE index.** `((x>>2)±t) & 255` yields `0..255` regardless of how
  large `t = elapsed_ms>>5/6` grows or the sign of the sum (`& 255` masks the low
  byte), so no OOB on the `[u8;256]` tables even at `elapsed_ms` near u32::MAX. ✔
- **Hash.** `cell_start/cursor : u16`, max value = NW = 448 ≪ 65 535; `order`
  index `s < cell_start[c+1] ≤ 448`; cell index `≤ 899 < 900`. ✔
- **`put565`/`fill_rect_black`/`max_px`/`line_max`/`soft_glow`.** All fb indices
  are range-guarded (`i+1 < fb.len()`, `b <= fb.len()`) and their internal
  products (`(x1-x0)*s ≤ 217 156`, `tint*v ≤ 65 025`) are small. ✔
- **Sign-flip trap avoided.** Mapped raw and jerk are kept in i32
  (`IMU_*_SGN * axis` → i32), never stored sign-flipped in i16, so the
  `-1 * -32768` i16 overflow (the documented pic-grid bug) cannot occur. ✔

---

## Verdict

**fix-then-ship.** Nothing overflows, leaks, or blows up on this lens. Land
**F1** before shipping — it silently breaks the design's central "strictly
dissipative / always settles" guarantee and produces a visible one-directional
rest-creep; the fix is a one-token change (`>> 8` → `/ 256`) per line. **F2** is
the same one-token class on the gravity map and should go in with F1 (they
compound). **F3** is not a current overflow — record the corrected bound and,
ideally, add the free `.clamp(±VMAX)` at the end of `wall` so the documented
velocity invariant is actually true and the render square can never approach the
i32 ceiling under future tuning.
