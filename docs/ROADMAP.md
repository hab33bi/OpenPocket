# OpenPocket Roadmap

Adapted 2026-07-07 from the original watch-OS plan (`archive/Outdated-plan.txt`). What changed against that plan:

- **Dropped: the fluid simulation** (old Screen B / Milestone 5). Replaced by a fullscreen image reveal — the goal it served (proving touch latency, frame pacing, responsiveness) is now carried by the swipe-to-unlock transition itself.
- **Dropped: Embassy async migration, LVGL/Slint tracks, host preview requirement** — the custom blocking renderer is proven; these stay out unless a concrete blocker appears.
- **Kept**: premium/dark AMOLED art direction, measurement culture ("do not claim smooth without numbers"), safety rules, burn-in mitigation (deferred milestone), module boundaries (in lightweight form).

## Product shape

A pocket watch. **Lock screen** = the existing clock (time, date, animated bezel ring). **Swipe up from the bottom** with a touch-tracked grabber unlocks it, revealing a fullscreen image (Spike, from Cowboy Bebop), clipped by the round panel. **Swipe down** (or 60 s timeout) re-locks. Touch must feel extremely responsive: target < 50 ms touch-to-render, measured honestly.

## Done ✅ (hardware-verified 2026-07-07, see archive/08 for full journals)

- Retained-canvas display stack: `WatchFb` + `DmiIndex` + partial windowed DMA flush (2-px alignment), flush ~0 ms during animation, 24 ms full-frame
- Lock-screen clock: Inter Display Bold scale-to-gray AA, bezel ring with build-time cubic-Bezier schedules, rounded faded comet-tip arc ends, seam heal/unheal, minute-change undraw/redraw cycle
- Fixed 20 fps cadence, all phases within budget (worst frame 41 ms); idle frames cost ~0 (flush skipped)
- Repo pivot: OpenPocket rename, dead experiment code removed, docs restructured

## M1 — Real time (PCF85063 RTC)

- `drivers/pcf85063.rs`: BCD time registers 0x04–0x0A, VL (clock-integrity) flag
- Flash-time seeding: build.rs embeds `BUILD_UNIX_LOCAL` (host **local** time + ~40 s flash fudge); firmware seeds the RTC only when VL is set or RTC < build time — the RTC keeps time across reflashes while powered
- `time.rs`: RTC-anchored wall clock (base + `Instant` elapsed, hourly re-anchor); lock screen shows real HH:MM and a dynamic date line ("July 7th 2026" style with ordinal suffix)
- build.rs font subset expands programmatically from the month-name/suffix tables
- Verify: boot log `RTC: VL=.. read=.. build=.. action=kept|seeded`; power-cycle without reflash keeps time

## M2 — Touch bring-up (CST9217)

- `drivers/cst9217.rs`: port init/reset/report-read from SensorLib `TouchDrvCST92xx` (MIT, Waveshare BSP) — chip-ID read as go/no-go gate; TP_RESET pulse timing verbatim
- Polling model (no ISR): the frame loop's cadence wait doubles as an INT-pin poll loop; INT asserted → I2C report read (~0.5 ms) → latest-report-wins
- `input/gestures.rs`: touch-down/track/release recognizer with drag distance + release velocity; swipe-up arms only from the bottom ~25% of the panel
- Serial-only milestone: raw report dumps, then gesture state logs; verify coordinates track the finger (axis orientation check) and the 20 fps cadence is undisturbed

## M3 — Unlock image + scene skeleton

- build.rs asset pipeline: decode `assets/Spike.jpg` (1191×1200) at build time, center-crop → 466×466, Bayer 4×4 ordered dithering → RGB565-BE static in flash (~424 KB of 16 MB)
  - *Rejected alternative*: runtime JPEGDEC-style decoding — buys nothing for a single fixed asset; revisit if images ever come from SD card
- `scenes/unlocked.rs`: fullscreen blit (round clip is the physical panel)
- `app.rs`: scene state machine `Locked | Dragging | Unlocking | Unlocked | Relocking` — this milestone wires Locked ⇄ Unlocked with a temporary tap toggle
- Verify: image quality/dither acceptable (user), idle frames back to 0 ms in Unlocked

## M4 — Swipe-to-unlock

- Drag: black sheet + time/date translate with the finger; image static beneath, revealed from the bottom; **ring fades out scrub-tracked** — fade proportional to drag distance, quantized to 16 levels (most drag frames stay partial-flush cheap ~5–15 ms; level-change frames accept a 24 ms full flush)
  - *Rejected alternative*: translating the ring with the sheet — its curved shape dirties nearly every row → permanent full-flush frames
  - *Experiment parked*: CO5300 hardware vertical scroll (0x33/0x37) — unknown semantics on this round panel
