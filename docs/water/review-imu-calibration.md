# Water — adversarial review: IMU correctness & calibration

Lens: axis→screen mapping tunability, rest-offset calibration soundness,
gravity direction across all four tilts, read-failure tolerance (dead IMU →
down-vector, stale-read reuse). Target: `docs/water/water_draft.rs`,
`docs/water/IMPL-SPEC.md §6`, `docs/water/integration.md §4`.

Verdict: **fix-then-ship**. The mapping is genuinely tunable and the dead-IMU /
stale-reuse paths are largely correct, but two first-live-tick initialization
bugs are real and one of them fires on nearly every open.

---

## What is correct (survived the attack)

- **Axis→screen mapping is genuinely tunable via consts.** `IMU_X_SRC/SGN`,
  `IMU_Y_SRC/SGN` (lines 104–107) feed `axis()` (642–649). The four consts span
  any axis-swap + independent per-axis flip, i.e. all 8 flat-mount orientations.
  Sign is applied identically to gravity (307–308) and jerk (321–322). First-flash
  tuning is a const edit. No defect.
- **Gravity direction is correct for all four tilts, and the jerk sign is also
  physically correct.** Because the accelerometer output is specific force
  (`a_proper − g`), the *same* tuned sign that makes a static tilt run the water
  downhill also makes a linear flick slosh the water the correct (lagging) way —
  they are not independent signs to get wrong. No defect.
- **Rest-bias clamp is sound in principle.** `±REST_BIAS_CLAMP = 820 LSB
  (±0.1 g)` (273–274) bounds the removable offset, so a real tilt (≫0.1 g planar)
  can never be cancelled by calibration; a flat watch still reads in-plane g ≈ 0
  and breathes. Capturing while tilted injects at most 0.1 g of wrong bias — a
  deliberate, bounded trade, acceptable for first-flash. No defect *given the two
  fixes below*.
- **Dead-from-boot fallback is correct and mapping-independent.** Never-live →
  `(0, DOWN_G)` (317) is a hardcoded *screen*-space down-vector, so the pool
  falls to screen-bottom even if the IMU consts were mistuned. Good robustness.
- **Transient-fault stale reuse is correct.** `imu = None` → `acc = last`
  (295), `live = false`; `live || ever_live` keeps gravity from the last good
  vector (311), and jerk = mapped(last) − mapped(last) = 0, so a dropped read
  never jolts or sprays. Good.

---

## Findings

### 1. [medium] A dropped *first* read permanently disables rest-bias calibration

`calibrate()` (269–278) clears `need_calib` unconditionally:

```rust
fn calibrate(&mut self, acc, live) {
    if live { /* capture bias; ever_live = true */ }
    self.need_calib = false;   // <-- runs even when live == false
}
```

`need_calib` is armed only in `open()` (232); nothing re-arms it. So if the very
first Water frame's `read_accel(...).ok()` returns `None` (a transient NACK / bus
glitch on the shared 400 kHz bus right after the scene transition), `calibrate`
runs with `live == false`, captures **no** bias, yet still sets
`need_calib = false`. Calibration is then skipped for the entire session.

Consequence: `bias_x = bias_y = 0` forever, so the sensor's zero offset is read
as a real tilt. Even a modest 0.03 g offset (≈246 LSB → `gx = 246 >> 6 = 3` Q6)
is a constant unopposed accel whose damping-limited terminal velocity is
`3 / (1 − 254/256) ≈ 384 Q6 = 6 px/frame` — the pool creeps and sits pressed
**off-center against the wall** instead of centered at the bottom, defeating the
entire point of the calibration. Only closing and reopening the app (with a good
first read) recovers.

Fix — clear `need_calib` (and set `ever_live`) *only* on a live sample, so a
faulted first tick simply retries next frame:

```rust
fn calibrate(&mut self, acc: (i16, i16, i16), live: bool) {
    if live {
        let bx = IMU_X_SGN * axis(acc, IMU_X_SRC);
        let by = IMU_Y_SGN * axis(acc, IMU_Y_SRC);
        self.bias_x = bx.clamp(-REST_BIAS_CLAMP, REST_BIAS_CLAMP);
        self.bias_y = by.clamp(-REST_BIAS_CLAMP, REST_BIAS_CLAMP);
        self.last_ax = acc.0;              // (see finding 2)
        self.last_ay = acc.1;
        self.last_az = acc.2;
        self.ever_live = true;
        self.need_calib = false;
    }
    // not live: leave need_calib = true and retry on the next frame
}
```

### 2. [medium] First live tick computes jerk against `last_* == 0` → spurious spray on (almost) every open

`last_ax/ay/az` start at 0 (216–218) and `open()` never reseeds them. In `tick`
the jerk is computed from `last_*` (321–325) **before** `last_*` is updated
(326–328). On the first live tick after open, `last_* == 0`, so:

```
jx = rmx − 0 = rmx,  jy = rmy − 0 = rmy,  jmag = |rmx| + |rmy|
```

Opening the app while the watch is tilted — the *normal* case, since you tilt the
watch to look at it — makes `rmx/rmy` a large fraction of 1 g. At a 45° view
angle, in-plane g ≈ 0.7 g ≈ 5800 LSB on one axis, so `jmag ≈ 5800 > JERK_TH =
2600`. And `ever_live` was just set true by `calibrate` earlier in the same tick,
so the spray gate `self.ever_live && jmag > JERK_TH` (329) **fires**: surface
particles get flung up/out (`vy -= jmag >> 8`, 377) — an unwanted spray burst the
instant the reveal completes on a calm pool. It self-corrects in ~10–20 frames
(VMAX + damping), but it fires on essentially every open and reads as a glitch,
undermining the premium feel. Reopening is worse: `last_*` still holds the
*previous* session's sample, so the jerk is against a stale, possibly very
different orientation.

Fix — seed `last_ax/ay/az = acc` at the first live calibration (folded into the
`if live` branch above). Because `calibrate` runs before the jerk block in the
same tick, the first `lrmx/lrmy` then equal `rmx/rmy` and the first jerk is
exactly 0. This also fixes the stale-reopen jerk, since `open()` re-arms
`need_calib`, forcing a fresh reseed on the next live tick. (No standalone edit
to the jerk block is needed.)

### 3. [low] Permanent IMU death *after* being live freezes the pool at the last tilt instead of settling down

Once `ever_live` is true, the gravity branch `live || self.ever_live` (311) is
always taken, so a permanently-dead IMU (every read faults from here on) reuses
the **last good vector forever** — never the `(0, DOWN_G)` down-vector. If the
watch was tilted when the IMU died, the pool runs to that side and stays there,
which is arguably worse than falling to screen-bottom. This matches the spec's
stated intent (only *never-live* uses the down-vector; transient faults reuse
last) and there is no way to distinguish a long transient from a permanent death
without a counter, so it is not a hard defect — but a small consecutive-fault
counter that snaps gravity to `(0, DOWN_G)` after, say, ~1 s of continuous
failures would be strictly more robust and cost ~2 bytes of state.

---

## Note (not a defect)

`WATER-APP-PLAN.md §2` calls for capturing "a few samples flat" for the rest
offset; the implementation captures a **single** sample on the first live tick.
The `±0.1 g` clamp bounds the resulting error and makes the single-sample choice
acceptable, but if on-device tuning shows jitter in the captured bias, averaging
4–8 samples before latching is the low-risk upgrade (and pairs naturally with the
finding-1 retry loop).
