# Time Display Handoff Prompt (time-display branch)

**Date:** 2026-07-07 (approx, based on logs)
**Branch:** `time-display`
**Hardware:** Waveshare ESP32-S3-Touch-AMOLED-1.75 (466x466 round, CO5300 QSPI, 8MB PSRAM, ESP32-S3R8)
**Goal:** Black background, centered Inter Display Bold digital time (HH:MM, 3x scale) + date ("July 7th 2026", 1x), with beautiful startup fade-in of text + solid thick bezel ring (10px padding from edge, r≈223) that draws with premium eased curve. On every minute change: undraw the ring (reverse curve) then immediately redraw it. All at highest possible FPS, silky smooth ring animation, truly solid line (no gaps/artifacts even when complete).

**Key constraints:**
- no_std + esp-hal (no floats in hot paths where possible, Q14 fixed point + LUTs mandatory for trig).
- Double PSRAM framebuffers, ping-pong render-to-back / flush-front via QspiBus (8KiB DMA chunks, direct 0x2C).
- Direct pixel writes (RGB565 BE).
- Live only (no prebake for this UI).
- Custom 1bpp glyph packing from fontdue in build.rs (InterDisplay-Bold.ttf at 72pt for scale-to-gray AA).
- Must stay fast: flush is ~25-?ms bottleneck; render must be << that for 15-30+ FPS.

## Current Architecture & Implementation (as of latest)

### Framebuffer & Pipeline (main.rs + qspi_bus.rs)
- Two PSRAM vecs: fb0/fb1 (466*466*2 bytes each).
- Loop:
  - render_to_back(fb, elapsed)
  - write_command(0x2C)
  - flush_bytes(front_fb)   // now direct slice, **no scratch copy** (saves CPU + PSRAM<->SRAM traffic)
- No hard cap (CLOCK_FRAME_US=0) — run full speed.
- DMA flush: first chunk has command+addr, subsequent are continuation quad writes. 8KiB chosen for fewer CS toggles.
- Init: AXP power, display init seq, 80MHz QSPI.
- Dual-core worker exists but clock path is single-core direct (the other shaders use row-split eval).

**DMA/PSRAM notes (what we know):**
- PSRAM writes are relatively slow/expensive (the reason targeted black and precomp lists were explored).
- fill(0) on full fb every frame is a cost but simple and reliable.
- Direct flush (passing PSRAM slice to half_duplex_write) is better than copy-to-scratch (we removed the copy).
- GDMA can access PSRAM; the current path tries to let it.

### Clock Logic (src/clock.rs)
- `Clock` holds last_minute / last_change_ms for minute detection + undraw/redraw state.
- `render_to_fb`:
  - `fb.fill(0)`
  - Compute fade_q14 (only first ~2.4s)
  - Compute bezel_p with cubic ease-in-out (accel then decel). Separate durs for initial (6000ms), undraw (2500ms), redraw (2500ms).
  - draw_bezel_solid(..., bezel_p, fade)
  - draw_time_centered (CY+5, scale=3)
  - draw_date (CY+70, scale=1)
- Time/date use real Inter glyphs (generated at 72pt, 1bpp packed, ymin captured).
- **scale-to-gray AA for text only**: in draw_glyph, for each output pixel sample 2x2 in high-res source via Q8 math, count set bits (0-4) → alpha = count*255/4, blend with fade. This matches the user's quoted technique exactly and produces beautiful sharp text.

**Bezel ring (the hard part):**
- Precompute once in `new()`: `bezel_offsets: Vec<u32>` (byte indices into fb).
  - 20160 angular steps (high for small steps).
  - For each sample: dr loop for thickness=10, **plus the 5 neighbor positions** baked in. This makes the list itself represent a solidly filled band.
  - Pushed in angular order → prefix of list = partial arc.
- `draw_bezel_solid(eased_p, ...)`:
  - Compute solid color once (full bright * intensity).
  - `num = (eased_p * list.len() as f32) as usize`
  - Write hi/lo bytes for list[0..num]
  - When p >= 0.99 → full list (complete ring).
- This replaced earlier pure on-the-fly and giant-list experiments.
- Q14 + 512-entry sin/cos LUT (from raidal/build.rs) + bias (+8192) for rounding.
- Cubic ease (inline in render).

