# Wheel "Instant" Plan — zero-lag micro-interactions

Goal (user): the wheel must feel instant — responsive the moment it loads in, zero lag
between consecutive actions, hard flicks controllable (no easy railing), easier
overscroll, higher effective fps. Research basis: docs/research/TOUCH-PIPELINE-RESEARCH.md
(perception budgets, AOSP/iOS mechanisms) + measured codebase audit.

## Measured/verified problems (codebase audit 2026-07-22)

| # | Problem | Evidence |
|---|---------|----------|
| P1 | Intro is deaf | 29 frames x 25 ms = ~725 ms with zero touch polling (app.rs intro loop) |
| P2 | Scroll frame cost ~21-27 ms → ~45 fps ceiling | draw_scroll: full 424 KiB PSRAM clear + full-screen mark_rect → always the 13 ms full-flush path |
| P3 | Drag-start jump | DragStart carries the full pre-classification distance (slop ~10 px+) → first tracked frame jumps by it |
| P4 | Hard flicks rail too easily | wheel_power doubles at V_REF=700 raw; hotter velocity read compounds it; V_MAX=4000 ≈ 25-row projection on a 9-row list |
| P5 | Overscroll stiff | rubber d=64 px (max stretch well under a row); research: entry slope c=0.55 correct, d is the travel lever |
| P6 | Blind window between actions | run loop renders ring + flushes before polling; coast exits to run loop between consecutive gestures |

## The plan (phased; each phase independently flashable + feel-testable)

### Phase A — Feel constants (minutes, zero risk)
- A1 Rail accountability (research Rx4, NumberPicker/Wear doctrine):
  - V_REF_Q8 700 → 1200 (power-curve knee: doubling now needs a genuinely hard flick)
  - V_MAX_Q8 4000 → 1400 = exactly full-list travel (9 rows x 68 px / K=112 ms).
    The hardest single flick and any chain cap at "one full traversal" — hard flicks
    still rail, but only when genuinely violent; nothing is ever faster than rail speed.
- A2 Easier overscroll (research Rx7): keep c=0.55 entry slope, raise d 64 → 128 px
  (max stretch ~1.9 rows). Optionally c → 0.6 if still stiff after feel-test.

### Phase B — Instant-on (interruptible intro, research Rx8)
- B1 Arm free-scroll recognizer BEFORE the intro starts.
- B2 Poll touch every intro frame; on first touch evidence (DragStart / DragEnd /
  finger_down): draw the final resting frame immediately, enter Wheel scene, and route
  the gesture straight into wheel_interact (DragStart → grab; DragEnd → flick). Never
  queue, never replay, never block on the animation.
- B3 Same interruptibility for the relock/unlock composites that end in Wheel later (W3).

### Phase C — Zero-jump drag start (research Rx2, AOSP remainder consumption)
- C1 wheel_track re-anchors at the recognition point: baseline = DragStart dist;
  target = s_start + (dist - baseline). First tracked frame moves only the remainder —
  content engages from zero, no catch-up jump. Flick energy is unaffected (release
  velocity carries it).

### Phase D — Frame pipeline: damage-minimized scroll + motion LOD (research Rx5/6)
- D1 Targeted clear: draw_scroll keeps a small persistent rect cache (last frame's
  glow/icon/label rects, ≤32). Clear only those rects (~80-150 KiB worth) instead of
  fill(0) on 424 KiB. Status bar excluded — drawn once, never cleared during scroll.
  Cache invalidated on scene entry/wake (first frame = today's full clear+draw).
- D2 Damage marking: mark old ∪ new rects instead of the whole screen; raise the
  partial-flush threshold (currently 1/3) to ~3/4 for span flushes and MEASURE with the
  existing flush_ms/span logs. Expected scroll frame: clear ~1-2 ms + draw ~5 ms +
  flush ~5-8 ms partial ≈ 12-16 ms (from 21-27 ms).
- D3 Motion LOD: above |v| ≈ 2 px/ms (row moves >50 px/s visibly blurred), skip the glow
  halo and render crisp pre-rendered sprites only (no bilinear mid-sizes). Restore full
  fidelity below threshold — the decay tail is slow, so the landing frames are always
  full quality. (User-approved: ring may lag during heavy movement.)
- D4 Cadence: hold 25 ms rigidly (research: steady cadence beats jittery-faster; jitter
  reads as velocity flicker). If headroom after D1-D3 proves out (logs), trial 20 ms.

### Phase E — Zero lag between consecutive actions (research Rx1/3)
- E1 Coast linger: after a glide lands, stay in the interaction loop polling touch for
  ~150 ms before returning to the run loop — the next gesture starts with zero
  scene-machine overhead (no ring tick/flush between).
- E2 Run-loop late-latch: poll touch once BEFORE the ring tick render+flush each idle
  frame; an event preempts the tick (ring resumes next frame — user-approved delay).

## Order & verification
A+B+C+E first (small, one flash together), then D (renderer, one flash). Verify with
existing telemetry: "wheel: glide/chain v=... -> row N" lines for feel constants,
flush_ms/span/render logs for D. Feel-test gates each phase; constants (V_REF, V_MAX, d,
LOD threshold, linger) are tunable from the logs without re-planning.

## Non-goals (hardware learnings, do not revisit)
Touch prediction/extrapolation; stacked input filters; INT-gated reads during drags;
dual-core pipeline (PSRAM contention, see PIPELINE-RESEARCH).
