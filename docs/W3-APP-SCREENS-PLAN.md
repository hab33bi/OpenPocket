# W3 — App Screens: High-Fidelity Design Plan

Scope set by user (2026-07-22): **no Bebop "surprise" — Bebop lives in the Gallery.**
Every other app gets an elegant placeholder/template, designed to high fidelity.
This document is the design contract; implementation follows user sign-off.
Parent contract: docs/M6-APPWHEEL-PLAN.md (esp. §11 unlock time-morph).

---

## 1. Design language (shared by every screen)

- **Canvas**: pure AMOLED black. All content fitted to the r=233 circle with the
  wheel's worst-corner chord math — nothing ever clips the round boundary.
- **The clock never moves.** After the unlock morph lands the time at the status
  slot (CX, y≈52, TEXT_GLYPHS), that small clock persists on the wheel AND inside
  every app — the one fixed point of the whole UI. Battery joins it as today.
- **App title**: small spaced caps (TEXT_GLYPHS), y≈92, screen-centered, in the
  app's accent color at ~60% intensity. Quiet — the content is the hero.
- **Palette**: ice-blue base (200,215,255) for all text; saber azure↔violet for
  focus/glow; ONE restrained accent per app (below). Gradients always Bayer-
  dithered on lit pixels. Glows are layered quadratic inner glows (lock-arc tech).
- **Hero zone**: y 120–360. **Footer zone**: y 380–430 (page dots, ghosted hints).
- **Honesty rule**: no fake radios. Mock content is presented as *content*
  (named, curated, in-world), never as live data pretending to work.
- **Motion**: every app has exactly ONE cheap signature animation (partial-flush
  rect, ≤2 ms), breathing at rest like the wheel's ring. Everything else is still.
- **Input**: PWR = back to wheel. Top-edge swipe-down = relock (from ANY app).
  Idle ladder unchanged (dim → relock via auto-relock). All animations
  interruptible — first touch wins, everywhere (established doctrine).

## 2. Open/close morph (wheel ↔ app)

Curve (0.4, 0, 0.2, 1), integer-approximated; 25 ms cadence; interruptible.

- **Open (~350 ms)**: focused row's icon scales 56→96 px while translating
  icon_cx→CX, y→150; its glow halo expands ~1.4x and dissolves (alpha→0);
  non-focused rows fade+slide 12 px away from center; then app content rises in
  (wheel-intro mechanics: 2-frame stagger, 20 px rise, cubic ease-out) while the
  hero icon crossfades into the app's hero element. Status clock never blinks.
- **Close (~240 ms)**: reverse, faster (returns are always faster). PWR mid-morph
  reverses from current progress — never queue, never snap.
- Rendering: reuses WheelFx-style rect cache (targeted clear + union bbox flush);
  scaled icon frames via blit_icon_scaled (bilinear, nearest in fast phase).

## 3. Unlock time-morph (recap of M6 §11 — first W3 milestone)

Unlock lands in the WHEEL (Scene::Unlocked leaves the main flow; Spike's art
moves to Gallery). As the sheet travels up, the lock digits scale/translate
continuously from center-large (TIME_GLYPHS) to the status slot (small, CX,52),
scrubbed 1:1 by sheet progress — scrubbing the sheet scrubs the morph. Wheel
rows reveal underneath keyed to the same progress (intro reveal driven by p, not
frames). Release completes/springs back with the existing verdict.

## 4. Per-app high-fidelity designs

Row order: Time, Gallery, Phone, Messages, Activity, Settings, Music, Photos,
Weather, Timer. Accent named per app.

### 4.1 Time — accent: azure
Big centered HH:MM (TIME_GLYPHS, the lock digits at full size, y≈CY−10) — the
same glyphs the user already loves. Under it, date line "TUE 21 JUL" (TEXT).
**Signature motion**: a 2 px azure seconds arc sweeping the rim (r=226, lock-arc
AA tech), tip carrying a soft 8 px glow. W5 upgrade path (3 decorative
selectable rings) builds on this screen; W3 ships this as the elegant base.

### 4.2 Gallery — accent: sunset amber (Bebop lives here)
Full-bleed art. Page 1: the existing Spike artwork (moved from the old Unlocked
scene). Pages 2..N: additional Bebop stills (see §6 asset note). Horizontal
swipe pages with the wheel's physics vocabulary (1:1 direct drag, velocity
verdict, detent snap per page, rubber at ends). Footer: page dots (8 px, lit
dot glows amber). Final page holds the caption, small italic-feel spaced text,
bottom-center: **"SEE YOU SPACE COWBOY..."** — quiet, no fanfare; it's simply
the last thing in the gallery. Captions fade in 300 ms after the page settles.

### 4.3 Phone — accent: teal
Placeholder as *object*, not fake app: a centered rotary-dial motif — 10 digit
glyphs (TEXT) arranged on a r=120 ring around a teal contact circle with
initials "JB" (chord-fitted). The dial ring is ghosted at 40%; pressing a digit
pulses it (press-pulse from W4 vocabulary, no sound, no promise of calling).
Sub-line under title: "no line out here" (in-world honesty, ghosted 30%).
**Signature motion**: the dial ring drifts ±2° (slow triangle, 8 s).

### 4.4 Messages — accent: violet
Three static conversation bubbles, chord-fitted rounded rects (AA corners,
r=14): left bubble "JET: where are you" / right bubble (ice-blue fill 20%,
right-aligned) "chasing a bounty" / left "ED: found it! found it!". Names in
30% caps above bubbles. Timestamps ghosted right. **Signature motion**: a
typing indicator (three 4 px dots, staggered 300 ms pulse) under the last
bubble — the conversation forever almost-continuing.

