# Water — tilt-driven liquid simulation (plan)

Status: PLAN, ready for an **ultracode** implementation pass. The
foundation is already in place and hardware-verified:

- `drivers/qmi8658.rs` — the 6-axis IMU driver (accel ±4g @ 1000 Hz,
  8192 LSB/g). Boot probe confirms **WHO_AM_I = 0x05** on this board, so
  the gravity source is real and reachable on the shared 400 kHz bus.
- Wheel row **7 = Water** (Lucide `waves`, neon-blue accent). Currently
  rests on its splash as a placeholder (`has_content(WATER) == false`).

The deliverable: a premium tilt-responsive liquid of **tiny neon-blue
squares** that pools at the bottom of the round face, sloshes as you tilt
and shake the watch, and (optionally) shimmers with the OS aurora.

---

## 1. The vibe

A shallow pool of luminous cyan fills the lower part of the round screen.
Tilt the watch and the liquid runs downhill and climbs the far wall; flick
it and a wave breaks and throws spray; hold it vertical and it all pours
to one side, the surface settling with a few bobbing ripples. Particles
are small filled squares (3–4 px) in the signature neon blue, brighter at
the crests, deeper/darker in the body, with a faint aurora drift through
the mass and a bright meniscus line where it meets air. Black AMOLED
behind — the liquid glows.

Signature animation: the liquid is *always* alive (unlike the still
template apps) — even at rest it has a slow breathing ripple, and it
reacts to the IMU every frame.

---

## 2. Reading gravity (QMI8658)

Per frame, in the run loop (which owns `self.i2c`), read the accelerometer
and hand a gravity vector to the Water tick:

```
let (ax, ay, az) = qmi8658::read_accel(&mut self.i2c)?;   // raw i16, 8192 = 1g
```

- **Axis → screen mapping is unknown until measured on-device** (the touch
  Y-axis needed a flip; the IMU's mounting orientation relative to the
  panel must be found the same way). Plan: a short on-device calibration —
  print `ax, ay, az` while tilting in each of the 4 screen directions,
  pick the two in-plane axes and their signs so "tilt top-edge down" →
  gravity points to screen-top, etc. Bake the mapping as a small const
  (like the touch driver's flip).
- **Rest calibration**: on app open, capture a few samples flat and store
  the zero offset (the QMI8658 has a small bias; the ball game calibrates
  for exactly this). Subtract it. A "recalibrate" affordance (long-press
  or a tap in a corner) is nice-to-have.
- **Gravity vector for the sim**: `g_screen = (gx, gy)` in the screen
  plane, magnitude ≤ 1g normally; clamp/scale to a fixed-point per-frame
  acceleration. The z axis (into the screen) is unused for a 2-D pool but
  its magnitude tells you "watch is flat vs upright" — handy to modulate
  splashiness.
- **Shake / slosh impulse**: the frame-to-frame change in accel (jerk)
  injects velocity into the particles — this is what makes a flick throw
  spray. Optionally fold in gyro (angular velocity) for rotational slosh.
- Read cost: one 6-byte I2C read (~0.2 ms). Trivial. Read once per frame;
  if a read errors, reuse the last vector (or fall back to a fixed
  down-vector so the app still works with a dead IMU).

---

## 3. Simulation model — recommended: **particle + coarse grid (PIC-lite)**

The user asked for "tiny squares", so a **particle system** (not a pure
grid CA) is the right visual. Pure ball-physics looks like bouncing dots,
not liquid; pure SPH needs float + expensive neighbor kernels. The sweet
spot on this CPU is a **particle system with a coarse background grid for
incompressibility/repulsion** — the PIC/FLIP idea, stripped to integers:

**Particles** (target **~250–400**, tune to frame budget):
- State per particle: `x, y` (Q4 or Q8 fixed-point px), `vx, vy` (fixed).
- Integrate: `v += g_screen·dt + slosh_impulse; v *= damping; pos += v·dt`.

**Coarse grid** (e.g. 24×24 cells over the 466² face, ~20 px cells):
- Each frame, scatter particles into cell density counts.
- Where a cell is over-full, push its particles toward lower-density
  neighbours (a cheap pressure/repulsion pass) — this is what makes the
  mass behave as an incompressible fluid that *spreads and levels* instead
  of piling into a point. 1–2 relaxation iterations is enough for a UI.
- Grid also gives O(1) neighbour lookup if you prefer explicit short-range
  repulsion between nearby particles.

**Boundary — the round wall** (radius ~226):
- If a particle is outside the circle, project it back to the rim and
  reflect its velocity with damping (energy loss = no perpetual motion).
- The meniscus/surface emerges naturally from the density gradient at the
  top of the mass.

All fixed-point, no FPU. Budget: ~350 particles × (integrate + boundary +
one grid pass) ≈ a few tens of thousands of integer ops/frame — well
inside a 25 ms frame. The cost driver will be **rendering + flush**, not
physics.

> Alternative if particles-as-liquid proves too "grainy": a **height-field
> / shallow-water** column model (N columns, water heights, flow between
> neighbours under gravity) renders as a filled wave and is very cheap, but
> it can't splash/throw droplets or go past ~vertical. The particle model
> is the more impressive, more general choice and matches "tiny squares".
> Keep height-field as the fallback.

---

## 4. Rendering — neon squares, damage, flush

- Draw each particle as a 3–4 px filled square in neon blue. **Brightness
  by speed/height**: fast/crest particles brighter (near-white core),
  settled/deep particles a deeper cyan — sample a build-time **water LUT**
  (deep indigo → cyan → white) indexed by a per-particle intensity, so the
  whole look is one gradient (same doctrine as the saber LUT).
- **Aurora option**: reuse the ring's periodic-noise idea — offset each
  particle's LUT index by a slow noise sampled at its position, so a
  luminous drift moves through the body. Cheap, and ties Water into the OS
  visual language.
- **Meniscus**: a brighter 1 px rim on the topmost particles (surface
  cells in the grid) reads as a light-catching water line.
- **Damage/flush reality**: the liquid occupies a large, fully-dynamic
  region → this is a **full-frame-flush app** (~13 ms) every frame, like
  the Gallery. So realistically **~40 fps** with physics + render inside
  the remaining ~12 ms. That's fine and matches the rest of the OS. If we
  want headroom, the **perf-plan P1 async-flush** (docs/PERF-DUALCORE-PLAN)
  would roughly double it — Water is the app that most wants P1.
- Render technique: clear the pool's bounding region (or the whole round
  interior) and redraw all particles each frame — simplest and, since it's
  full-frame anyway, no damage-tracking cleverness needed. Keep the status
  clock (topmost) and let the liquid live below it.

