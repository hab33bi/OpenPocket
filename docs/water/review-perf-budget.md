# Water — adversarial review: PERFORMANCE / per-frame budget

Lens: does physics + render truly fit the ~10–11 ms compute window (25 ms
cadence − ~13 ms full flush) on Xtensa integer math? Recounts the real ops for
the stated counts, grounded in the actual helper bodies in
`src/scenes/apps.rs` (the draft duplicates them verbatim).

Cycle model: 240 MHz, integer `MULL` 1 cyc, hardware signed divide ~32 cyc,
SRAM load 1–2 cyc, branch ~2 cyc. PSRAM sequential write ≈ 85 MB/s (perf-plan:
424 KiB memset ≈ 5 ms). Full flush ≈ 13 ms; partial scales with dirty bytes.

## Verdict: fix-then-ship

The model fits comfortably **at rest** (~9–10 ms/frame → 40 fps with room to
spare). It **overruns the 25 ms cadence on the signature slosh/spray frame**
(~27 ms → sub-40 fps stutter at exactly the dramatic moment). One root cause
dominates and the fixes are local (a build-time LUT + a glow cap), not a
rewrite. Physics is fine; the overrun is entirely in render.

---

## F1 (HIGH) — `max_px` glow/meniscus: 6–8 divides/pixel × an *unbounded* pixel count → render under-budgeted ~4× and the slosh frame busts the cadence

**Where:** `water_draft.rs` `max_px` (l.709–727, six `/255` per pixel — 2 per
channel × 3), `soft_glow` (l.730–743, +2 divides/pixel for the falloff),
`draw_meniscus_line`/`line_max` (l.748–777), and the render call sites: the
per-surface-particle glow (l.614–616) and the meniscus line (l.626–627).
Confirmed identical to the real `apps::max_px`/`soft_dot` (apps.rs l.1146,1165).

**The cost the spec hides.** IMPL-SPEC §8 budgets "surface glow ~0.3 ms" and
"meniscus line ~0.1 ms". But `max_px` is *the exact per-pixel divide cost the
spec forbids for the body* — `tint.c * v / 255 * 31 / 255` is two divides per
channel, six per pixel (~192 cyc), plus `soft_glow`'s two falloff divides
(~8/pixel total). `soft_glow(r=3)` touches ~29 pixels. So one glow ≈
29 × ~200 cyc ≈ 5 800 cyc ≈ 24 µs.

**The count is unbounded and spikes at the worst time.** `soft_glow` fires for
every particle whose hysteresis `surface` flag is set (`n < SURF_LO=3`). A
flick/hard tilt is *precisely* what scatters particles into low-neighbour-count
isolation → the surface set explodes. At rest ~64 surface particles; during a
spray, 150+ airborne particles are all flagged surface and all glow.

**Recount, worst (slosh/spray) frame:**

| stage | spec | recount | note |
|---|---:|---:|---|
| clear (full-disc rect 466×~396 ≈ 369 KB) | ~4 ms | ~4.3 ms | honest |
| splat 448×25 px (put565, streaming) | 0.7 ms | ~0.7 ms | + isqrt, see F2 |
| **surface glow** | **0.3 ms** | **~3.6 ms** | 150 × 29 px × ~200 cyc |
| **meniscus line** | **0.1 ms** | **~0.75 ms** | ~900 max_px × ~200 cyc |
| isqrt speed (F2) | (in 0.7) | ~0.7 ms | 448 Newton loops |
| **render total** | **~5 ms** | **~9.9 ms** | |
| physics | ~5 ms | ~4 ms | fits (see F3) |
| full flush | 13 ms | 13 ms | |
| **frame** | **~23 ms** | **~27 ms** | **> 25 ms cadence** |

The glow alone is ~12× over its line item, and it lands on the same frame as the
full-disc clear + full flush — the frame that already has the least slack.

**Fix (no rewrite):**
1. Both glow tints are compile-time constants (`(0,210,255)`, `(150,240,255)`).
   Bake a `GLOW565[256]` (RGB565-BE indexed by `v`) at build time; `max_px`
   becomes read → unpack → per-channel `max` → pack → write, **zero divides**
   (~15 cyc vs ~192). Glow 3.6 → ~0.3 ms, meniscus 0.75 → ~0.1 ms.