**Positions (current):**
- Time: CY + 5, scale 3 (bigger, width hopefully matches date).
- Date: CY + 70, scale 1.
- Centered via summed advances (unit = src/2 because 72pt source).
- Baseline uses scaled ymin from TTF metrics.

**Other:**
- No post-animation traveling segment. Once full, static full solid.
- Black bg, white-ish text, blue-ish ring.
- FPS logging (ema).

## History / What Was Tried (chronology of problems & attempts)

(See also docs/02-ANIMATION-BOTTLENECK.md, 06-OPTIMIZATION-CHRONOLOGY.md for project-wide patterns.)

1. **Early versions**: On-the-fly arc with low steps (1k-5k), no neighbors → visible gaps, not solid, stepped animation.
2. **Solid attempts**: Added thickness dr, +bias, 5-neighbor fill, higher steps (5760). Became "solid" but still not silky (large per-frame jumps).
3. **AA on line (scale-to-gray inspired)**: Soft alpha at thickness edges + very high steps (20k+). Beautiful in theory, killed FPS (3-5 fps). User: "AA on the drawn line not the time", revert to solid.
4. **Giant precomp list (28k steps, no neighbors in list)**: Pure writes for speed. Targeted black every frame instead of fill(0). FPS tanked worse (~5.5 fps, 181ms) because blacking ~300k-entry list every frame was disastrous on PSRAM. List without neighbors → when complete, "weird effect", line not solid again.
5. **Current (hybrid → unified list)**: fill(0) restored, moderate→high steps (20160) with **neighbors baked into list gen**, always prefix of list (consistent solid for partial + full). Longer durs (2500/2500/6000). Direct DMA kept. Result: higher FPS than the giant-black version, but...
6. **Other experiments**: No full clear (targeted only) → remnants on text/ring changes. Different ease curves, different thicknesses.

**Known perf characteristics:**
- Full fb fill(0) + list blast (current ~100k-200k writes?) + text AA + flush → current achieved FPS (user reports "higher" than 5.5 but "doesn't seem smoother").
- Flush itself is heavy (many 8KiB chunks + CS overhead on this panel).
- PSRAM random writes are costly; sequential blast from list is better than scattered.
- On-fly trig/LUT per active sample is measurable when steps high.

## Current Problems (as reported + observed)

1. **Higher FPS but "doesn't seem smoother"**:
   - Animation of the ring (the growing/shrinking arc) still looks stepped/jerky.
   - Root: even with 20160 steps, the delta_p per frame (at achieved FPS and current 2500ms durs) causes relatively large jumps in `num = p * len`.
   - At ~20 fps and 2500ms sweep: ~50 frames for full circle → ~400 steps per frame advance. Still visible.
   - Need *smaller steps* (finer visual increments per frame).

2. **When ring completes, line isn't completely solid / "weird effect" again**:
   - In previous giant-list version the list gen did not include neighbor fills.
   - Even now, if list gen and draw path diverge, or if at the p==1 threshold there is a one-frame pop, or if the baked neighbors aren't sufficient for the exact sampling.
   - User sees it "once the ring completes".

3. **Silky smooth line drawing desired**:
   - The "draw" effect (arc growing from presumably 0° ) must look continuous, premium, no banding/gaps/stair-steps even on the thick ring.
   - Partial (anim) and full (static) must look identical in solidity.