- During drag the fixed cadence is dropped: render-on-touch-move
- Release: dy > H/3 **or** velocity > ~0.5 px/ms → ease-out unlock completion (new build.rs schedule, ~12 frames); otherwise spring back
- Re-lock: swipe down from the top in Unlocked (mirror transition) + 60 s auto re-lock; ring heals back via the existing Redraw/Heal phases
- Verify: serial `drag dy= compose= flush= spans=` — < 20 ms typical, < 50 ms worst; user judges grabber tracking and spring feel

## M5 — Burn-in mitigation & power ✅ (2026-07-21)

Idle ladder, any touch wakes instantly from every stage (the cadence wait
loop polls touch continuously, so relaxed frame rates cost no latency):

- **30 s → Dim**: brightness ramps to 0x38 (reg 0x51, −8/frame); the
  minute-change ring sweep is suppressed while dimmed (text still updates).
- **2 min → AOD** (Locked only; 60 s auto-relock always runs first): black
  canvas + HH:MM only at brightness 0x18, redrawn once per minute at a
  minute-indexed pixel drift (8 positions, ±6 px) to spread AMOLED wear;
  ring off entirely; 500 ms cadence.
- **10 min → Sleep**: display off (0x28) + sleep-in (0x10). Wake: sleep-out
  (0x11) + 120 ms settle → repaint → display on (0x29) → full brightness.
- Waking from AOD/Sleep repaints the lock scene before any gesture renders,
  so a swipe straight out of AOD unlocks correctly.

## Arc rendering v2 (2026-07-21, user-directed — supersedes "arc frozen")

Per-pixel radial alpha map: solid core → ~1.75 px antialiased stroke edge.
All ring paths (sweep stamps, static blit, scrub-fade runs) share the map +
a 256-entry intensity LUT, so appearance is path-independent. Ease deepened:
startup (0.45,0,0.25,1), minute (0.4,0,0.2,1); seam heal 250 ms.

- **Glow: tried and removed** — invisible against the AMOLED black floor at
  tasteful intensity, and its wider taps cost ~73% more sweep work. Revisit
  only with a fundamentally different technique (e.g. blur pass).
- **Sweep runs at 40 fps** (build-time schedules at TARGET_FPS 40; the app
  switches to a 25 ms cadence while `Clock::is_animating`, 20 fps static).
  Fixed the visible stepping at the ease curve's speed peak. Comet-tip
  window restamps at stride 2 (still ~13 steps/px, gap-free) to fit the
  budget: worst sweep frame 24 ms, typical 8–14 ms, idle 0 ms.

## M4 status (2026-07-21): direction + polish fixes applied

The sheet composer, drag sessions, settle animation, swipe-down relock, and
60 s auto-relock are implemented and flashed.

**The "sluggish/glitchy grab" root cause was NOT latency** (the earlier
INT-starvation diagnosis is superseded): the CST9217's **Y axis is inverted
relative to the panel** (raw y=0 = physical bottom). The user's physical
bottom-up swipes landed in the code's *top* arm zone and classified as
swipe-*down*, which Locked ignores — while a physical top-down swipe unlocked
(and, notably, felt responsive, clearing the latency theory). Fixed in
`drivers/cst9217.rs::read_touch`: Y flipped once at the driver boundary so all
consumers see display coordinates. X orientation unverified (nothing
direction-sensitive uses it yet — check with corner taps before M6).

**Also fixed (user-reported):**
- Black box punched out of the ring by the sliding digits during relock: the
  compose order was ring-then-text-erase; the erase (a black rect at the old
  text spot) clobbered freshly drawn ring pixels. Reordered erase → ring →
  text, with `rect_touches_ring` forcing a ring repaint when the erase reaches
  the annulus while the ring is visible.
- Magnetic snap (user-confirmed model): release verdict is now **50% of the
  screen** (was 25%) — past half always completes, under half always retracts,
  nothing rests midway — **or a quick flick** (vel > 0.5 px/ms) completes
  regardless of distance. Finger-tracked 1:1 while down; decision at release.

**Round 2 (same day) — release freeze + relock flicker:** serial showed
`vel_q8=0` releases: lift-off INT pulses were being missed inside blocking
compose/flush windows, so releases waited on the 1.5 s fallback timeout
(felt as "sheet freezes near the end, then jumps"). And every relock frame
was a 24 ms full flush because the ring pass repainted from row 0. Fixes:
- Finger down → touch reads on a fixed ~10 ms timer (no INT gating); INT
  edge-gating remains only while idle.
- Ring pass repaints only the rows that changed (fresh band rows / erased
  text rows / all rows only on a fade-level step; skipped entirely at level
  0) — most drag and settle frames are partial flushes again.
- Settle's final frame flushes together with the end-of-transition
  normalize (was two back-to-back full flushes = visible flicker).
- Arm zones widened 25% → 37.5%, slop 14 → 10 px, and the sheet moves on
  the very first classified sample (DragStart carries its initial travel).
- Glyph renderer clips rows before coverage sampling (sliding offscreen
  text was paying full rasterization cost).

## Drag smoothness backlog (ideas, not scheduled — M4 feel is accepted)

