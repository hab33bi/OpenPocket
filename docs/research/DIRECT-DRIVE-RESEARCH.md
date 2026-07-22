# Direct-Manipulation Scroll Tracking on Sparse/Bursty Touch Streams

Follow-up to TOUCH-PIPELINE-RESEARCH.md. Trigger: measured CST9217 defect — at slow
finger speeds the chip stops producing reports entirely, then bursts; classification
(10 px slop) arrived with 200-360 px of accumulated travel, and slop re-anchoring
discarded it ("screen-length slow drag moves one row").

## 1. Anchor-at-touch-down vs per-event deltas; burst catch-up

**The anchor/absolute form is canonical and strictly more robust than delta
accumulation.** iOS `UIPanGestureRecognizer.translation(in:)` is cumulative
displacement since gesture begin — anchor-at-origin absolute. A dropped or coalesced
report changes nothing: the next report's absolute position fully re-specifies content
position. Android is delta-based at the API surface (`onScroll` signed distanceX/Y)
but deltas are computed from absolute positions — a gap produces one large delta and
content still lands exactly where the finger is. **Platforms never let content lag the
finger during tracking; after a gap, content jumps to the finger on the next event.**
Catch-up smoothing lives one layer down, in input resampling:

- **Android InputTransport.cpp**: RESAMPLE_LATENCY 5 ms, MIN_DELTA 2 ms,
  **MAX_DELTA 20 ms (resampling disabled entirely across bigger gaps)**,
  MAX_PREDICTION 8 ms and ≤50% of last inter-sample delta.
- **Flutter resampler**: samplingOffset −38 ms, **interpolation-only** — when sample
  time passes the newest real event, position CLAMPS to the last real position. No
  prediction ever.

Reading for this hardware: platform resampling is a micro-jitter tool (~7-13 ms phase
errors), not a gap-repair tool. Flutter's model is the precedent: never predict; smooth
only between two REAL samples; smoothing feeds rendering only.

## 2. Mid-gesture direction reversal

Confirmed signed/cumulative on both platforms; scroll back past origin within one
gesture is native behavior. Gotchas: slop is radius-from-anchor, not path length;
slop never re-arms on reversal; Android subtracts slop from the first delta.

## 3. Release velocity from sparse/bursty samples (AOSP VelocityTracker.cpp)

- **HORIZON = 100 ms** — samples older than that (vs newest) discarded.
- **ASSUME_POINTER_STOPPED_TIME = 40 ms** — a gap bigger than this before lift clears
  history: velocity is zero from a standstill. (Exactly the chip-quiet scenario;
  stretch to ~50 ms for 10 ms poll granularity, not beyond ~2x healthy interval.)
- **Impulse strategy** introduced precisely for few/irregular samples (pairwise finite
  differences, kinetic-energy model); LSQ2 on 2-3 irregular samples is unstable.
- **Minimum time-delta constraint**: samples can arrive ~0.5 ms apart (FIFO flush) —
  divide-by-tiny-dt explodes velocity; clamp dt (~5 ms floor).
- **Resampled/smoothed samples are excluded from velocity** — raw reports only.

## 4. Tap discrimination on a direct-drive surface

Android: TOUCH_SLOP 8 dp (~1.27 mm), TAP_TIMEOUT 100 ms, PRESSED_STATE 64 ms; hover
slop = slop/2 exists as a smaller "intent" threshold. iOS: delaysContentTouches +
canCancelContentTouches = "deliver provisionally, cancel retroactively". Synthesis for
a direct surface (not verbatim shipped behavior, each half is): **~0.5 mm render gate
(jitter only) + ~1.2 mm retroactive tap radius at lift** (displacement radius from
anchor, not path length), duration as secondary guard (~350 ms).

## 5. Session robustness: dropout mid-gesture

ACTION_CANCEL / touchesCancelled semantics: aborted gesture commits NOTHING (no tap,
no fling); content stays at last tracked position; a new DOWN is a NEW gesture with a
new anchor — no platform attempts positional continuity across a stream death (a
bridged anchor would produce a violent synthetic scroll).

Controller family: CST816S documented quirks (I2C sleeps until touch, no touch-up IRQ,
invalid coords in lift reports; fixes via IrqCtl 0xFA EnTouch/EnChange/EnMotion,
MotionMask 0xEC, DisAutoSleep 0xFE). **For CST9217 specifically no public datasheet
documents the slow-movement report suppression or a motion-threshold register** —
firmware must be defensive. LVGL polled-indev contract (the load-bearing embedded
precedent): **silence while the chip still says "touched" means "finger stationary at
last position", not "gesture over"** — the driver synthesizes continuity.

## 6. Gearing

Direct touch is 1:1 everywhere, including watches (UIPickerView tracks ~1:1; Apple
Watch/Wear touch scrolling 1:1). >1:1 gain is reserved for INDIRECT input (crown
sensitivity tiers, rotary degrees→dp scalars). No shipped precedent for >1:1 finger
gearing on a small touchscreen; gain breaks the stick-to-finger contract and amplifies
sensor noise. Reach is solved with flick momentum, not tracking gain.

## PRESCRIPTIONS (ranked) — implementation status in parentheses

1. Absolute anchor from first report; signed; never accumulate deltas. (SHIPPED —
   wheel_direct anchor_y/anchor_s.)
2. Burst catch-up: snap or short-lerp ≤~40 ms between real samples only; no
   extrapolation; smoothing feeds rendering only. (SHIPPED — 3/4 pursuit ≈2-3 frames;
   velocity reads raw ring, not smoothed s.)
3. Velocity: impulse-style on raw samples; HORIZON 100 ms; assume-stopped ~40-50 ms;
   per-pair dt floor ~5 ms. (SHIPPED — STALE_MS 50, DT_FLOOR_MS 5, 2-segment
   recency-weighted hotter-of.)
4. Jitter gate ~5-6 px render gate + ~13 px retroactive tap radius + ~350 ms duration
   guard; gate subtraction on crossing. (SHIPPED — GATE_PX 6, TAP_RADIUS_PX 13.)
5. Dropout: chip-touched silence = stationary (keep session); true loss = CANCEL,
   commit nothing, settle nearest; resume = new gesture, new anchor. (SHIPPED — via
   recognizer liveness + Rest verdict on stale.)
6. Gearing 1:1; fix reach with fling/detents, never tracking gain; if ever trialed,
   ≤1.5x and after the gate. (SHIPPED as 1:1.)

Sources: AOSP VelocityTracker.cpp / InputTransport.cpp / ViewConfiguration.java ·
Compose VelocityTracker.kt · Flutter gestures binding.dart + resampler ·
UIPanGestureRecognizer setTranslation docs · Zanella "Replicating UIScrollView" ·
delaysContentTouches / canCancelContentTouches · Android ViewGroup ACTION_CANCEL docs ·
lupyuen CST816S NuttX notes + DS-CST816S V1.3 · esphome-cst9217 · ESP32_Display_Panel
PR #248 · Wear rotary docs · digitalCrownRotation docs.
