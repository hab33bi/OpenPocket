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

## Later / parked

- Pipeline overlap (render ‖ flush on core 1) — scaffolding removed; re-add when scenes outgrow the frame budget
- Runtime JPEG decoding (SD card image sources)
- NTP time sync (needs Wi-Fi), timezone/DST handling
- IMU (tilt-to-wake), audio, SD — all out of scope until the lock/unlock experience is polished

## Performance bar (unchanged)

Touch-to-visible < 50 ms; animation ≥ 20 fps stable (currently 20 fps locked); every milestone logs frame/flush/touch timings over serial; no "smooth" claims without numbers plus a human observation note.