---

## 5. Integration into the scene machine

- `has_content(WATER) → true` once built.
- The Water tick needs the gravity vector, which requires `i2c`. Two
  options:
  1. **Read in the run loop, pass into the tick** — add a `grav: (i32,i32)`
     (and maybe `jerk`) parameter threaded to `apps::tick` for the Water
     branch only. Cleanest; keeps `apps` I2C-free.
  2. Give the Water sim a dedicated interactive loop like `gallery_interact`
     (owns the frame, reads IMU, steps, renders, yields on touch) — better
     if we want a higher/independent cadence for the liquid.
  Recommend **option 1** first (uniform with the other ticks), escalate to
  option 2 if the liquid wants its own faster loop.
- Simulation **state** (particle array, grid, calibration offset, RNG,
  last-accel) lives in `apps::State` (it's already the per-app state home).
  ~400 particles × ~12 bytes ≈ 5 KB — fits internal SRAM comfortably; keep
  it OUT of PSRAM (per-frame random access).
- Open/close: Water is a content app — the splash `waves` logo flies in,
  the pool fills in on the content reveal (particles rain in / the level
  rises with `q`), and on close the liquid drains as the logo returns.
- Idle: the liquid keeps ticking while Awake; dims/relocks on the normal
  ladder. Optionally freeze the sim when Dim to save power.

---

## 6. Build-time assets

- `WATER_LUT`: 256-entry deep-indigo → cyan → neon-white gradient in
  RGB565-BE (build.rs, like the saber LUT), Bayer-dithered at blit time.
- Reuse the existing `NOISE_A/B` periodic noise for the aurora drift.
- No new sprites (squares are procedural). The `waves` icon is already
  vendored and rasterized at all sizes.

---

## 7. Suggested ultracode build order

1. **IMU calibration + axis mapping** on-device: log accel, tilt in 4
   directions, bake the screen-plane `(gx, gy)` mapping + rest-offset
   capture. (De-risks the one true unknown.)
2. **Particle core**: array in `State`, gravity integrate, round-wall
   bounce, render as flat squares. Prove it pools and tilts.
3. **Incompressibility**: coarse density grid + 1–2 relaxation passes so it
   spreads and levels like liquid, not a heap.
4. **Slosh/spray**: jerk impulse from accel delta (and/or gyro) → waves
   break and throw droplets on a flick.
5. **Look**: WATER_LUT (speed/height shading), meniscus rim, aurora drift.
6. **Polish**: fill-in on open / drain on close, rest breathing ripple,
   optional recalibrate gesture, freeze-when-dim.

## 8. Risks / decisions to confirm

- **Particle count vs fps**: start ~300, measure; the flush floor caps us
  near 40 fps regardless, so spend the CPU budget on physics quality.
- **Axis mapping / calibration** is the only genuine unknown — everything
  else is standard integer particle work. The boot probe already proved
  the chip talks.
- **Incompressibility method**: coarse-grid pressure (recommended) vs
  explicit pairwise repulsion (simpler, grainier). Pick during step 3 by
  eye.
- **Gyro**: optional; accel alone gives a great tilt liquid. Add gyro
  slosh only if step 4 wants more life.
- This app most benefits from **perf-plan P1** (async flush) — worth
  sequencing P1 near the Water work if 40 fps feels tight.
