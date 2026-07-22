# WHEEL-FEEL-RESEARCH — premium micro-interaction physics for the OpenPocket app wheel

Research target: `src/scenes/wheel.rs` + the wheel physics in `src/app.rs`
(`wheel_interact`, `wheel_track`, `wheel_coast`, `wheel_settle`, `wheel_rubber`,
`wheel_rubber`), a vertical snap-to-row carousel: 10 rows, `PITCH_PX = 68`, Q8
fixed-point, frame-indexed animation at ~30 fps (→ 40-50 fps after the flush
pipeline lands). Goal: Apple/Google-grade *feel* with **no layout or visual-style
changes** — only timing, response, and physics.

This document does not touch source. It ends with a **Prescription for OpenPocket**
in our own units (Q8 px/ms velocities, per-25 ms integer factors, px, ms).

---

## 0. What the code does today (baseline, for reference)

Measured from the source so every prescription below is a concrete delta:

| Stage | Current behavior | Source |
|---|---|---|
| Track | true 1:1 finger glue; rubber-band past ends | `wheel_track` |
| Release velocity | `vel_q8` = **last-2-sample** slope `(dpx<<8)/dt`, then **×3/2 boost** | `gestures.rs` `DragEnd`, `wheel_interact` |
| Decay | `v *= (256 − 8·dt/25)/256` → **248/256 per 25 ms** (dt-scaled) | `wheel_coast` |
| Coast stop | hard cutoff at `|v_q8| ≤ 64` (0.25 px/ms) | `wheel_coast` |
| Boundary | reflect: penetration ÷2, velocity `×(−5/16)` restitution (0.3125) | `wheel_coast` |
| Snap | **separate** phase after coast: nearest row, `s += diff·2/3` per frame, snap-to when `|diff| ≤ 768` (3 px) | `wheel_coast`, `wheel_settle` |
| Rubber band | linear `÷2` past either end | `wheel_rubber` |
| Tap-to-focus | same 2/3 ease (`wheel_settle`) | `app.rs` tap branch |
| Slop | `MOVE_SLOP_PX = 10` | `gestures.rs` |
| Frame pacing | coast runs **as-fast-as-possible**, `dt` clamped 10–50 ms (intro uses a blocking 25 ms `pace()`) | `wheel_coast`, `wheel::pace` |

**The single biggest structural issue** falls out of the numbers below: our decay
(248/256) is *lower-friction than iOS's "normal" scroll*, and we then multiply
release velocity by 1.5, and our list is only 9·68 = **612 px** of total travel.
Any medium-or-harder flick therefore projects **thousands of px**, rails to the
end, and triggers the ball-bounce — then a *disjoint* snap phase corrects it. The
motion reads as *coast → slam → bounce → re-settle* instead of one continuous
glide that lands on a considered row. Fixing that (velocity-projected targeting,
§2) is prescription #1.

---

## 1. iOS scroll physics, precisely

### Deceleration rates (confirmed)
`UIScrollView.DecelerationRate` exposes two documented constants, expressed as a
**per-millisecond** multiplier applied to velocity:

- `.normal` = **0.998 / ms** (the default for scroll views)
- `.fast` = **0.99 / ms**

After *k* ms, `v(k) = v0 · rate^k`. [Apple: UIScrollView.DecelerationRate];
confirmed against multiple engineering write-ups. ([Apple docs][a1], [Lobanov][a2])

**Convert to our per-25 ms integer factor** (`factor/256`):

| Rate | per-ms | `rate^25` (per 25 ms frame) | our integer factor |
|---|---|---|---|
| iOS `.normal` | 0.998 | 0.9512 | **243/256** |
| iOS `.fast` | 0.99 | 0.7778 | **199/256** |
| **OpenPocket today** | **0.99873** | **0.96875** | **248/256** |

Read the last row carefully: our 248/256 is **floatier than iOS `.normal`**
(248 > 243). For a long content scroll that's arguably fine; for a **10-row
picker** it is too slippery — it's why gentle flicks over-travel.