Current state: composes ~1–10 ms (ring recolor via runs), partial flushes for
most drag frames, touch read every ~10 ms while the finger is down. Ideas in
rough order of expected payoff if we revisit:

1. **Compose cap 16 ms → ~10 ms** (`COMPOSE_MIN_US`): composes are cheap now;
   a 60→100 Hz render-on-move cap tightens finger tracking. One-line change,
   measure first that fade-zone frames stay under the cap.
2. ~~**Target smoothing**~~ — DONE 2026-07-21 (k=0.5 per compose), together
   with edge-travel mapping: the reachable finger travel (touch-down → panel
   edge) maps onto the full sheet travel, fixing the ~90% stall/stagger on
   slow drags; release verdict now evaluates sheet travel, not finger px.
3. **Ring damage as per-run row spans**: the fade recolor currently marks the
   annulus bounding rect (~60% of the screen → full flush on level-step
   frames). Marking each run's row span instead keeps level-step frames
   partial — fewer full-frame writes racing the panel scan.
4. **More fade levels** (16 → 32): recolor is ~1 ms now; finer quantization =
   visibly smoother scrub fade for the cost of more (cheap) level frames.
5. **Velocity-carried settle**: seed the settle animation with the release
   velocity (start fast, decay) instead of a fixed 3/8 exponential — the
   sheet keeps the finger's momentum through the handoff.
6. **Pipeline overlap** (parked in "Later"): compose on core 0 while core 1
   flushes the previous frame — halves effective frame time if scenes ever
   outgrow the budget.

## Polish backlog (near-term, after M4)

- **Font anti-aliasing upgrade**: current text uses 1-bpp glyphs + 2×2 box sampling
  (5 alpha levels) and the time is a 3× nearest-neighbor upscale — edges are steppy.
  Plan: rasterize each rendered size directly in build.rs with fontdue's native
  8-bit antialiasing (time glyphs at final pixel size, date at its size), store
  4-bit alpha atlases in flash, draw with full alpha blending and no runtime
  scaling. Smoother digits, no upscale blockiness, modest flash cost (~tens of KB).

## PWR button features (2026-07-21, research done)

`docs/research/BUTTONS-RESEARCH.md` has the register-level plan (poll AXP2101
reg 0x49 per frame — no IRQ GPIO on this board; battery % at 0xA4; never
touch 0x22/0x27/rails). Behaviors: **Locked + PWR → lightsaber ring
flourish** (a ~1 s premium ignition animation, REAL glow — pre-rendered
intensity, AMOLED-aware, unlike the removed subtle ring glow); **Unlocked →
App Wheel** navigation (open/back per M6 plan). Also pending: **unlock
end-of-travel lag** fix (suspect: damped-tracking tail near b=0).

## M6 — The App Wheel (planned to the T — see docs/M6-APPWHEEL-PLAN.md)

Concept (see design reference, 2026-07-21): an app switcher as a **right-aligned
vertical free-scroll carousel** hugging the round display's edge.

- Each row = app label (left of icon) + icon; the row's right edge follows the
  **circle's chord at that row's y** — rows at the vertical center reach furthest
  right; rows above/below indent inward, so the column visually wraps the bezel.
  The indent must be recomputed continuously during scrolling so rows glide along
  the curve (scrub-tracked, never stepped).
- Focused row (vertical center): larger bright label + largest icon (reference:
  "Activity"). Neighbors: smaller, dimmed, indented per curvature. Scale/alpha
  interpolate smoothly with scroll position — no discrete focus jumps.
- Free scroll with momentum + snap-to-row settle (exponential ease-out, matching
  the unlock animation's feel). Status line top-center (time | battery).
- v1 apps are placeholders (Phone, Messages, Activity, Settings…); tapping any
  row just returns to / re-centers the wheel — the wheel itself is the deliverable.
- Perf plan: rows are icon+text sprites composited onto black; per-frame damage =
  the rows' bounding band (partial flush); target the same 20 fps cadence with
  render-on-touch-move during drags. Icons as build.rs-generated RGB565 sprites.

## Storage stage (when needed)

The board has a **TF (microSD) slot** (pins in HARDWARE.md) and a 32 GB card is
on hand. Bring up SDMMC when flash capacity or dynamic content demands it —
candidates: image galleries (with runtime JPEG decoding), app assets, logs.

## Later / parked

- Pipeline overlap (render ‖ flush on core 1) — scaffolding removed; re-add when scenes outgrow the frame budget
- Runtime JPEG decoding (TF-card image sources)
- NTP time sync (needs Wi-Fi), timezone/DST handling
- IMU (tilt-to-wake), audio — out of scope until the lock/unlock experience is polished

## Performance bar (unchanged)

Touch-to-visible < 50 ms; animation ≥ 20 fps stable (currently 20 fps locked); every milestone logs frame/flush/touch timings over serial; no "smooth" claims without numbers plus a human observation note.
