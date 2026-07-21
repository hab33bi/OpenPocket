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

## M5 — Burn-in mitigation & power (deferred, from the old plan)

Idle dimming, subtle content shift for static elements, AOD-style minimal mode (HH:MM, 1 update/min, pixel offset), display sleep, reduced animation when idle.

## M4 status (2026-07-21): implemented, responsiveness fix pending

The sheet composer, drag sessions, settle animation, swipe-down relock, and
60 s auto-relock are implemented and flashed. **User verdict: drag grab is
sluggish/glitchy — sometimes doesn't engage — while the old tap-to-switch felt
instant.**

**Diagnosis (to execute next):** touch reads are edge-triggered on the INT pin
with a 20 ms level fallback. During a drag, composes+flushes block the CPU for
5–25 ms at a time; the CST9217's short INT pulses land inside those windows and
are missed, so reports (and therefore sheet updates) arrive erratically. The
M3 tap path never needed a report *stream*, which is why it felt fine.

**Fix plan:**
1. While the recognizer is in Pending/Dragging (finger down), read the chip
   **unconditionally on a timer** (~every 10 ms, latest-wins) — no INT gating.
   Edge-gating remains only for the Idle phase (bus hygiene at rest).
2. Consider: arm zone bottom 25% → ~37% ("fairly large segment of the bottom
   edge"), slop 14 px → 10 px for a faster grab, and emit the first DragMove
   together with DragStart so the sheet moves on the very first classified
   sample.
3. Measure with the drag log line (`drag b= compose= flush= spans=`): target
   compose+flush < 20 ms typical and report intervals ≤ 20 ms during movement.

## Polish backlog (near-term, after M4)

- **Font anti-aliasing upgrade**: current text uses 1-bpp glyphs + 2×2 box sampling
  (5 alpha levels) and the time is a 3× nearest-neighbor upscale — edges are steppy.
  Plan: rasterize each rendered size directly in build.rs with fontdue's native
  8-bit antialiasing (time glyphs at final pixel size, date at its size), store
  4-bit alpha atlases in flash, draw with full alpha blending and no runtime
  scaling. Smoother digits, no upscale blockiness, modest flash cost (~tens of KB).

## M6 — The App Wheel (deferred until last; plan to the T before starting)

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