### How iOS derives release velocity from touch history
`UIPanGestureRecognizer.velocity(in:)` / `UIScrollView`'s internal tracker do
**not** use the single last delta. They compute a **recency-weighted velocity over
a short trailing window** (~the last few samples / ~tens of ms), so a finger that
*paused* just before lift releases with ~0 velocity and a still-accelerating
finger releases with its true lift-off speed. This is the same intent Android
makes explicit with a 2nd-degree least-squares fit (§6). Our **last-2-sample
slope is the naive version of this** and is the noisiest possible estimator on a
10 ms report stream. ([WWDC 2018 §803][a3], [Gitter][a4])

### The projected-endpoint formula (the important one)
From WWDC 2018 *Designing Fluid Interfaces* (session 803), the `project(...)`
helper Apple uses to turn a release velocity into a **landing point**:

```
// velocity in points/second, decelerationRate per-ms (e.g. 0.998)
func project(initialVelocity, decelerationRate) -> distance {
    return (initialVelocity / 1000) * decelerationRate / (1 - decelerationRate)
}
```

i.e. **distance = v_pxms · d/(1−d)** where `v_pxms` is px/ms and `d` is the
per-ms rate. This is the closed form of the geometric coast sum and is what lets
iOS pick a snap target *at the instant of release* so momentum and snap are one
motion. ([WWDC 2018 §803][a3], [Gitter, "project"][a4])

- iOS `.normal`: `d/(1−d) = 0.998/0.002 = 499` px per px/ms
- iOS `.fast`: `0.99/0.01 = 99` px per px/ms
- **Our discrete equivalent** (per-frame factor `f`, step `dt`): total travel =
  `v0 · dt/(1−f)`. With `f = 248/256`, `dt = 25`: **`dt/(1−f) = 800` px per px/ms.**

So our coast reaches even *further* than iOS `.normal` — a 3 px/ms hard flick,
after the ×1.5 boost (4.5 px/ms), projects **~3600 px**, ≈ 53 rows, on a 9-row
list. Confirmed root cause of the rail-and-bounce.

---

## 2. Snap-target projection — making momentum + snap ONE motion

**Apple (pagers & pickers).** `scrollViewWillEndDragging(_:withVelocity:
targetContentOffset:)` hands you the *projected* natural stopping offset; a
snapping scroll view **rewrites `targetContentOffset` to the nearest snap
boundary of that projection** before deceleration even begins. `UIPickerView` /
`UIDatePicker` / the Photos-style pickers do exactly this: project → choose the
row nearest the projected stop → **decelerate directly to that row** (paging
clamps to ±1 page; pickers snap to nearest, no ±1 clamp). There is no "coast then
correct" — the deceleration curve is *re-aimed* so its endpoint already is the
row. ([Apple: scrollViewWillEndDragging][a5], [WWDC 2018 §803][a3])

**Android (RecyclerView `SnapHelper`).** The mirror image: the fling velocity is
run through `OverScroller`/`SplineOverScroller` (spline model) to
`calculateScrollDistance`, then `LinearSnapHelper.calculateDistanceToFinalSnap`
adjusts the target to center the nearest item. `PagerSnapHelper` clamps to ±1.
Same principle: **estimate where the fling would stop, then snap the estimate.**
([SnapHelper source][a6], [rubensousa][a7])

**Concrete algorithm for our constants** (replaces the coast + separate snap):

1. On release, take `v0_q8` (Q8 px/ms; see §6 for a better estimate), boost ≈×1.0
   (drop the ×1.5 — see §5).
