# Touch Pipeline Research: Gesture Detection & Low-Latency Scrolling for a Premium Wheel UI

Target system: ESP32-S3 @ 240 MHz single core, CST9217 over I2C 400 kHz polled ~10 ms,
466x466 round AMOLED, 25 ms frame cadence, 13 ms full QSPI flush (partials faster),
retained PSRAM framebuffer with row-span damage tracking. Scroll physics
(velocity-projected snap, iOS .fast decay 199/256 per 25 ms, K=112 ms projection,
progressive rubber band, 2-segment release velocity) already implemented.

---

## 1. Touch-down-to-first-movement latency and the slop window

**Perceived-latency budgets (the numbers).** The perception literature splits cleanly by task:

- **Dragging (finger down, content tracking):** users can discriminate latency down to
  ~2-6 ms; what they actually perceive is the *spatial gap* between finger and content,
  which is latency x finger speed (Ng/Jota/Dietz/Wigdor, CHI 2013, "How Fast is Fast
  Enough?"). The just-noticeable difference for direct-touch **dragging is ~11 ms**;
  improvements as small as **8.3 ms** are noticeable (Deber et al., CHI 2015, "How Much
  Faster is Fast Enough?").
- **Tapping / land-on:** JND is ~**69 ms** for direct touch (Deber 2015), and in the
  land-on segment of a drag users **could not distinguish 1 ms from 64 ms** (Jota 2013).

Practical reading: the drag-tracking loop is where every millisecond counts (at 300 px/s
finger speed, each 10 ms of latency is a 3 px finger-content gap); tap feedback has a
~70 ms grace budget, which one 25 ms tick plus a partial flush comfortably meets.

**How platforms handle the slop window.** Both platforms accept a genuinely dead slop
window — content does not move until slop is exceeded — but two things keep it from
feeling dead:

1. **Keep slop small and consume the remainder.** Android's slop is **8 dp** (~1.3 mm).
   AOSP `ScrollView.onTouchEvent` **subtracts the slop from the first scroll delta**
   (`deltaY -= mTouchSlop`) — the scroll re-anchors at the slop boundary, so the first
   rendered movement is only the few pixels *beyond* slop. Content starts moving smoothly
   from zero rather than jumping 8 dp to "catch up" (AOSP ScrollView.java).
2. **iOS does the same via the pan recognizer's translation origin.** UIScrollView's pan
   measures translation from the recognition point (~10 pt to recognize — empirical,
   never published). Net effect identical: no positional jump at recognition.

Android bounds the *time* dimension too: **TAP_TIMEOUT = 100 ms** ("duration we wait to
see if a touch is a tap or a scroll") (AOSP ViewConfiguration.java).

## 2. Fast gesture classification, and biasing it toward scroll

Production pattern: **time + distance hybrid with per-axis asymmetry**:

- **Distance:** 8 dp slop on the scrollable axis; **PAGING_TOUCH_SLOP = 2x (16 dp)** for
  the cross-axis — deliberately *harder* to trigger the non-dominant gesture.
  RecyclerView exposes `setScrollingTouchSlop()` (TOUCH_SLOP_DEFAULT vs TOUCH_SLOP_PAGING).
- **Direction gating:** RecyclerView only starts a drag on an axis where the view can
  scroll — on a vertical-only list, horizontal wander never delays scroll classification.
- **Time:** TAP_TIMEOUT 100 ms, LONG_PRESS 400 ms, DOUBLE_TAP 300 ms — classification is
  forced within ~100 ms rather than waiting indefinitely.

**The "eager scroll" pattern** (Apple, WWDC18-803 Designing Fluid Interfaces): respond
instantly, treat the dominant gesture as the default hypothesis, reclassify late. On a
scroll-dominant surface, move content as soon as scroll-axis movement exceeds a minimal
jitter gate; classify **tap retroactively** (finger up while inside slop, within
~200-300 ms). A tap misclassified as a 3 px scroll is invisible; a scroll misclassified
as a pending tap feels dead.

## 3. Consecutive-interaction pipelines: catch, brake, and fling chaining

**iOS: stop-on-touch ("catch"), no velocity accumulation.** Touch-down during
deceleration stops the deceleration and enters tracking directly — the finger catches
the moving content with **no slop wait for the catch itself**; the next fling starts
fresh from the new release velocity. iOS compensates with high max fling velocities.

**Android: the Scroller "flywheel."** AOSP `Scroller.fling()` chains: if a new fling
starts while one is running, it computes residual velocity (`getCurrVelocity()`) and
**adds it to the new fling's velocity — only if `Math.signum()` matches** (same
direction). On by default since Honeycomb ("successive fling motions will keep on
increasing scroll speed"). Chromium ported it as "fling boost" with the condition that
the intervening touch be brief and direction-consistent (boost windows on the order of
tens to ~150 ms). Classic widgets are inconsistent (ScrollView/RecyclerView fling fresh).
Reference constants: SCROLL_FRICTION 0.015, DECELERATION_RATE ln(0.78)/ln(0.9)=2.36,
INFLEXION 0.35, min/max fling **50 / 8000 dp/s**.

**iPod click wheel:** end-to-end traversal via **input-rate acceleration** (scroll step
per wheel-degree grows with rotation speed), not momentum chaining (Apple patents
6865718, 2005/0097468).

**Synthesis:** premium = *stop-on-touch as the base rule* (control) + *conditional
chaining* for rapid same-direction re-flicks (traversal).

## 4. Frame pipeline during active scrolling

**Input sampling order — poll as late as possible before use** (VR "late latching",
Meta: cuts 2-5 ms motion-to-photon). Embedded translation: order each tick
**poll touch → integrate physics → render damage → flush**, poll immediately before
physics. Any other ordering silently adds up to one full tick.

**Frame pacing beats raw throughput** (Android Frame Pacing library/AOSP docs):
alternating short/long frames is perceived as stutter even at higher average fps; the
pacing library deliberately *delays* presentation to hold a steady cadence. A rock-steady
40 Hz with constant input-to-photon phase beats jittery 45-60. With per-tick velocity
integration, cadence jitter reads as **velocity flicker**.

**Level-of-detail during fast motion.** Texture/AsyncDisplayKit's interface-state model:
expensive content is deferred off the interaction path during fast flings (placeholders),
full fidelity restored as motion settles. Formal "drop decorations above velocity X"
doctrine is thin (honest gap — practiced as motion LOD in games), but doubly effective
here: simpler frames also mean **smaller damage spans → faster partial flushes**. LVGL
guidance converges: dirty-region minimization is the dominant embedded lever.

## 5. Overscroll rubber-band values

Reverse-engineered UIScrollView formula: **offset' = (x·d·c)/(d + c·x)**, with
**c = 0.55** and **d = the viewport dimension along the scroll axis** (466 here).

- The **initial slope of the curve is c**: at the moment the edge is crossed, content
  still tracks the finger at **55%** — soft, connected engagement.
- The function **asymptotes at d**: overscroll never exceeds one viewport dimension.
- "Give" is two-parameter: **c sets entry softness** (0.5-0.6 band), **d sets total
  travel**. If it feels stiff, the levers are c up or d up; d is what bounds the stretch.

## 6. Wheel/picker specifics: capping hard flicks

Pickers **deliberately cap fling energy**, with unusually concrete constants:

- **Android NumberPicker divides max fling velocity by 8** (SELECTOR_MAX_FLING_VELOCITY_
  ADJUSTMENT = 8): a wheel caps at **1000 dp/s where a list allows 8000**. Snap-to-row
  animation fixed at 300 ms. Velocity cap = implicit travel cap through the decay.
- **Wear OS Compose rotary** (androidx RotaryScrollable.kt): fling velocity over a 30 ms
  window, fires only within 2x that window of the last event, and **scaled by 0.7** (30%
  haircut). Snap mode: **maxSnapsPerEvent = 2 items**, snapDelay 100 ms, new-gesture
  threshold 200 ms.
- **Apple Watch Digital Crown:** linear haptic detents by default; sensitivity tiers.
- **UIPickerView:** no published constants (honest gap). Empirically: fast-style decay +
  row snapping; travel ≈ v x K, so a velocity cap IS a rows-per-fling cap. A hard flick
  does not rail a long picker end-to-end.

**Doctrine: wheels cap velocity (÷8, x0.7) and/or snap count — never letting a single
flick become unaccountable travel.** Railing end-to-end in one flick is treated as a
defect; long traversal is served by input acceleration instead.

## 7. Interactive during the entrance animation

Apple doctrine (WWDC18-803): interfaces must be **responsive, interruptible,
redirectable** — an iPhone X app can be dismissed *during its launch animation*.
Mechanisms (WWDC17-219, WWDC16-216):

- **Input armed during animations** (`.allowUserInteraction`, hit-test the presentation
  layer or final layout).
- **UIViewPropertyAnimator**: pausable, scrubbable, reversible mid-flight.
- On first touch during an entrance: **don't queue the touch, don't block on the
  animation** — retarget from the current presented state (preserving velocity) or
  fast-forward to end state and process against final layout. Never replay the touch
  after the animation finishes.

---

## PRESCRIPTIONS (ranked by expected perceived-latency / feel impact)

1. **Late-latch each tick: poll touch → physics → render → flush.** Freshest input read
   directly before physics. Gain up to ~25 ms worst case, ~10 ms typical — larger than
   the dragging JND (11 ms). Zero cost.
2. **Eager scroll classification + slop-remainder consumption.** Small scroll-axis slop
   (~1.2-1.5 mm ≈ 10-16 px here) purely as jitter gate (do NOT go to zero — bistable
   edge coords); on crossing, re-anchor the drag at the slop boundary so the first frame
   moves only the remainder. Tap classified retroactively.
3. **Catch-then-chain fling model.** Touch during deceleration brakes/stops with no slop
   requirement; same-direction release during/shortly after (≲100-150 ms) adds residual
   velocity (AOSP flywheel, same-sign check), clamped.
4. **Cap wheel fling energy like a picker, not a list.** NumberPicker ÷8 spirit: set
   v_max so one maximal flick travels a bounded, screen-relative distance; optional Wear
   x0.7 haircut on raw hard flicks. Keeps every fling accountable.
5. **Motion LOD + damage minimization during fast scroll.** Above a velocity threshold
   drop the most expensive decorations, restore in the last ~2-3 decay frames. Smaller
   spans → faster partial flushes → never miss the tick. Degrade only what isn't noticed
   at speed; no mid-motion visual *changes* that flash.
6. **Hold the cadence rigidly; never render early or late.** If a frame threatens to
   overrun, drop LOD rather than slip the tick (cadence jitter = velocity flicker).
7. **Rubber band: c = 0.55 entry slope, d = viewport dimension.** Tune softness via c
   (0.5-0.6), travel via d.
8. **Interactive-during-intro.** Arm the touch pipeline before the entrance starts; first
   touch fast-forwards/retargets from the current presented state; hit-test final layout;
   never queue/replay.
9. **Tap feedback within one tick** is perceptually instant (69 ms JND) — don't spend
   complexity there; spend it on 1-3.
10. **Out of scope on this chip:** no position prediction/extrapolation, no stacked input
    filters (prior hardware findings). Slop-as-jitter-gate is the only filter layer.

**Evidence confidence:** AOSP constants quoted from source — high. Apple-side numbers
(c=0.55, ~10 pt pan threshold, UIPickerView behavior) are reverse-engineered/community —
medium (c=0.55 independently corroborated). Motion-LOD as named doctrine — thin.
Chromium fling-boost exact ms gates — not pinned.

Sources: CHI'13 "How Fast is Fast Enough" / CHI'15 "How Much Faster is Fast Enough"
(tactuallabs.com) · AOSP ViewConfiguration.java / ScrollView.java / Scroller.java /
NumberPicker.java · androidx RotaryScrollable.kt · rubber-band gist (originell/6961057) ·
"How UIScrollView Works" (Lobanov) · Apple Scroll View Programming Guide · WWDC18-803 ·
WWDC17-219 · Meta Late Latching · Android Frame Pacing (developer + source.android.com) ·
Texture/AsyncDisplayKit docs · LVGL #6860 · HIG Digital Crown · Apple patents 6865718 &
2005/0097468 · Chromium review 57563007 · Wear rotary input docs
