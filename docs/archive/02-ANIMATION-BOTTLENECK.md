# Raidal-2 Animation Bottleneck — Problem Analysis

> **Companion to:** [`01-PROJECT-HANDOFF.md`](01-PROJECT-HANDOFF.md)  
> **Doc index:** [`README.md`](README.md) · **Entry prompt:** [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md)  
> **Audience:** A higher-capability model tasked with achieving **~10× FPS** at **equal or better** visual quality vs current build.  
> **User goal:** Keep live mathematical shader (not video playback) as primary path; video pre-bake is fallback only.

---

## Table of contents

1. [Problem statement](#1-problem-statement)
2. [What "10× FPS" actually means numerically](#2-what-10-fps-actually-means-numerically)
3. [The three clocks in every frame](#3-the-three-clocks-in-every-frame)
4. [Why lowering resolution failed to help](#4-why-lowering-resolution-failed-to-help)
5. [Work accounting — operations per frame](#5-work-accounting--operations-per-frame)
6. [Memory hierarchy — the hidden multiplier](#6-memory-hierarchy--the-hidden-multiplier)
7. [Current pipeline cost model](#7-current-pipeline-cost-model)
8. [What we already solved (do not re-litigate)](#8-what-we-already-solved-do-not-re-litigate)
9. [What is still slow and why](#9-what-is-still-slow-and-why)
10. [The 10× FPS roadmap — ranked by leverage](#10-the-10-roadmap--ranked-by-leverage)
11. [Dual-core — what works and what does not](#11-dual-core--what-works-and-what-does-not)
12. [Video pre-bake — honest trade study](#12-video-pre-bake--honest-trade-study)
13. [Success metrics for the next model](#13-success-metrics-for-the-next-model)
14. [Open questions / hypotheses to test](#14-open-questions--hypotheses-to-test)

---

## 1. Problem statement

We have a **working** port of the Raidal-2 WebGL shader on an ESP32-S3 driving a 466×466 AMOLED at ~**0.6–1.0 FPS**. The animation is **beautiful but frozen in slow motion**. The user wants approximately **10× throughput** (~6–10 FPS) **without** the visible quality regression seen at `RENDER_DIVISOR=4`, and **prefers** continuing live shader evaluation over pre-rendered video — though video remains a back-burner escape hatch.

The failure mode is **not** display output bandwidth anymore. It is **compute and memory access** in the shader pipeline.

---

## 2. What "10× FPS" actually means numerically

| Current | 10× target |
|---------|------------|
| ~0.6 FPS → ~1667 ms/frame | ~6 FPS → **~167 ms/frame** |
| ~1.0 FPS → ~1000 ms/frame | ~10 FPS → **~100 ms/frame** |

**Hard floor — DMA flush:**

```
flush ≈ 25 ms  →  max FPS ≈ 1000/25 = 40 FPS (render = 0)
```

So display IO is **not** the 10× problem. The budget for **eval + upscale** at 6 FPS is:

```
167 ms total − 25 ms flush ≈ 142 ms for render
```

At 10 FPS:

```
100 ms − 25 ms ≈ 75 ms for render
```

**Current render** is ~800–1600 ms. Required speedup on render alone:

```
800 / 142 ≈ 5.6×  (to hit 6 FPS)
1600 / 142 ≈ 11×  (if eval still dominates at 1400ms)
```

The user said "10× FPS" which from 0.6 FPS means **~6 FPS**, requiring roughly **5–12× reduction in eval+upscale** depending on which end of current performance we're at.

---

## 3. The three clocks in every frame

Every frame has three independent timers (now explicitly logged):

```
total ≈ eval_ms + upscale_ms + flush_ms + small overhead
```

### 3.1 `eval_ms` — Pass A (scales with RENDER_DIVISOR²)

- Grid size: `low_w × low_h` where `low_w = ceil(466 / div)`
- div=2 → 233² = **54,289** pixels
- div=3 → 156² = **24,336** pixels
- div=4 → 117² = **13,689** pixels

Per pixel work (qualitatively constant):

- 9 layers × (~4 LUT + smoothstep + multiply) ≈ **36 trig LUT touches**
- Plus 3× `tanh`, RGB565 pack

**Scales down** when div increases.

### 3.2 `upscale_ms` — Pass B (scales with OUTPUT resolution only)

- **Always** 466 × 466 = **217,156** output pixels
- Each: 4× RGB565 gathers + integer bilinear + 2 byte writes

**Does not scale** with RENDER_DIVISOR (this was the key discovery).

### 3.3 `flush_ms` — DMA (solved)

- 424 KiB over QSPI @ 80 MHz, 8 KiB chunks
- **~25 ms**, stable

---

## 4. Why lowering resolution failed to help

### Experiment: div=4

| Metric | div=2 (est.) | div=4 (measured) |
|--------|--------------|------------------|
| eval | ~3200 ms | ~800 ms |
| upscale | ~800 ms | ~800 ms |
| total render | ~4000 ms | ~1600 ms |

**Speedup:** 4000/1600 = **2.5×**, not 4×, because upscale is constant.

**Quality:** User reported **visible degradation** — blocky aurora filaments on 466px round display.

### Lesson

`RENDER_DIVISOR` is a **quality knob**, not a master FPS knob, until upscale is ≪ eval.

After two-pass + upmap (Gen 3), upscale *should* drop to tens of ms — **verify with user's `upscale_ms` log**. If eval still ~1200ms at div=3, total remains ~1.2s → still ~0.8 FPS.

---

## 5. Work accounting — operations per frame

### 5.1 Eval pass (div=3)

```
24,336 pixels × 9 layers × ~4 LUT = ~875,000 LUT operations/frame
+ 24,336 × 3 tanh
+ 24,336 row copies from PSRAM (156 rows × ~10 KiB row_packed)
```

At 240 MHz, **theoretical** minimum if 1 cycle/op:

```
875k ops → 3.6 ms  (impossibly optimistic)
```

**Measured** ~800–1400 ms → **~1000–200 cycles per LUT-equivalent op** effective — classic **PSRAM latency + cache miss + float/i64** penalty.

### 5.2 Upscale pass (Gen 3 integer)

```
217,156 pixels × (4× u16 load + ~15 integer ops)
```

If 50 cycles/pixel → 10.8M cycles → **45 ms** (plausible target)

If still ~800 ms, either:
- Gen 3 not flashed / old fused path still running
- PSRAM gather dominates (low_rgb565 in PSRAM)
- upmap iteration overhead

**Action:** Read `upscale_ms` from serial — this single number validates the entire Gen 3 hypothesis.

### 5.3 Flush

```
424 KiB / 25 ms ≈ 17 MB/s effective (reasonable for QSPI + overhead)
```

---

## 6. Memory hierarchy — the hidden multiplier

| Memory | Size | Latency | Current use |
|--------|------|---------|-------------|
| Internal SRAM | 512 KiB total (board has +384 KiB ROM) | **fast** | DMA buf, scratch row_pack (~7 KiB), reclaimed statics |
| PSRAM | **stacked 8 MB** (ESP32-S3R8) | **5–20× slower random** | row_packed, low_rgb565 (before move), upmap, framebuffer |

### Critical placement decisions

| Buffer | Size @ div=3 | Where it lives | Impact |
|--------|--------------|----------------|--------|
| `row_packed` | ~1 MiB | PSRAM | 1 sequential row copy/scanline — OK |
| `low_rgb565` | 48 KiB | PSRAM (won't fit 48KiB heap) | **4 random reads per output pixel in upscale** |
| `upmap` | 2.5 MiB | PSRAM | Sequential read in upscale — OK |
| `framebuffer` | 424 KiB | PSRAM | Sequential DMA chunk read — OK |

**Hypothesis:** Moving `low_rgb565` (48 KiB) to internal SRAM could cut upscale_ms dramatically. Failed attempt: 96 KiB heap → linker overflow. **Next model should explore:**

- Static `#[ram(reclaimed)]` array for `low_rgb565` outside heap
- Shrink DMA buffers if possible
- Partial internal placement via explicit section attributes in esp-hal

---

## 7. Current pipeline cost model

```
T_frame = T_cos + T_eval + T_upscale + T_flush

T_cos     ≈ 9 LUT calls        ≈ negligible
T_flush   ≈ 25 ms                ≈ fixed
T_upscale ≈ f(217156, PSRAM gathers from low_rgb565)
T_eval    ≈ g(24336, 9 layers, PSRAM row staging, i64 math)
```

**Dominant term today:** `T_eval` (unless upscale wasn't actually fixed).

**To reach 142 ms render budget:**

| Component | Budget |
|-----------|--------|
| eval | ≤ 100 ms |
| upscale | ≤ 30 ms |
| overhead | ≤ 12 ms |

---

## 8. What we already solved (do not re-litigate)

| Issue | Solution | Status |
|-------|----------|--------|
| Black screen | QSPI quad, PMIC, col offset 6 | ✅ Done |
| Slow flush (38+ ms) | 8 KiB DMA, BE byte FB | ✅ ~25 ms |
| Per-frame sqrt/atan | Static cache in init | ✅ Done |
| Per-frame cos(i-t) × pixels | 9 trig ops/frame only | ✅ Done |
| Float upscale in fused path | Two-pass + upmap | ✅ Code exists — verify timing |
| div=4 quality loss | Reverted to div=3 | ✅ Done |

**Do not waste time on:** SPI bit-banging, PMIC re-discovery, CASET offset, basic QSPI protocol.

---

## 9. What is still slow and why

### 9.1 Eval pass — structural causes

1. **Still O(low_w × low_h × 9 × LUT)** — fundamental to shader fidelity
2. **i64 accumulators** in inner loop — register pressure on Xtensa
3. **Per-pixel inner loop** not vectorized — S3 has no FP SIMD
4. **PSRAM row_packed read** each scanline — 156 × ~10 KiB copy — probably OK
5. **Q14 LUT** helps vs float but not enough alone for 10×
6. **No dual-core** — second CPU idle

### 9.2 Upscale pass — if still slow

1. `low_rgb565` in PSRAM → cache misses on gather
2. 217k loop with no strip optimization
3. `upmap` struct size (12 bytes) × 217k cache footprint

### 9.3 Algorithmic floor

WebGL runs same math on **217k pixels** per frame on a GPU with thousands of parallel cores. We run **24k–54k** eval pixels + **217k** upscale pixels on **one** 240 MHz core.

**Math parity at 6 FPS requires ~100× effective speedup vs naive full-res port** — already partially achieved (4000ms → ~1000ms = 4×). Need another **5–10×**.

---

## 10. The 10× FPS roadmap — ranked by leverage

### Tier S — Do first (qual-neutral, high quant gain)

| # | Optimization | Expected gain | Risk |
|---|--------------|---------------|------|
| S1 | **Confirm `upscale_ms` < 100 ms** on Gen 3 build | Proves architecture | User may still be on old binary |
| S2 | **`low_rgb565` in internal SRAM** (not heap) | 2–5× upscale | Linker layout |
| S3 | **Dual-core eval split by pixel range** (no row seam) | ~1.8× eval | Sync complexity |
| S4 | **Tighter fixed-point** — i32 only, i64 out of inner loop | 1.5–3× eval | Color drift if wrong |
| S5 | **Release + LTO** always for perf tests | 1.2–1.5× | — |

### Tier A — Strong candidates

| # | Optimization | Expected gain | Risk |
|---|--------------|---------------|------|
| A1 | **Strip upscale by output rows** with row-local upmap slice | Cache locality | Code complexity |
| A2 | **Eval‖upscale pipeline** — upscale scanlines as eval completes | 1.3–1.8× wall time | Double buffering |
| A3 | **Reduce LUT calls** — reuse `cos_a` across channels where algebra permits | 1.1–1.2× | Must preserve math |
| A4 | **div=2 with fast upscale** if eval fast enough | Quality ↑ | eval budget |

### Tier B — Medium

| # | Optimization | Expected gain | Risk |
|---|--------------|---------------|------|
| B1 | Async/non-blocking DMA flush while computing next frame | Hides 25ms flush | esp-hal async SPI |
| B2 | Direct PSRAM DMA from framebuffer (bypass copy) | 2–5 ms flush | HAL changes |
| B3 | ESP-NN / SIMD assembly kernels for eval | Unknown | High effort |

### Tier C — Quality trade (user rejected so far)

| # | Optimization | Expected gain | Risk |
|---|--------------|---------------|------|
| C1 | div=4+ | 2–4× eval | **Visible** |
| C2 | 7 layers instead of 9 | ~1.2× | Flat aurora |
| C3 | Skip per-channel tanh | small | Color wrong |

### Tier D — Nuclear option (back-burner)

| # | Optimization | Expected gain | Risk |
|---|--------------|---------------|------|
| D1 | **Pre-baked shader video loop** in flash | **10–40 FPS** | Not live math |
| D2 | **Delta compression** between frames | Storage ↓ | Decode CPU |
| D3 | **SD card stream** for long loops | Unlimited length | IO + power |

---

## 11. Dual-core — what works and what does not

### Failed approach: row-based split with handoff

- Core 0 renders low rows 0..mid
- Core 1 renders mid..end
- **Problem:** bilinear upscale at row `mid` needs **both** row `mid-1` and `mid` — seam dependency prevents true parallelism without pipeline staging

### Viable approach: eval pixel index split

```
total_low_pixels = low_w * low_h
core0: eval pixels [0, N/2)
core1: eval pixels [N/2, N)
```

No seam — disjoint writes to `low_rgb565`. Then single-core or parallel upscale (upscale reads complete buffer).

### Viable approach: frame pipeline

```
Frame N:   flush framebuffer(N-1) while eval framebuffer(N)
```

Hides 25 ms flush if eval > 25 ms.

---

## 12. Video pre-bake — honest trade study

### When it wins

- User accepts non-live shader
- Short seamless loop (2–4 s of aurora cycle)
- FPS becomes flush-limited (**25–40 FPS** possible)

### Pipeline (offline)

```
WebGL Raidal-2 @ 466×466 → PNG/frame or raw RGB565
→ quantize / delta-encode → flash partition or SD file
→ device: timer ISR → DMA blit frame[n] → panel
```

### Storage (raw, no compression)

| Duration | FPS | Size |
|----------|-----|------|
| 2 s | 30 | 424KiB × 60 = **25 MiB** |
| 4 s | 30 | **50 MiB** |
| 10 s | 30 | **127 MiB** |

16 MB flash can hold ~**1.2 s** raw @ 30 FPS. Need compression or SD for longer.

### Hybrid (recommended if math path stalls)

- Pre-bake **one loop** on PC with **exact** WebGL
- Device plays loop at full speed
- Optional: crossfade at seam

**User preference:** Optimize math first.

---

## 13. Success metrics for the next model

### Required serial output

```
fps~X eval=Yms upscale=Zms flush=25ms total=Wms
```

### Phase gates

| Gate | Pass condition |
|------|----------------|
| G1 | `upscale_ms < 100 ms` at div=3 |
| G2 | `eval_ms < 300 ms` at div=3 |
| G3 | `total < 170 ms` → **≥6 FPS** |
| G4 | Visual WebGL parity checklist (see handoff §9.3) |

### Quality checklist (must pass)

- [ ] No 4×4 blockiness (div=4 artifact gone)
- [ ] Aurora rotates smoothly
- [ ] Teal / violet / gold separation visible
- [ ] Soft band edges (not hard rings)
- [ ] tanh highlight roll-off

---

## 14. Open questions / hypotheses to test

1. **What are actual Gen 3 `eval_ms` and `upscale_ms`?** User reported "slight improvements" but didn't paste split timings — critical missing data.

2. **Is eval still >> upscale?** If yes, focus Tier S3–S4. If upscale still ~800ms, Gen 3 not active or `low_rgb565` placement is the bug.

3. **Can `low_rgb565` fit internal via `#[ram(reclaimed)] static mut LOW: [u16; 24336]`** without heap growth?

4. **Does eval speed scale linearly with div?** If div=2 at 54k pixels takes 3× div=3 at 24k, eval is compute-bound not memory-bound.

5. **Would div=2 at 6 FPS be acceptable quality if eval hits 75ms?** Quality ↑ vs div=3.

6. **Is `upmap` 2.5 MiB causing cache thrash** when iterated with `low_rgb565`? Consider row-sliced upmap.

7. **ESP32-S3 SIMD / esp-dsp** — any applicable fixed-point kernels?

---

## Summary sentence for the next model

> The animation is slow because **~875k–2.4M LUT-heavy ops per frame** run on a **single 240 MHz core** with **PSRAM-backed buffers**, while **flush is already solved at 25 ms**. Achieving **6–10 FPS** requires **another 5–12× cut in eval+upscale** without dropping below **div=3** quality — via **internal SRAM placement, dual-core eval, tighter fixed-point, and pipelining** — or accepting **pre-baked video** for flush-limited playback.

---

*Document version: Turn 3 of 3 — Jul 2026*  
*See [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md) for the copy-paste continuation prompt.*