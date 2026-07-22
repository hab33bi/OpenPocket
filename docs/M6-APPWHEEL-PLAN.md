# M6 — The App Wheel: Plan to the T

Status: **APPROVED DESIGN** (2026-07-21, all user decisions in). Build order §10.
Source of truth for the look: the user's concept image (right-aligned carousel
hugging the round bezel, focused "Activity" row large/bright, neighbors dimmed
and indented, status line "9:45 AM | 78%" top-center).

## 1. Purpose & scope

The app switcher is a **right-aligned vertical free-scroll carousel** wrapped
along the circular display's edge — and it becomes the **main unlocked hub**:
unlocking lands directly in the wheel. The Spike image moves into a "Gallery"
app row. Rows open **app template screens** (placeholder content with a
premium open animation); "Time" is the first semi-real app (§8).

## 2. Navigation map (final, user-decided)

| State | Input | Result |
|---|---|---|
| Locked | swipe up from bottom | unlock → **Wheel** (sheet reveals the wheel) |
| Locked | PWR short press | lightsaber ring flourish, stays locked (separate feature) |
| Wheel | vertical drag / flick | free scroll with momentum + snap |
| Wheel | tap focused row **or** PWR | open that app (morph animation, §6) |
| Wheel | tap non-focused row | animated scroll bringing it to focus |
| Wheel | swipe down from top edge | **relock immediately** |
| App | PWR short press | back to wheel (reverse morph) |
| App | swipe down from top edge | **relock immediately** |
| anywhere | 60 s idle | auto-relock (then dim → AOD → sleep ladder as today) |

Top-edge swipe-down = relock from anywhere; PWR = context action
(flourish / open / back). No other path returns to the old Spike screen —
it lives in the Gallery row.

## 3. Geometry (the circular indent — the signature)

Panel 466×466, center C=(233,233), R=233.

- **Scroll model**: continuous offset `s` (Q8). Rows at fixed pitch
  `P = 68 px` in list space; row *i* center `y_i = 233 + i·P − s`. Focused =
  nearest y=233; `s` at rest is an exact multiple of P.
- **Chord-following right edge**, recomputed every frame (glides, never
  steps): `half_w(y) = isqrt(R² − (y−233)²)`; right edge
  `x_r(y) = 233 + half_w(y) − 14`. Precomputed `chord[y]` LUT (466 entries).
- **Row layout**: icon right-aligned at `x_r`; label right-aligned to the
  icon's left edge − 12 px (labels grow leftward, per concept).
- **Focus interpolation** (continuous): `t = smoothstep(clamp(1 −
  |y_i−233|/(2P), 0, 1))`; icon 40 → 56 px, label small → large, alpha
  0.45 → 1.0 — all tracking `s` every frame.
- **Focus indicator**: a **blue glow ring** around the focused icon —
  pre-rendered layered-glow sprite (bright AA ring core + two soft halo
  layers), alpha-keyed to `t`. Pre-rendering sidesteps the AMOLED-black
  visibility failure of the runtime ring glow: the sprite carries real
  intensity (~60% peak halo) because it exists only at the focused icon.
- Visible rows: center ± 2 full, ± 3 fading at the crown.

## 4. Motion spec

- **Tracking**: any slop-exceeding vertical drag scrolls `s` 1:1 through the
  proven k=0.5 critically-damped filter. Wheel recognizer has **no edge arm
  zones** except: a drag *starting* in the top ~12% that moves down =
  relock gesture, not scroll.
- **Momentum**: release velocity (with lift-coord fold-in) carries;
  `v *= 0.94` per 25 ms frame (~400 ms decay), `s += v·dt`.
- **Snap**: |v| < 0.3 px/ms → exponential settle to nearest row (diff·3/8,
  snap ≤ 2 px) — the unlock settle's feel.
- **Rubber band**: past first/last row, displacement ÷3, springs back.
- **Press pulse**: focused-row tap-down scales icon 1.0 → 0.92 → 1.0
  (~150 ms) before the open morph.
- **Cadence**: 40 fps while `s` moves or finger down; 20 fps at rest; rest
  frames ~0 (retained canvas).

## 5. Icons — Lucide, rasterized like the fonts (user-decided)

- Source: **Lucide** (ISC license — vendor `LICENSE` alongside) SVGs,
  committed under `assets/icons/`: clock (Time), image (Gallery), phone,
  message-circle (Messages), activity, settings, music, images (Photos),
  cloud-sun (Weather), timer.
- build.rs rasterizes SVG → alpha with **resvg + tiny-skia** (build-deps
  only): true vector AA at exactly 40 px and 56 px, quantized to 4-bit
  alpha sprites — identical quality pipeline to the font atlases. Ice-blue
  tint applied at draw time via the existing intensity-LUT approach.