4. **FPS still not "substantial" enough for the user** (or at least the smoothness doesn't feel like it matches the FPS number).
   - 181ms frames were seen in one log (bad impl).
   - Flush is likely the floor.

5. **Warnings**:
   - Some pre-existing (light_rays parens, unused raidal imports in main — other shader paths).
   - We cleaned the clock-specific dead_code (smoothstep_q14 etc.).
   - FONT_METRICS spam on every build (from build.rs prints) — annoying but diagnostic.

6. **Other nits**:
   - Module comments sometimes lag the code (e.g. render doc mentioned targeted writes after we restored fill).
   - Text baseline/positioning was fought over; current seems accepted but fragile.
   - No easy way to measure render vs flush time separately.

## What We Know Works / Tradeoffs

- **scale-to-gray on font**: Excellent. Keep 72pt 1bpp + 2x2 count. Do **not** apply similar AA math to the ring (kills FPS).
- **Cubic ease**: Good "premium" feel. Keep.
- **Double buffer + direct DMA flush (no scratch)**: Good improvement for PSRAM/DMA usage.
- **fill(0) + list blast**: Better than blacking huge lists.
- **Q14 + LUT + bias**: Essential for clean circles.
- **Precomputed list for static**: Big win vs computing trig every frame for the common case.
- **Baking neighbors into list**: Makes partial and full consistent.
- **Longer durations**: Directly gives smaller p-delta per frame → smaller arc steps (at cost of "slower" feel).
- Higher achieved FPS helps smoothness (more samples of the animation curve).

**Tradeoffs**:
- Higher list resolution (smaller steps) = larger list = more writes per static frame.
- Longer anim duration = silkier but feels less "snappy".
- fill(0) every frame = simple/correct but not free.
- Pure list prefix for anim: order of pushes (angular + local neighbors) is "good enough" but not perfect angular sort.

## Recommendations / Next Steps for Silky Smooth Solid Ring

1. **For smaller steps / silkier animation (primary request)**:
   - Increase precomp steps further (try 28800-40000) if FPS allows.
   - Or (better lever): increase undraw_dur + redraw_dur + initial to 3000-4000ms+ / 7000-8000ms. At 20-30 fps this gives much finer p increments.
   - Consider making initial/undraw/redraw durations different or configurable.
   - Profile: at current FPS, what is the observed arc advance per frame? (add temp debug print of delta num?).

2. **Fix / guarantee solid when complete**:
   - The current baked-neighbor list + unified prefix path should fix the "weird effect".
   - If still gaps on full: during list gen, be more aggressive (e.g. 3x3 instead of +1 neighbors, or multiple passes, or post-process to fill holes).
   - Alternative for full static: after blasting list, do a cheap "thicken" pass, or fall back to a small on-fly with neighbors just for full (but that re-introduces the switch artifact).
   - Verify visually on device with a still frame.

3. **Further FPS wins (use PSRAM/DMA even better)**:
   - The flush is probably still dominant. Ideas:
     - Larger chunks if the HAL/display allows (try 16KiB?).
     - See if we can avoid full 0x2C + full frame send every time (partial updates? panel may not support easily).
     - Use reclaimed SRAM for a "dirty" mask or small working set, but ring+text is sparse.
     - Precompute the *color bytes* too? (two lists or packed u16 offsets+color, but color changes with fade/intensity rarely).
   - For static ring: if list blast + fill is still heavy, explore GDMA mem2mem fill or other, but keep simple.
   - Remove fill(0) + only write the current drawn pixels (ring list + text on-pixels). Requires blacking only the *previous* drawn set on changes. Tricky but big win if done right (track previous num + previous glyph bboxes).
   - The dual-core worker is idle for clock path — not useful here.

4. **Animation quality**:
   - The cubic is good, but perhaps combine with a tiny sin modulation or different ease for "premium watch" feel.
   - Make sure the starting angle of the "draw" looks intentional (currently phase 0).
   - When p reaches 1, force exact full list (no float precision issues).

5. **Code hygiene**:
   - Keep comments in sync with implementation (the render doc was stale).
   - Consider moving constants (steps, durs, thickness, PAD) to top with comments.
   - The FONT_METRICS warnings on every build are from build.rs — either remove the println or gate behind a feature.

6. **Testing / measurement**:
   - Add temp timing: measure render time vs total (use Instant before/after fill+draws).
   - Log "bezel num pixels this frame" during anim to see step size.
   - Test on real hardware with different animation speeds.
   - Check for tearing (double buffer should be good).
   - Verify baseline alignment of Inter after position changes (use 'y' etc.).

7. **If we need even silkier**:
   - Precompute *both* a high-res ordered list (for prefix anim) and a deduplicated compact list (for fast static full).
   - Switch at p==1 to the compact one (may have a 1-frame pop if sizes differ).
   - Or accept the cost of high-res list for everything and optimize the write loop (e.g. manual unrolling, or write in larger bursts if possible).
   - Draw the ring as multiple concentric passes with slight offsets for "premium" thickness look.

## Files of Interest
- `src/clock.rs` — almost everything (precompute, draw_bezel, render, text, positions, durations).
- `src/bin/main.rs` — double fb, loop, flush calls, no-cap, logging.
- `src/qspi_bus.rs` — flush_bytes (direct now).
- `build.rs` — font gen at 72pt + 1bpp pack + ymin.
- `assets/InterDisplay-Bold.ttf`
- `docs/` — other handoffs for patterns (esp. 02, 06 for perf thinking).
- `src/raidal.rs` — LUT + Q14 helpers (reused).

## Known Good State to Preserve
- Inter font AA (scale-to-gray) looks great.
- Minute undraw + immediate redraw.
- Eased curve.
- 10px pad, black bg, centered layout.
- Double buffer + DMA path.
- No AA math on the ring itself.

## Handoff Summary for Next Person
The ring "draw" is the only complex animation. The current list-prefix approach with neighbors baked in + high steps + longer durs is the best attempt at "silky smooth + always solid".

**Immediate experiments to try**:
1. Crank precomp steps to 30000+ and re-flash. Measure FPS and visual step size.
2. Increase undraw/redraw to 3000-3500ms (and initial proportionally). This is the easiest lever for "smaller steps".
3. If static FPS suffers from large list, add a dedup pass for the full-ring blast path only (use a temp [bool; W*H] or hash during precomp — precomp is once).
4. Temporarily force on-the-fly high-steps path even for p==1 and compare solidity/FPS.
5. Add debug prints (or serial) of "bezel pixels written" and "frame render ms" to quantify.

Document any new findings, failed experiments, and exact numbers (achieved FPS, visual quality notes) back into this file or a new one.

Good luck — the goal is a premium, smooth, solid, watch-like ring that feels expensive even though it's "just drawing a line" on a tiny MCU with PSRAM.

**End of handoff.** Update this doc with results.

---

## Update 2026-07-07 — Silky Ring Experiments (implemented, pending hardware verify)

### Changes made (`src/clock.rs`, `src/bin/main.rs`, `build.rs`)

1. **Higher angular resolution**: `BEZEL_STEPS` 20160 → **36000** (smaller arc increments per frame).
2. **Longer animation durations** (smaller `p` delta per frame at same FPS):
   - Initial sweep: 6000ms → **8000ms**
   - Undraw: 2500ms → **3500ms**
   - Redraw: 2500ms → **3500ms**
3. **Dual bezel lists** (addresses static-FPS vs anim-smoothness tradeoff):
   - `bezel_offsets_anim`: high-res angular-order list (may contain duplicates) — used for **prefix** draws while `p < 1`.
   - `bezel_offsets_full`: **deduped** row-major list built from a `covered[]` bitset at init — used when `p >= 1.0` for fewer writes on static frames.
   - Exposed `bezel_anim_len` / `bezel_full_len` at init for serial diagnostics.
4. **Stronger solidity**: neighbor fill expanded from 5 (cross) → **3×3** during precompute. Same bitset drives the deduped full list so partial and complete coverage match.
5. **Exact full ring at completion**: `draw_bezel_solid` branches on `eased_progress >= 1.0` → always blasts entire deduped list (no float truncation at 0.99).
6. **Profiling hooks**:
   - `Clock::last_bezel_writes` + `last_bezel_delta` (absolute frame-to-frame change).
   - Main loop logs: `render=Xms flush=Yms total=Zms | bezel_writes=N delta=D` (1 Hz EMA fps).
7. **Constants moved to top** of `clock.rs` with comments (`BEZEL_STEPS`, thickness, durations, fade).
8. **build.rs**: `FONT_METRICS` warnings gated behind `FONT_DEBUG=1` env var (quiet default builds).

### Expected effects (theory — flash to confirm)

| Metric | Before (approx) | After (expected) |
|--------|-----------------|------------------|
| Arc steps per frame @ 20fps, 2500ms sweep | ~400 list entries/frame | ~**257** anim entries/frame @ 20fps/3500ms; finer angular sampling (36000 vs 20160) |
| Static ring writes/frame | full anim list (~100k–200k+ with dupes) | **deduped full list only** (unique ring pixels; expect substantially fewer) |
| Ring solidity at p=1 | occasional gaps / "weird effect" | 3×3 fill + deduped full path should eliminate gaps |
| Build noise | FONT_METRICS every build | silent unless `FONT_DEBUG=1` |

### What to look for on device

Serial log examples to capture:
```
Clock ready N ms | bezel anim=XXXXX full=YYYYY offsets | ...
clock fps~XX.X render=Rms flush=Fms total=Tms | bezel_writes=W delta=D
```

- During startup/minute anim: `delta` should be **smaller** than before (smoother sweep). Target visually: no visible stair-steps on the thick ring.
- After anim completes (`bezel_writes == bezel_full_len`): ring should be **fully solid** with no pinholes or banding.
- Static phase: `bezel_writes` should equal `bezel_full_len` and stay flat; `render` should drop vs anim phase.
- Compare `flush` vs `render` — if `flush >> render`, DMA chunk size / partial-update experiments are the next lever.

### If still not silky enough

- Bump `BEZEL_UNDRAW_MS` / `BEZEL_REDRAW_MS` to 4000ms.
- Try `BEZEL_STEPS = 40000` if static FPS holds (watch `bezel_anim_len` in log).
- If anim-phase FPS dips (large anim prefix), consider leading-edge-only 3×3 expansion instead of baking all neighbors into anim list (not yet implemented).

### Build status

`cargo check --features esp` passes (2026-07-07). Hardware flash + visual validation still required.

### Hotfix 2026-07-07 — black screen (PSRAM OOM)

**Symptom:** Display stayed completely black after flash.

**Root cause:** Baking 3×3 neighbors into the anim list at 36000 steps produced ~3.5M `u32` entries (~14 MiB), exceeding the 8 MiB PSRAM budget. `Clock::new()` likely panicked during precompute (panic handler loops forever → black panel).

**Fix:**
- Anim list now stores **center pixels only** (~396k entries, ~1.5 MiB).
- 3×3 solidity applied at **draw time** during arc anim (`write_bezel_3x3`).
- Deduped full list still built from 3×3 bitset at init (unchanged).
- Removed per-frame `format!` for time string (stack buffer; 8 KiB SRAM heap).

Expected init log: `bezel anim=~396000 full=~19444 offsets`.

### Update 2026-07-07 — render time ramp during anim (profiled + fixed)

**Observed on device (prefix-redraw path):**
```
clock fps~11.7 render=66ms  flush=24ms total=90ms  | bezel_writes=30510
clock fps~9.9  render=112ms flush=24ms total=137ms | bezel_writes=252738
clock fps~7.7  render=239ms flush=24ms total=264ms | bezel_writes=863658
clock fps~6.3  render=458ms flush=24ms total=483ms | bezel_writes=1919943
clock fps~5.4  render=672ms flush=24ms total=697ms | bezel_writes=2945538
clock fps~5.5  render=66ms  flush=24ms total=91ms  | bezel_writes=19444  (static)
clock fps~11.0 render=66ms  flush=24ms total=91ms  | bezel_writes=19444  (stable)
```

**Root cause:** Double-buffer + `fb.fill(0)` every frame forced a **full prefix replay** of the anim list (`list[0..p*len]`) with 3×3 expansion. Cost grew **linearly with eased progress** (~3.5M pixel writes at p≈1). Flush stayed flat at ~24ms; render dominated. Static phase was fine (~66ms) because deduped full list is only ~19k writes.

**Fix (incremental + carry):**
1. **Copy prev front → back** via ping-pong (`render_to_fb(fb, Some(prev), ...)`) instead of `fill(0)` each frame.
2. **Incremental arc delta only:** add `list[drawn..target]` (grow) or black `list[target..drawn]` (shrink); never replay full prefix.
3. **Static mode:** when not in initial/minute-cycle anim, **skip ring draws entirely** — ring pixels carried in copied fb (~19k px, 0 writes/frame).
4. **Text:** clear tight text bbox each frame, redraw time+date (fade + digit changes).
5. **Per-frame cap:** `MAX_CENTERS_PER_FRAME = 4500` (~40k px writes worst case) to bound render time if a frame stalls.

**Expected after fix:** render ~50–70ms during anim AND static; `cdelta` ~4000–4500 centers/frame at ~11fps; `px_writes` ~0 in static, ~30–40k during anim; flush still ~24ms; total ~75–95ms consistent.

**New log format:**
```
clock fps~X render=Rms flush=Fms total=Tms | centers=C cdelta=D px_writes=W
```