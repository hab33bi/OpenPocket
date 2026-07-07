# Optimization Chronology — Every Attempt, Result, Lesson

> **Purpose:** Prevent the next model from repeating failed paths and understand *why* each change had the effect it did.  
> **Doc index:** [`README.md`](README.md) · **Entry prompt:** [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md)

---

## Timeline overview

```
Phase 0: Black screen bring-up
Phase 1: Plasma demo (superseded)
Phase 2: Raidal-2 port, div=2, fused path     → ~4 s/frame, 0.2 FPS
Phase 3: SoA cache + row staging               → ~4 s (minor)
Phase 4: div=4 "extreme" + byte FB + 8K DMA   → ~1.6 s, 0.6 FPS, quality ↓
Phase 5: div=3 two-pass Q14 + upmap (current)  → slight improvement (split TBD)
```

---

## Phase 0 — Display bring-up

### Symptoms
- Solid black AMOLED after flash
- Serial showed boot OK, no panel errors

### Root causes found (all required)

| # | Cause | Fix |
|---|-------|-----|
| 1 | Single-line SPI instead of QSPI | `DataMode::Quad` for pixels, cmd `0x32` |
| 2 | GPIO 6/7 not wired in code | `with_sio2`, `with_sio3` |
| 3 | AXP2101 rails off | `axp2101_enable_display_power()` |
| 4 | CASET missing col offset 6 | `0x2A` with `LCD_COL_OFFSET` |

### Outcome
✅ Stable 466×466 image pipeline.

### Lesson
**Never regress QSPI protocol or PMIC sequence** — instant black screen.

---

## Phase 1 — Velvet Aurora plasma (`src/plasma.rs`)

### Approach
- Simpler domain-warped plasma, float sin/cos per pixel
- Full 466×466 eval
- Superseded by Raidal-2 but file remains in tree

### Outcome
- Display proof-of-life
- Too simple for user's aesthetic goal (Raidal-2 specifically requested)

### Lesson
`plasma.rs` is **not** the active renderer. Do not confuse with `raidal.rs`.

---

## Phase 2 — Initial Raidal-2 port (div=2)

### Architecture
- Float eval per pixel, 9 layers
- `PixelStatic` AoS cache in PSRAM: `atan_p` + `denom[9]` per pixel
- Per-frame: iterate all 54,289 low-res pixels (233×233)
- Full fused upscale to 466×466 each frame
- `low_rgb` f32 scratch in PSRAM (~650 KiB at one point)

### Measured (user serial)
```
render ≈ 3975 ms
flush  ≈ 38 ms
total  ≈ 4013 ms
fps    ≈ 0.2
```

### Analysis
- ~80% eval, ~20% upscale+overhead (estimated before split timing)
- PSRAM random access on AoS structs per pixel
- 36 LUT calls × 54k pixels ≈ 2M LUT ops

### Lesson
Shader math volume at div=2 is **inherently heavy** for single-core MCU.

---

## Phase 3 — SoA cache + row staging + fused upscale

### Changes
- Split `atan_p` and layer-major `inv_denom` (SoA)
- Precompute `0.03/denom`
- Copy one row (~9 KiB) to internal SRAM per scanline
- Fused: eval row → immediately upscale associated output rows
- `OutputBands` precomputed which output rows trigger on each low row

### Measured
- User reported **minor** improvement (~4 s → still ~4 s order of magnitude)

### Why it barely helped
- **Upscale still touched all 217k output pixels** via 466-wide float bilinear per output row
- Row staging helped eval reads but eval still dominated
- Output band iteration overhead per low row

### Lesson
**Profile upscale separately** — fused paths hide cost in one `render_ms` number.

---

## Phase 4 — "Extreme" optimization (div=4)

### Changes
- `RENDER_DIVISOR = 4` → 117×117 eval
- Row-packed static cache (single memcpy per row)
- Macro-unrolled 9 layers
- PSRAM byte framebuffer (no u16→byte swap on flush)
- DMA chunks 4 KiB → 8 KiB
- Fused row upscale retained

### Measured (user serial)
```
render ≈ 1599 ms
flush  ≈ 25 ms
total  ≈ 1625 ms
fps    ≈ 0.6
```

### Quality
❌ User: **visible degradation** — blocky aurora filaments on 466px round display.

### Math validation
| Component | div=2 est. | div=4 meas. |
|-----------|------------|-------------|
| eval | ~3200 ms | ~800 ms |
| upscale | ~800 ms | ~800 ms |

Speedup 4000→1600 = **2.5×**, not 4×, because upscale constant.

