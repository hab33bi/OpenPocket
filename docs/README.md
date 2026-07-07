# Pocket Watch Smoke Test — Documentation Index

> **Repo:** `C:\Users\USER\pocket-watch-smoke-test`  
> **Board:** Waveshare ESP32-S3-Touch-AMOLED-1.75 (466×466 CO5300 QSPI AMOLED)  
**SoC note:** ESP32-S3R8 — 512 KiB SRAM + 384 KiB ROM + **stacked 8 MB PSRAM** + 16 MB external Flash (per official Waveshare docs)  
> **Goal:** Raidal-2 WebGL aurora shader at **~6–10 FPS** with **live math** and **WebGL visual parity**  
> **Status (Jul 2026):** Display works. Shader runs at ~0.6–1 FPS. Flush solved (~25 ms). Eval/upscale still dominate.

---

## Start here

| Audience | First file |
|----------|------------|
| **New AI model (continuation)** | [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md) — copy the prompt block into a new chat |
| **Human developer** | [`01-PROJECT-HANDOFF.md`](01-PROJECT-HANDOFF.md) — full project bible |
| **Performance engineer** | [`02-ANIMATION-BOTTLENECK.md`](02-ANIMATION-BOTTLENECK.md) — why it's slow and what to try next |

---

## Document map

| # | File | Purpose | When to read |
|---|------|---------|--------------|
| 01 | [`01-PROJECT-HANDOFF.md`](01-PROJECT-HANDOFF.md) | Hardware, pins, PMIC, QSPI, repo layout, bring-up history, module reference, footguns | Always — foundation |
| 02 | [`02-ANIMATION-BOTTLENECK.md`](02-ANIMATION-BOTTLENECK.md) | 10× FPS math, eval vs upscale vs flush, ranked optimization tiers, dual-core notes | Before changing render path |
| 03 | [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md) | **Master entry prompt** for higher-capability model with links, success criteria, decision tree | Paste into new chat |
| 04 | [`04-SHADER-MATH-MAPPING.md`](04-SHADER-MATH-MAPPING.md) | GLSL ↔ Rust fidelity contract; safe vs unsafe optimizations | Before touching `raidal.rs` math |
| 05 | [`05-MEMORY-LINKER.md`](05-MEMORY-LINKER.md) | Buffer sizes, PSRAM vs internal SRAM, `dram2_seg` overflow, `low_rgb565` placement | Before allocator/linker changes |
| 06 | [`06-OPTIMIZATION-CHRONOLOGY.md`](06-OPTIMIZATION-CHRONOLOGY.md) | Every optimization attempt, timings, lessons — avoid repeating failures | Before proposing "new" ideas |
| 07 | [`07-PREBAKE-PIPELINE.md`](07-PREBAKE-PIPELINE.md) | Video/frame-loop fallback spec (back-burner only) | If live math cannot hit 6 FPS |

---

## Recommended reading order (new model)

```
03-FOLLOWUP-PROMPT.md     ← mission + constraints (5 min)
    ↓
02-ANIMATION-BOTTLENECK.md ← bottleneck physics (15 min)
    ↓
06-OPTIMIZATION-CHRONOLOGY.md ← what already failed (10 min)
    ↓
05-MEMORY-LINKER.md        ← if upscale_ms high (10 min)
04-SHADER-MATH-MAPPING.md  ← if changing eval math (10 min)
01-PROJECT-HANDOFF.md      ← reference as needed (skim TOC)
07-PREBAKE-PIPELINE.md     ← only if live path stalls
```

---

## Code entry points

| File | Role |
|------|------|
| `src/bin/main.rs` | Init, PMIC, display, loop, `eval=` / `upscale=` / `flush=` logging |
| `src/raidal.rs` | Two-pass render: Q14 `eval_pass` → `upscale_pass` |
| `src/qspi_bus.rs` | CO5300 QSPI, `flush_bytes` 8 KiB DMA chunks |
| `build.rs` | `SIN_LUT` + `SIN_LUT_I16` Q14 tables |
| `src/plasma.rs` | Legacy demo — **not** active renderer |

---

## Performance baseline (confirmed Gen 5, `--release`, div=3)

```
fps~1.0 eval=798ms upscale=217ms flush=25ms total=1042ms
```

| Build | eval | upscale | flush | total | FPS | Quality |
|-------|------|---------|-------|-------|-----|---------|
| div=2 fused | ~3975 ms | (fused) | 38 ms | ~4013 ms | 0.2 | Good |
| div=4 extreme | ~1599 ms | (fused) | 25 ms | ~1625 ms | 0.6 | **Degraded** |
| **div=3 Gen 5 (current)** | **798 ms** | **217 ms** | **25 ms** | **1042 ms** | **1.0** | OK |

**10× target:** `fps~≥10.0 eval=<80ms upscale=<22ms flush=~25ms total=<104ms`

**Bottleneck:** eval (77%) then upscale (21%). Flush solved. See [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md) for research mandate and decision tree.

---

## Flash command

```powershell
cargo espflash flash --release --monitor
```

Port: COM4 on user's machine (adjust `--port` as needed).

---

*Documentation set — Turn 3 of 3 — Jul 2026*