- The blue focus glow ring is generated the same way (procedural radial
  profile, not SVG), as is each app's morph-shape mask (§6).

## 6. App open/close — the morph (user-specced)

Open (~350 ms total, curve (0.4, 0, 0.2, 1)):
1. Press pulse completes; the focused icon **fades out within its own
   shape** while that shape (rounded glyph silhouette → circle) **expands
   and translates to screen center** simultaneously (~250 ms). Other rows +
   status line fade/slide out (~150 ms, overlapped).
2. The circle blooms to fill; **app content fades in** (~120 ms): template =
   centered app name + a quiet placeholder line; Gallery = the Spike image;
   Time = the three rings. The template's flavor text is a Cowboy Bebop
   surprise (implementation detail — left unspecified here deliberately).

Close (PWR): exact reverse — content fades, shape contracts back to the
row's icon position as the wheel rows slide back in. Relock (top swipe) skips
the reverse morph: the lock sheet slides down over whatever is showing.

## 7. v1 app rows (10)

Time, Gallery, Phone, Messages, Activity, Settings, Music, Photos, Weather,
Timer — one const table (name, lucide icon id, open-kind). Open-kinds:
`Template` (all), `Gallery` (Spike blit), `TimeApp` (§8).

## 8. The Time app (first semi-real app)

A refined lockscreen variant: time + date slightly smaller, and **three
concentric arcs** — ice-blue, amber, violet. v1 rings are **decorative**
(user-decided): they exist to prove the interactions —
- Tap a ring → it becomes **selected**: thickens 2–3× with a **premium
  layered glow** (bright core + mid halo + wide faint halo — pre-rendered
  per-ring glow sprites at both states, real intensity, AMOLED-aware).
- Exactly one selected at a time; the others sit muted (greyed, subtle
  residual glow). Tap the selected ring again → deselect all.
- Ring meanings (hour/min/sec progress etc.) assigned in a later iteration.

## 9. Rendering & perf plan

- Same retained WatchFb + DMI stack. Labels pre-rendered at both focus
  sizes (fontdue 4-bit alpha); size interpolation = **cross-fade** between
  the two sprites (anchored right + v-center) — smooth scale reading, zero
  runtime scaling artifacts.
- Damage per frame = union band of changed rows (erase old bbox → redraw →
  mark); partial vs full flush decided by the existing threshold.
- **Status line** top-center: `HH:MM | NN%`; battery % from AXP2101 reg
  0xA4 (per docs/research/BUTTONS-RESEARCH.md), polled every ~10 s, cached;
  redrawn on change only.
- PWR: I2C poll of AXP2101 reg 0x49 once per frame (no IRQ GPIO on this
  board — research-verified); init enables INTEN2 bits then **clears stale
  flags last** (chip latches pre-boot presses); never touches 0x22/0x27/
  rail registers (long-press hardware power-off stays intact).
- Budgets (measured, logged): scroll compose+flush < 20 ms typical /
  < 25 ms worst; touch-to-render < 50 ms; morphs 40 fps; rest frames 0 ms.

## 10. Build order (each step flashed + user-validated before the next)

- **W0 — Inputs**: PWR short-press bring-up (log-only, both scenes) +
  X-axis corner-tap verification (required for row hit-testing).
- **W1 — Static wheel**: geometry, chord indents, focus scaling + glow
  ring, Lucide icon pipeline, status line with live battery %.
- **W2 — Motion**: tracking, momentum, snap, rubber band, tap-to-focus,
  top-edge relock gesture.
- **W3 — Integration**: unlock lands in wheel; Gallery row carries Spike;
  PWR open/back; app template + open/close morph; relock from all states;
  idle-ladder interplay.
- **W4 — Polish**: enter stagger, press pulse, Bebop surprise, perf pass.
- **W5 — Time app**: three decorative rings, selection + layered glow.

Tracked separately (before/parallel to M6): the **locked-state PWR
lightsaber flourish** and the **unlock end-of-travel lag** fix.

## 11. W3 addendum (user, 2026-07-22): unlock time-morph

When unlock lands in the wheel (W3), the lock screen's large time digits
must MORPH into the wheel's small top status time: as the sheet travels
up, the digits scale/translate continuously toward the status-line
position and size, tracked 1:1 by unlock progress (scrubbing the sheet
scrubs the morph). The wheel's status clock is the morph's destination —
its position/size are the landing keyframe.

## 12. W3 scope update (user, 2026-07-22): Bebop -> Gallery

The W4 "Bebop surprise" is CANCELLED as a standalone moment. Cowboy Bebop
lives in the Gallery app instead (curated stills, final page carries
"SEE YOU SPACE COWBOY..."). Every other app ships an elegant high-fidelity
placeholder/template per docs/W3-APP-SCREENS-PLAN.md (the W3 design
contract - per-app layouts, accents, signature motions, build order).