### Lesson
**RENDER_DIVISOR is not a FPS multiplier.** It trades quality for partial eval savings only.

---

## Phase 5 — Two-pass + Q14 + upmap (current code)

### Changes
- `RENDER_DIVISOR = 3` → 156×156 (user-selected quality floor)
- **Pass A:** `eval_pass()` → `low_rgb565` via Q14 fixed-point
- **Pass B:** `upscale_pass()` via precomputed `UpPixel[217156]` integer bilinear
- Removed fused `OutputBands` path
- Split timing: `eval_ms`, `upscale_ms`, `flush_ms`
- `SIN_LUT_I16` Q14 in `build.rs`
- Tight `for layer in 0..9` loop (removed macro unroll)

### Measured (user serial, `--release`, stable ±1 ms)
```
fps~1.0 eval=798ms upscale=217ms flush=25ms total=1042ms
First frame: 1043 ms | Init cache: 498 ms
```

| Component | Expected | **Measured** |
|-----------|----------|--------------|
| eval | 200–600 ms ideal | **798 ms** (77% of frame) |
| upscale | 30–80 ms | **217 ms** (21% — better than 800 ms fused, still 7× over target) |
| flush | ~25 ms | **25 ms** ✅ |
| quality | Better than div=4 | User confirmed OK |

### Analysis
- Integer upmap **works** (217 ms vs ~800 ms fused) but `low_rgb565` likely still in PSRAM
- **Eval is the primary bottleneck** — 798 ms needs ~10× cut for 10 FPS target
- 10× total frame: 1042 → 104 ms requires **stacked** optimizations, not one fix

### Lesson
Split timers validated the architecture hypothesis. Next session must **research + stack** Tier S optimizations.

---

## Failed / deferred attempts

### Dual-core row split
- **Idea:** Core0 low rows 0..mid, Core1 mid..end
- **Failed:** Row `mid` upscale needs row `mid-1` and `mid` — seam dependency
- **Deferred fix:** Split by **pixel index** not row

### 96 KiB internal heap
- **Goal:** Fit `low_rgb565` in heap → internal SRAM
- **Failed:** `dram2_seg overflowed by 24560 bytes`
- **Next:** `#[ram(reclaimed)] static` — see `05-MEMORY-LINKER.md`

### PSRAM direct DMA (zero-copy flush)
- **Idea:** DMA directly from PSRAM framebuffer
- **Blocked:** `SpiDmaBus` copies to internal `tx_buf` always in esp-hal 1.1.1
- **Stretch:** Lower-level `SpiDma::half_duplex_write` with PSRAM `DmaTxBuf`

### ESP32-P4 PPA hardware upscale
- **Not available** on ESP32-S3

### Release profile default
- `opt-level = 3` in both dev and release — user often flashes dev profile
- **Recommend:** always benchmark with `--release`

---

## Optimization leverage summary

| Attempt | Eval impact | Upscale impact | Quality | Verdict |
|---------|-------------|----------------|---------|---------|
| Static cache | ✅ huge init, ✅ per-frame | — | Neutral | Keep |
| 9× frame_cos only | ✅ | — | Neutral | Keep |
| Row staging | ✅ small | — | Neutral | Keep |
| SoA layout | ✅ small | — | Neutral | Keep |
| div=4 | ✅ 4× pixels↓ | — | ❌ Bad | Reject |
| div=3 | ✅ 2.25× pixels↓ | — | ✅ OK | Current |
| Byte FB + 8K DMA | — | — | Neutral | ✅ flush 25ms |
| Fused upscale | — | ❌ hidden 800ms | Neutral | Removed |
| Integer upmap | — | ✅ intended | Neutral | Keep — verify |
| Q14 eval | ✅ moderate | — | Neutral | Keep — tune |
| Dual-core rows | — | — | — | ❌ Wrong split |

---

## What "10× FPS" requires from here

| From | To | Needed change |
|------|-----|---------------|
| 0.6 FPS (1667 ms) | 6 FPS (167 ms) | **10× total** or **5× render** if flush 25ms |
| 1.0 FPS (1000 ms) | 10 FPS (100 ms) | **10× total** |

Render budget at 6 FPS: **~142 ms** (after 25ms flush).

Current render ~800–1600 ms → need **5.6–11×** render speedup.

**Cumulative targets:**
- upscale: 800 → 30 ms (**27×**) — via internal SRAM + integer upmap
- eval: 800–1400 → 80–100 ms (**8–17×**) — via dual-core + i32 + div tuning

---

*Appendix document — Turn 3 of 3 — see [`README.md`](README.md)*