2. **Project** the natural stop, Q8, using the closed form with our chosen decay
   factor `f`:
   `s_proj = s + (v0_q8 · K) >> 0`, where `K = dt/(1−f)` in ms-px units.
   For `f = 243/256` (iOS-normal), `dt = 25`: `1−f = 13/256`, so
   `K = 25·256/13 ≈ 492`. Thus `s_proj_q8 = s_q8 + v0_q8·492` (v0_q8 is Q8 px/ms,
   ×492 ms → Q8 px). *(With today's `f = 248/256`, K = 800.)*
3. **Clamp** `s_proj` to `[0, s_max]` (a small overshoot budget of ~½ pitch is
   fine to allow a soft end-bounce; see §4).
4. **Choose the row:** `row = round(s_proj / PITCH)`, `target = row·PITCH`.
5. **Decelerate to `target` as one motion** (§3 settle) — the coast never "free
   runs" past the target and there is no separate snap phase.

Effect: a gentle flick lands 1-3 rows on; a medium flick 4-7; a hard flick pins
the end row and *softly* bounces instead of ricocheting. Momentum and snap become
indistinguishable — the defining trait of premium wheels.

---

## 3. The settle curve — landing a ≤68 px correction

**What Apple pickers use.** The final landing on `UIPickerView` / watchOS crown
lists reads as a **critically damped spring** (no overshoot, velocity continuous
from the fling). Critical damping = `damping = 2·√stiffness`; it's the fastest
approach that never crosses the target, which is exactly the "decisive but soft"
quality of an Apple detent. UIKit's own spring animator and SwiftUI's default
interactive springs are near-critically-damped. ([WWDC 2018 §803][a3],
[Holko, UIKit Dynamics][a8])

**Premium duration for a ≤68 px correction:** ~**200–300 ms** perceptually. Below
~120 ms a correction reads as a mechanical *snap* (no "settle"); above ~350 ms it
reads as *laggy*.

**Our current curve is too fast and too abrupt.** `s += diff·2/3` per frame is an
exponential with time constant `τ = −frame_ms / ln(1/3) ≈ 0.91·frame_ms` ≈ **30 ms
at 33 ms/frame**; it reaches the 3 px snap threshold in ~3 frames ≈ **~80–100 ms**.
That is a hard snap, not a settle — and because it's a *separate* phase after the
coast stops, the eye sees a velocity discontinuity (glide decelerates to a near
stop at the cutoff `|v|≤64`, *then* a fresh ease yanks it to the row).

**Recommendation (two tiers):**

- **Ideal — critically-damped spring** toward `target`, carrying the coast
  velocity in (so §2's deceleration *is* the settle). Pick `ω` for a ~250 ms
  settle: settle-to-~2% for critical damping ≈ `6/ω`, so `ω ≈ 6/250 ms
  = 0.024 rad/ms`. Integer semi-implicit Euler per frame `dt`:
  `a = −k·(s−T) − c·v; v += a·dt; s += v·dt` with `k = ω² ≈ 0.00057 /ms²`,
  `c = 2ω ≈ 0.048 /ms`. Scale into Q8 (keep `k` as `k·2^16` etc.). No overshoot,
  continuous from the fling.
- **Pragmatic — gentler exponential**, if the spring's integer scaling is
  unwelcome: change the step from `2/3` to **`~5/16` (≈0.31)** → `τ ≈ 90 ms`,
  settle ≈ **~220 ms**, and **remove the hard `|v|≤64` cutoff** so coast blends
  straight into this ease (§5). Keep the 3 px final snap-to.

---

## 4. Rubber band — the real curve, and end-bounce

**The classic Apple formula** (reverse-engineered, widely reproduced and matching
`c = 0.55`):

```
f(x) = (x · c · d) / (d + c · x)        // ≡ (1 − 1/(x·c/d + 1)) · d
```

`x` = distance dragged past the edge, `d` = dimension (iOS uses the scroll-view
size), `c = 0.55`. Properties: near the edge `f ≈ c·x` (linear, gentle); as
`x → ∞`, `f → d` (asymptotic — you can *never* pull past `d`, resistance rises the
harder you pull). Our linear `÷2` has the right *near-edge slope* (0.5 ≈ 0.55) but
**no progressive stiffening** — it keeps giving at half-rate forever, so a hard
pull feels loose instead of taut. ([originell gist][b1], [Holko][a8],
[Codename One][b2])

**Integer-friendly approximation** (c ≈ 141/256 ≈ 0.55, choose a *small* `d` for a
firm watch feel — the full 466 px is too soft for a picker; use ~1.4 pitch):

```
d = 96                                  // px; max overshoot asymptote
give = (x · 141 · d) / (256·d + 141·x)  // all i32; near-edge ≈0.55x, caps at d
```

(Numerator ~ 141·96·x; denominator 256·96 + 141·x. For small x → 0.55x; for large
x → 96 px.) Drop-in for `wheel_rubber`'s `t/2` branches, applied to the overshoot
`x = −t` or `t − max`.

**Boundary BOUNCE when a fling hits the end.** iOS does **not** reflect velocity
with a restitution coefficient (our `×−5/16` ball-bounce). It **carries the live
velocity *into* the stretch** (same rubber curve applied to position), lets a
spring absorb it, and **spring-backs, critically damped, to the exact edge** — a
single taut in-and-out with no ricochet. Two fixes, in order of impact:

1. **Clamp the projection (§2 step 3)** so hard flings *target the last row*
   rather than shooting past it. This removes the bounce from the common case
   entirely — the wheel simply decelerates onto row 0 / row N−1.
2. For the residual overshoot that remains, **replace the reflect** (`s = −s/2;
   v = −v·5/16`) with: let `s` enter the rubber region, then run the §3
   critically-damped spring back to the boundary row. No `×−5/16`. Result: a soft
   settle-back, not a bounce-back.

---

## 5. Micro-interaction inventory (picker / carousel)

Drawn from Apple Watch home/list, iOS pickers, Wear OS rotary lists, and the RAIL
response model.

- **Touch-down acknowledgment / the 100 ms rule.** A response within **100 ms**
  reads as *instant* (RAIL "Response" budget; ideal ≤50 ms); beyond it the
  action–reaction bond breaks. Our catch-on-touch (`finger_down()` freezes the
  wheel mid-coast, then 1:1 track) *is* the acknowledgment and is the right model
  — **provided the first tracked frame paints < 100 ms** after touch-down. Guard
  this once the flush pipeline lands. ([RAIL / web.dev][c1], [Laws of UX:
  Doherty][c2])
- **Tap-vs-scroll disambiguation.** Movement-based (not time-based) is correct.
  Our `MOVE_SLOP_PX = 10` sits right on the platform norm (iOS ~10 pt, Android
  `touchSlop` ~8 dp). Keep it.
- **Velocity thresholds (tap / drag / fling).** tap = no move past slop; drag =
  past slop; **fling = release |v| above a floor.** We have *no* floor — any tiny
  residual velocity feeds momentum. Add **|v0| < ~0.30 px/ms (77 Q8) ⇒ treat as a
  drag-release: settle to nearest row, no projection**, so a slow finger-lift
  doesn't drift a row.
- **Row-focus feedback timing.** Apple animates the selected row's scale/emphasis
  with a short spring on landing. We can't change *visual styles*, but the
  **timing** of the existing size/alpha crossfade should ride the §3 settle so
  focus "locks in" exactly when motion stops — not before (during the disjoint
  snap the focus currently pops early).
- **Overscroll norms.** Android: EdgeEffect glow (≤11) / **stretch overscroll**
  (12+, a `SpringAnimation`); iOS: rubber + spring-back (§4). Both are *soft and
  progressive* — reinforces dropping the hard reflect.
- **Haptics substitute (we have none).** Wear OS fires a **haptic tick per item**
  as rotary lists cross detents (`RotaryScrollableDefaults.snapBehavior`, rotary
  haptics). With no haptic hardware, the premium substitute is a **subtle visual
  tick** — a brief brightness/scale pulse on the row as it crosses center. This is
  a *micro-interaction timing* cue, not a style change; flagged **optional** given
  the no-visual-change constraint. ([Wear rotary input][d1], [Wear Compose 1.2][d2])
- **"Scroll-wheel detents" in the physics.** Yes — good wheels add a faint
  per-row *magnetism*: negligible during fast coast (never fights momentum),
  growing as speed drops so the last stretch settles onto a row rather than
  between two. Implement in the decay loop as a **speed-gated pull toward the
  nearest row center**: once `|v|` falls below a threshold (e.g. ~0.6 px/ms,
  154 Q8), each frame add `pull = (nearest_center − s) · g` with small `g`. This
  *is* the continuous hand-off from coast to §3 settle and removes the hard
  `|v|≤64` cutoff and the separate snap loop.

---

## 6. Touch-history velocity on a noisy 10 ms stream

**Best practice.** Android's `VelocityTracker` default strategy is **LSQ2** — a
weighted least-squares fit of a **degree-2 polynomial** `y ≈ B0 + B1·x + B2·x²`
over a trailing horizon (~100 ms), taking `B1` as the lift-off velocity; the
quadratic term captures acceleration so a still-accelerating flick reports its
*true* release speed, not an averaged-down one. iOS uses the same intent via a
recency-weighted window (§1). ([VelocityTracker source][e1], [android-contrib][e2])

**Integer-friendly recommendation for the CST9217** (report rate *collapses* at
low finger speed, coords jitter at panel edges): a full LSQ2 is overkill for Q8
no_std. Use a **recency-weighted average of the last 3 inter-sample velocities
inside a fixed ~50–60 ms horizon**:

```
// keep last 3 (dy, dt) in Track; vi = (dy_i << 8) / dt_i   (Q8 px/ms)
v0 = (3·v_newest + 2·v_mid + 1·v_old) / 6          // 3:2:1 recency weights
// only include samples whose age < HORIZON_MS (≈60); if the finger paused
// before lift, the stale samples fall out of the window and v0 → ~0 naturally.
```

Why this beats today's last-2 slope:

- **Noise:** averaging 3 deltas halves the per-sample jitter that a single 10 ms
  delta injects.
- **Low-speed collapse:** when the CST9217 stops reporting (slow finger), the
  newest `dt` is large ⇒ that sample's `vi` is small ⇒ weighted result is small —
  the *correct* "released slowly, don't fling" outcome. The horizon guard formalizes
  it (drop samples older than ~60 ms).
- **Edge jitter:** the 3:2:1 blend damps a single spurious edge coordinate instead
  of letting it define the whole release velocity (today one bad lift sample with a
  tiny `dt` can spike `vel_q8`).

Pair with **dropping the ×1.5 boost to ≈×1.0–1.15** (§5): once §2 projection sets
the *reach*, the boost only inflates the projection into the clamp. A small ≤×1.15
"generosity" is enough to make flicks feel eager without over-travel.

---

## 7. Frame pacing — consistency over peak fps

**Finding.** Perceived smoothness depends **more on *consistent* frame intervals
than on raw fps.** Humans track motion well and are highly sensitive to *variance*
in frame timing ("jank"); a steady 30 fps reads smoother than a 30–50 fps stream
that jitters frame-to-frame. This is the basis of Google's jank metric, the
Android Frame-Pacing library, and the RAIL 16 ms-per-frame budget framed as a
*consistency* target. ([RAIL / web.dev][c1], [Laws of UX: Doherty][c2])

**Our situation.** Physics is already real-`dt` scaled, so *positions* stay
correct as frame cost varies — but the coast/settle loop runs
**as-fast-as-possible** with `dt` swinging 10–50 ms, so the *visual cadence*
jitters even though the math is right. That variance is exactly what the eye
dislikes.

**Recommendation.** Pace the coast + settle loop to a **fixed target interval at
or just above worst-case frame cost**, the way the intro already blocks to 25 ms
via `wheel::pace()`. Once the flush pipeline lands and worst-case
render+flush is known (say ~28 ms), pick a **steady period `T` the pipeline can
*always* hit — e.g. 33 ms (30 fps) or 25 ms (40 fps)** — and pace every physics
frame to `T` with the existing spin-wait. A rock-steady 30 fps will feel more
premium than an un-paced 30–50 fps sawtooth. (Keep `dt` real for the physics; only
the *cadence* is fixed.)

---

## Prescription for OpenPocket (ordered by expected feel impact)

All values in our units. Each line: **change → one-line rationale.**

1. **Velocity-projected snap target — unify coast + snap into one motion.**
   On release, project the stop `s_proj_q8 = s_q8 + v0_q8·K` (K = `dt/(1−f)`;
   `≈492` at `f=243/256`, `800` today), **clamp to `[0, s_max]`**, pick
   `row = round(s_proj/PITCH)`, and **decelerate straight to that row** — delete
   the free-running coast + separate snap phases.
   *Why:* kills the current coast→rail→bounce→re-settle; the wheel lands on a
   *considered* row exactly as iOS/Android pickers do. **(Highest impact.)**

2. **Drop the ×1.5 release boost to ≈×1.0–1.15.**
   In `wheel_interact`/flick path, replace `* 3 / 2` with `*1` (or at most a
   `×1.15` "generosity").
   *Why:* with projection setting reach, the 1.5× only inflates the projection
   into the end-clamp — it's the other half of why flings rail.

3. **Recency-weighted 3-sample release velocity over a ~60 ms horizon.**
   In `gestures.rs`, keep the last 3 `(dy,dt)` and emit
   `v0 = (3·v_new + 2·v_mid + 1·v_old)/6`, discarding samples older than ~60 ms.
   *Why:* the last-2-sample slope is the noisiest possible estimator; this is
   robust to CST9217 low-speed report collapse and edge jitter, matching
   VelocityTracker/iOS intent cheaply.

4. **Retune decay to iOS-normal: 248/256 → 243/256 (tunable).**
   Change the coast factor so per-25 ms is `243/256` (per-ms ≈0.998); expose it as
   a named const so it can be dialed toward `.fast` (199/256) if a firmer picker
   feel is wanted.
   *Why:* today we're *floatier than iOS normal* on a tiny 612 px list; a familiar,
   slightly firmer glide reads more precise. (§2's K updates with `f`.)

5. **Continuous, softer settle — critically-damped spring (or 2/3 → ~5/16 exp),
   and delete the `|v|≤64` hard cutoff.**
   Carry coast velocity into a critically-damped approach to the target
   (`ω≈0.024/ms`, ~250 ms), *or* pragmatically drop the ease step from `2/3` to
   `~5/16` (τ≈90 ms) with no separate cutoff.
   *Why:* the current ~80 ms 2/3 ease is a hard *snap* with a visible velocity
   discontinuity at the coast→snap seam; a ~220–250 ms damped landing is the
   Apple-picker "decisive but soft" detent. Apply the same settle to `wheel_settle`
   (tap-to-focus) for consistency.

6. **Progressive rubber band + spring-back boundary (replace linear ÷2 and the
   restitution reflect).**
   `give = (x·141·d)/(256·d + 141·x)` with `d≈96` in `wheel_rubber`; and once
   projection clamps (Rx #1), replace `s=−s/2; v=−v·5/16` with a critically-damped
   spring-back to the edge row.
   *Why:* real Apple rubber stiffens the harder you pull (ours stays loose), and
   iOS spring-backs rather than ball-bounces — removes the "gamey" ricochet.

7. **Fixed-cadence frame pacing for coast + settle.**
   Pace every physics frame to a steady `T` (e.g. 33 ms / 30 fps, or 25 ms once
   the pipeline sustains 40 fps) via the existing spin-wait, instead of
   run-as-fast-as-possible. Keep `dt` real for the math; fix only the cadence.
   *Why:* consistent frame intervals read as more premium than a jittery
   30–50 fps sawtooth.

8. **Micro-interaction polish.**
   (a) Fling floor: release `|v0| < ~0.30 px/ms (77 Q8)` ⇒ settle-to-nearest, no
   projection — a slow lift shouldn't drift a row. (b) Add a speed-gated per-row
   "detent" pull (grows as `|v|` drops) as the mechanism that *implements* Rx #5's
   hand-off. (c) *Optional* visual detent tick (brightness/scale pulse crossing
   center) as the no-hardware substitute for Wear OS haptic ticks — timing-only,
   flagged because of the no-visual-change constraint.

---

## Sources

- [a1] Apple Developer — UIScrollView.DecelerationRate (`.normal` 0.998, `.fast` 0.99): https://developer.apple.com/documentation/uikit/uiscrollview/decelerationrate-swift.struct
- [a2] Ilya Lobanov — "How UIScrollView works" (deceleration rate as per-ms multiplier): https://medium.com/@esskeetit/how-uiscrollview-works-e418adc47060
- [a3] Apple WWDC 2018 Session 803 — *Designing Fluid Interfaces* (projection function; deceleration-rate-based endpoints): https://developer.apple.com/videos/play/wwdc2018/803/ · transcript: https://asciiwwdc.com/2018/sessions/803
- [a4] Nathan Gitter — "Building Fluid Interfaces" (the `project(initialVelocity:decelerationRate:)` code): https://medium.com/@nathangitter/building-fluid-interfaces-ios-swift-9732bb934bf5
- [a5] Apple Developer — `scrollViewWillEndDragging(_:withVelocity:targetContentOffset:)` (rewrite target to snap boundary): https://developer.apple.com/documentation/uikit/uiscrollviewdelegate/1619385-scrollviewwillenddragging
- [a6] AOSP — `SnapHelper` / `LinearSnapHelper` (`calculateDistanceToFinalSnap`, `calculateScrollDistance`): https://android.googlesource.com/platform/frameworks/support/+/android-cts-8.0_r5/v7/recyclerview/src/android/support/v7/widget/SnapHelper.java
- [a7] Rúben Sousa — "RecyclerView snapping with SnapHelper": https://rubensousa.github.io/2016/08/recyclerviewsnap
- [a8] Arkadiusz Holko — "UIScrollView's Inertia, Bouncing and Rubber-Banding with UIKit Dynamics": https://holko.pl/2014/07/06/inertia-bouncing-rubber-banding-uikit-dynamics/
- [b1] originell — "Analysis of Apple's rubber band scrolling" (`f(x)=(x·d·c)/(d+c·x)`, c=0.55): https://gist.github.com/originell/6961057
- [b2] Codename One — "iOS Density, Scroll Physics, and Accessibility" (rubber-band coefficient 0.55): https://www.codenameone.com/blog/ios-density-scroll-and-accessibility/
- [c1] Google/web.dev — RAIL performance model (Response ≤100 ms; 16 ms frame budget / consistency): https://web.dev/articles/rail
- [c2] Laws of UX — Doherty Threshold (<400 ms) & instant-feedback: https://lawsofux.com/doherty-threshold/
- [d1] Android Developers — "Rotary input with Compose" (Wear OS, snap fling + rotary haptics): https://developer.android.com/training/wearables/compose/rotary-input
- [d2] Android Developers Blog — "Compose for Wear OS and Tiles 1.2" (rotary snap/haptics): https://android-developers.googleblog.com/2023/08/compose-for-wear-os-and-tiles-1-2-libraries-now-stable-new-features.html
- [e1] AOSP — `VelocityTracker.java` (LSQ2 default, degree-2 least squares; strategy list): https://github.com/aosp-mirror/platform_frameworks_base/blob/master/core/java/android/view/VelocityTracker.java
- [e2] android-contrib — VelocityTracker LSQ2 / quadratic velocity discussion: https://groups.google.com/g/android-contrib/c/TEAYoX0NzTw