2. **Bound the glow count.** Don't glow every isolated particle — glow only the
   particles that back the meniscus line (one per `surf_top` column, ≤64), or a
   fixed top-N by speed. Otherwise render cost scales with airborne spray, the
   opposite of what the budget assumes.
3. Replace `line_max`'s per-step `*s/steps` divides (2/step) with a DDA/Bresenham
   increment.

With (1)+(2): worst render ≈ 4.3 + 1.2 + 0.3 + 0.1 ≈ 5.9 ms → frame
≈ 4 + 5.9 + 13 ≈ 23 ms, back inside 25 ms.

---

## F2 (MEDIUM) — 448× per-frame `isqrt` in render is a divide-per-iteration Newton loop seeded at `x = v` (a ~1400× overshoot)

**Where:** render l.582 `let spd = isqrt((vx*vx+vy*vy) as u32)`, run for **every
particle every frame**; `isqrt` l.664 (identical to `apps::isqrt` l.1311).

**Cost.** The loop seeds `x = v` and does `y = (x + v/x)/2` — one 32-cyc divide
per iteration. Seeded at the value itself, it *halves* each iteration until it
nears the root before the quadratic phase kicks in. For a fast particle
`spd2 = 2·1024² ≈ 2.1 M`, `sqrt ≈ 1448`, so ~log2(2.1M/1448) ≈ 11 halvings + ~2
refinements ≈ 13 iterations × ~32 cyc ≈ 415 cyc. Across 448 particles under
slosh ≈ 186 k cyc ≈ **~0.7 ms** — not accounted for in the §8 "splat 0.7 ms"
line, and it compounds the F1 overrun.

**Fix.** The speed only drives a LUT index (`base += spd>>4`); it does not need
the true magnitude. Use the L1 norm `(|vx|+|vy|) >> 4` (zero divides, ~4 cyc) or
a bit-width-seeded isqrt. Saves ~0.5 ms/frame and removes 448 divides from the
hot render path.

---

## F3 (LOW) — relax op-count is ~2× understated (fits anyway)

**Where:** IMPL-SPEC §8 "relax ~150 500 ops = 448 × 24 × 14"; code `relax`
l.444–495.

`K_CAND=24` caps *in-radius passes* (`count++` only after `d2 < H2`), not the
candidates *examined*. A 3×3 block of 16 px cells is 48×48 px; the in-radius
disc (r=16) is only ~35 % of that area, so ~65 % of candidates in the block pay
the full `dx,dy,d2,compare` cost (~20 cyc, 2 gather loads) and are then
rejected. At the ~7 px spawn/rest spacing a filled block holds ~47 particles →
~47 examined, ~16 pass; the `break 'scan` never fires at rest. So the honest
count is ~47–68 examinations/particle, not 24.

**Impact: none to budget.** The `break 'scan` bounds examinations to
≈ K_CAND/passrate ≈ 68 even in a compressed pile, so relax is
≈ 448 × (68·20 + 24·25) ≈ 0.88 M cyc ≈ **~3.7 ms** worst, ~2.6 ms at rest —
still inside the §8 physics line (5 ms). Physics as a whole recounts to ~4 ms
and fits. Flagging only because the stated op-count is ~2× low; no reduction
required. (If ever trimmed: shrink `CELL_PX` toward `H_PX` is already tight, so
the lever is `K_CAND` or `NW`, per §10 step 9.)

---

## What checks out

- Body splat is genuinely divide-free per pixel: `put565` (l.695) is a raw
  2-byte store over black, no RMW — the 11 k body pixels stream. Correct.
- Physics fixed-point/integrate/hash/damp/wall are all cheap and honestly
  counted; the wall's isqrt+5 divides touch only the ~40 rim particles.
- Clear cost and the shrinking-bbox partial-vs-full flush logic match
  `flush_dirty`'s real 3/4 rule (app.rs l.1012); at-rest frames genuinely
  partial-flush and hit 40 fps+.
- No `i64`, no f32 in the hot loop; overflow proof holds for the multiplies.

Net: **at rest it fits with margin; the slosh/spray frame overruns by ~2 ms
purely on the F1 glow cost.** Land the F1 glow-LUT + glow cap and the F2 L1
speed swap and the worst frame is back under 25 ms with margin — all const/
build-time edits, no structural change.