### 4.5 Activity — accent: trio (azure / violet / teal)
Apple-style triple ring, saber-family colors — reuses ring AA + glow tech
directly. Radii 96/72/48, stroke 14 px, closure 82%/64%/91%, each tip carrying
the lock-arc tip glow. Center: "6 412" (LABELF) over "steps" (TEXT 40%).
**Signature motion**: on open, rings sweep from 0 to their closure (600 ms,
ease-out, staggered 80 ms) — the one place a bigger entrance is earned; at
rest, tips breathe like the wheel ring.

### 4.6 Settings — accent: ice (none)
A miniature of the wheel itself (continuity): 3 rows, PITCH 56, small icons
left, labels center — Brightness / Display / About. Focused row gets a micro
glow ring (glow sprite at 60% scale). Brightness row shows a live value arc —
**functional**: tap cycles 3 presets (writes 0x51; the one real control we own).
About row content (opens inline, replacing list): FW git-hash, chip "ESP32-S3",
"466×466 CO5300", battery %, uptime. Real data only.

### 4.7 Music — accent: warm white
Now-playing for a record that isn't spinning anywhere else: procedural vinyl
disc (r=110, center CX,y=225) — concentric groove rings (1 px, 8-12% ice),
amber label circle r=30 with "TANK!" (TEXT). Below: "SEATBELTS" (TEXT 60%) /
progress hairline 40% played. Ghosted transport glyphs (prev/play/next, Lucide,
30%) in footer. **Signature motion**: the disc rotates ~4 rpm (procedural
groove shimmer via phase-shifted arc highlights — partial ring redraw, the
tick_ring pattern exactly).

### 4.8 Photos — accent: azure
The elegant empty state (premium apps make emptiness beautiful): centered
aperture glyph (Lucide `aperture` at 96 px, 50%) inside a soft azure inner-glow
disc r=80 (flourish bloom tech at low amplitude), caption "nothing captured
yet" (TEXT 40%). **Signature motion**: the aperture glow breathes on the saber
lut cycle (identical cadence to the wheel ring — the whole OS breathes at one
tempo). Distinct from Gallery by design: Gallery is the curated art; Photos is
honest emptiness awaiting a camera that doesn't exist.

### 4.9 Weather — accent: amber
Mock presented as scene, not forecast: large "22°" (LABELF at 1.4x via scaled
draw) at y≈200, Lucide `sun` at 72 px to its upper-left with layered amber glow,
"MARS — CLEAR SKIES" (TEXT 50%) beneath — in-world flavor makes the mock
honest and charming. Hi/lo "26° / 14°" ghosted. **Signature motion**: sun rays
rotate imperceptibly (one 1 px highlight arc orbiting the sun disc, 20 s).

### 4.10 Timer — accent: azure→red gradient
Ring-first design: full-rim ring (r=200, stroke 10) at 100%, center "05:00"
(LABELF). W3 ships it **functional-minimal**: tap starts/pauses; ring depletes
in real time (RTC-driven, partial arc redraws — lock-arc tech in reverse);
color lerps azure→violet→red over the final 10%; at zero, three 300 ms
full-ring pulses then reset. Long-press (600 ms hold) resets. It's ~a day of
work on proven tech and makes one more app REAL.

## 5. Scene machine & integration

- `Scene::App(usize)` replaces Scene::Unlocked in the main flow (Unlocked scene
  + its PWR toggle path retired; Spike asset moves to Gallery).
- PWR: Locked→flourish · Wheel→open focused app · App→close to wheel.
- Relock: top-edge swipe from wheel AND every app (same recognizer path);
  auto-relock timer runs everywhere; wake-from-idle returns to where you were
  (wheel/app), Locked only via relock.
- WheelFx invalidate on every scene hand-off (established pattern).
- Each app = one `draw(fb, state)` + optional `tick(fb, elapsed)` partial
  animation, registered in a static APPS table next to WHEEL_APPS — the wheel
  index IS the app index.

## 6. Assets & budget

- New Lucide rasterizations (build.rs, existing pipeline): aperture 96, sun 72,
  transport glyphs 40, phone digits use TEXT_GLYPHS. ~40 KB flash total.
- Gallery stills: full-screen RGB565 = 434 KB each. Firmware ~1 MB, partition
  16 MB → room for many. **OPEN QUESTION (user)**: supply 2–4 Bebop stills to
  assets/gallery/ (any resolution; build.rs will center-crop/scale/dither), or
  W3 ships Spike + 2 procedural duotone treatments of it (azure night / amber
  sunset) until stills are provided.
- Vinyl, rings, bubbles, dial: all procedural — zero flash cost.

## 7. Open questions (answers shape implementation, defaults ready)

1. **Gallery stills**: provide images, or start with Spike + duotone treatments?
   (Default: Spike + treatments, swap-in later.)
2. **Bebop flavor in mock content** (Jet/Ed messages, TANK!/Seatbelts, MARS
   weather): keep as drafted, or neutral placeholders? (Default: keep — it makes
   the templates feel curated instead of lorem-ipsum.)
3. **Timer functional in W3** as designed, or template-only? (Default: functional.)

## 8. Build order (each step flashed + user-validated)

- **W3.1** Unlock→wheel + time-morph (§3); retire Scene::Unlocked; Spike→Gallery.
- **W3.2** Scene::App + template frame (clock/title/relock/PWR-back) + open/close
  morph, proven on Photos (simplest screen).
- **W3.3** Gallery: pages, swipe physics, captions, "SEE YOU SPACE COWBOY...".
- **W3.4** Batch A: Time, Activity, Settings (ring/list tech reuse).
- **W3.5** Batch B: Music, Weather, Phone, Messages (procedural art).
- **W3.6** Timer (functional) + relock-from-all-states + idle-ladder pass.
