# Pocket Watch Smoke Test — Complete Project Handoff

> **Purpose:** Transfer all knowledge from the Raidal-2 ESP32-S3 bring-up sessions to a higher-capability model or future developer.  
> **Board:** Waveshare [ESP32-S3-Touch-AMOLED-1.75](https://www.waveshare.com/esp32-s3-touch-amoled-1.75.htm)  
> **Goal:** Run the [21st.dev Raidal-2](https://21st.dev) WebGL aurora shader on-device at maximum FPS without visible quality loss.  
> **Status (Jul 2026):** Display works. Shader runs. ~0.6–1 FPS. Flush solved (~25 ms). Eval still dominates.

**Documentation index:** [`docs/README.md`](README.md)  
**Continuation prompt:** [`docs/03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md)  
**Companion docs:** [`02-ANIMATION-BOTTLENECK.md`](02-ANIMATION-BOTTLENECK.md) · [`04-SHADER-MATH-MAPPING.md`](04-SHADER-MATH-MAPPING.md) · [`05-MEMORY-LINKER.md`](05-MEMORY-LINKER.md) · [`06-OPTIMIZATION-CHRONOLOGY.md`](06-OPTIMIZATION-CHRONOLOGY.md) · [`07-PREBAKE-PIPELINE.md`](07-PREBAKE-PIPELINE.md)

---

## Table of contents

1. [Executive summary](#1-executive-summary)
2. [Hardware reference](#2-hardware-reference)
3. [Software stack](#3-software-stack)
4. [Repository layout](#4-repository-layout)
5. [Display bring-up — root causes of black screen](#5-display-bring-up--root-causes-of-black-screen)
6. [CO5300 QSPI protocol](#6-co5300-qspi-protocol)
7. [Memory architecture](#7-memory-architecture)
8. [DMA and framebuffer — how pixels reach the panel](#8-dma-and-framebuffer--how-pixels-reach-the-panel)
9. [Raidal-2 shader — WebGL fidelity requirements](#9-raidal-2-shader--webgl-fidelity-requirements)
10. [Render pipeline evolution](#10-render-pipeline-evolution)
11. [Current architecture (as of latest code)](#11-current-architecture-as-of-latest-code)
12. [Module and function reference](#12-module-and-function-reference)
13. [Decision log — what we tried and why](#13-decision-log--what-we-tried-and-why)
14. [Measured performance timeline](#14-measured-performance-timeline)
15. [Qualitative vs quantitative optimization framework](#15-qualitative-vs-quantitative-optimization-framework)
16. [Back-burner: pre-baked video frames](#16-back-burner-pre-baked-video-frames)
17. [Build, flash, debug](#17-build-flash-debug)
18. [Known issues and footguns](#18-known-issues-and-footguns)
19. [Turn 2 / 3 — remaining doc sections](#19-turn-23--remaining-doc-sections)

---

## 1. Executive summary

This project is a `no_std` Rust firmware for a round 466×466 AMOLED pocket display. After solving a **black screen** (wrong QSPI protocol, missing PMIC rails, column offset), we ported the **Raidal-2** fragment shader — a 9-layer radial aurora with `atan2`, `smoothstep`, per-channel `sin`, and `tanh` tone mapping.

**What works:**
- AXP2101 PMIC powers the panel
- CO5300 init over QSPI @ 80 MHz
- DMA pixel flush (~25–38 ms per full frame)
- Faithful shader math with static caching and Q14 fixed-point eval
- Integer bilinear upscale via precomputed `UpPixel` map

**What does not work yet:**
- Target FPS (want 10× improvement → ~6–10 FPS minimum; stretch 30+ FPS)
- `eval_pass` still ~800–1400 ms depending on divisor and optimization generation
- 60 FPS is **physically impossible** at full-frame updates: flush alone caps ~25–40 FPS

**Core lesson:** Lowering `RENDER_DIVISOR` only shrinks shader **eval** cost. Any path that upscales or touches all **466×466 output pixels** per frame has a ~800 ms floor unless the upscale pass itself is replaced or eliminated.

---

## 2. Hardware reference

### 2.1 Board identity

| Field | Value |
|-------|-------|
| Product | Waveshare ESP32-S3-Touch-AMOLED-1.75 |
| SKU | 31261 (variants 31262, 31264) |
| Wiki | https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.75 |
| Display | 1.75" capacitive touch AMOLED, **466×466**, 16.7M colors |
| Driver IC | **CO5300** (QSPI), CST9217 touch (I2C, not wired in this project) |
| PMIC | **AXP2101** @ I2C `0x34` |

### 2.2 SoC and memory

**Official specs (from Waveshare docs):**  
"Built-in 512KB SRAM and 384KB ROM, with **stacked 8MB PSRAM** and external 16MB Flash".  
SoC is **ESP32-S3R8**.

| Resource | Spec | Implication |
|----------|------|-------------|
| SoC | **ESP32-S3R8** — dual Xtensa LX7 @ **240 MHz** | No GPU. FP unit exists but PSRAM latency hurts. |
| Internal SRAM | **512 KiB** (total) + 384 KiB ROM | DMA descriptors, reclaimed heap/statics, stack, hot scratch only. Very tight for large internal buffers. |
| PSRAM | **stacked 8 MB** (octal / OPI via internal interface) | Framebuffer, static cache (`row_packed`), upmap, large `Vec`s. Slower random access. |
| Flash | **external 16 MB** | Firmware + room for pre-baked assets later |
| GDMA | `dma_can_access_psram` **yes** on S3 | DMA *can* read PSRAM, but esp-hal `SpiDmaBus` copies chunks to internal TX buf first |

### 2.3 Confirmed GPIO pinout (display)

| Signal | GPIO | Notes |
|--------|------|-------|
| LCD_CS | **12** | Manual CS (not SPI peripheral CS) |
| SCK | **38** | |
| SIO0 (MOSI) | **4** | Quad line 0 |
| SIO1 (MISO) | **5** | Quad line 1 |
| SIO2 | **6** | **Required** for quad — was missing in early attempts |
| SIO3 | **7** | **Required** for quad |
| LCD_RESET | **39** | Active-low reset sequence |
| I2C SDA | **15** | AXP2101 |
| I2C SCL | **14** | AXP2101 |

### 2.4 AXP2101 power rails

Display will stay **black** without this. See `axp2101_enable_display_power()` in `src/bin/main.rs`:

- **DC1** @ 3.3 V (register `0x82` = 18, enable bit in `0x80`)
- **ALDO1** @ 3.3 V (register `0x92` = 28, enable bit in `0x90`)

---

## 3. Software stack

| Component | Version | Role |
|-----------|---------|------|
| Rust | 1.88+ (edition 2024) | |
| Target | `xtensa-esp32s3-none-elf` | via espup toolchain |
| esp-hal | ~1.1.0, features `esp32s3`, **`unstable`** | SPI DMA, PSRAM, GPIO, I2C |
| esp-alloc | 0.10 | Dual heap: internal + PSRAM |
| esp-println | 0.13 | Serial monitor |
| libm | 0.2 | `no_std` trig for init / static cache build |
| esp-bootloader-esp-idf | 0.5 | Bootloader only |

**Not used:** ESP-IDF app code, LVGL, Embassy, FreeRTOS (bare-metal loop).

**Reference project:** [waveshare-watch-rs](https://github.com/infinition/waveshare-watch-rs) — Rust `no_std` firmware for a similar CO5300 board (2.06"); validated QSPI DMA patterns.

---

## 4. Repository layout

```
pocket-watch-smoke-test/
├── Cargo.toml              # deps, opt-level 3 dev+release
├── build.rs                # sin LUT f32 + i16 Q14, linker script
├── rust-toolchain.toml     # esp toolchain
├── src/
│   ├── lib.rs              # exports plasma, qspi_bus, raidal
│   ├── bin/main.rs         # entry: init, loop, timing
│   ├── qspi_bus.rs         # CO5300 QSPI DMA transport
│   ├── raidal.rs           # Raidal-2 shader + two-pass render
│   └── plasma.rs           # earlier plasma demo (superseded, still in tree)
└── docs/
    ├── 01-PROJECT-HANDOFF.md      # this file
    ├── 02-ANIMATION-BOTTLENECK.md # problem deep-dive
    └── 03-FOLLOWUP-PROMPT.md      # prompt for next model (Turn 2)
```

---

## 5. Display bring-up — root causes of black screen

All four were required. Fixing only some still produced a black panel.

### 5.1 Wrong SPI mode (fatal)

`Spi::write()` single-line SPI **is not** CO5300 QSPI. The panel needs:

- **Register writes:** `DataMode::Single`, cmd `0x02`, addr `(reg << 8)`
- **Pixel stream:** `DataMode::Quad`, cmd `0x32`, addr `0x003C00`, continuation chunks with `Command::None` / `Address::None`

### 5.2 Missing quad lines

GPIO 6 and 7 (SIO2/SIO3) must be connected. MOSI/MISO alone = no image.

### 5.3 PMIC not enabled

AXP2101 must enable DC1 + ALDO1 before the CO5300 powers on.

### 5.4 Column offset

CASET must use offset **6**: columns `0x0006 .. 0x01D1` (466 pixels). Full window `0x0000` misaligns on this round panel.

### 5.5 Init sequence (working)

See `main.rs` after reset. Critical steps:

1. Vendor unlock: `0xFE/0x20`, `0x19/0x10`, `0x1C/0xA0`, `0xFE/0x00`
2. `0xC4=0x80`, `0x3A=0x55` (RGB565), brightness max
3. CASET/PASET with col offset 6
4. **600 ms** delays around SLPOUT (`0x11`) and DISPON (`0x29`)
5. Per frame: re-issue `0x2C` (RAMWR) then pixel stream

---

## 6. CO5300 QSPI protocol

Implemented in `src/qspi_bus.rs`:

```
Register:  CS↓ → cmd 0x02 (single) → addr (reg<<8, single) → data → CS↑
Pixels:    CS↓ → cmd 0x32 (single) → addr 0x003C00 (single) → quad data
           → continuation chunks: cmd None, addr None, quad data → CS↑
```

Pixel format: **RGB565 big-endian** per halfword (high byte first on wire).

---

## 7. Memory architecture

**Board spec (official):** 512 KiB SRAM + 384 KiB ROM + **stacked 8 MB PSRAM** + 16 MB external Flash.

```
┌─────────────────────────────────────────────┐
│ Internal SRAM (512 KiB total)              │
│  • DMA TX/RX buffers: 8 KiB each            │
│  • Scratch.row_pack: ~7 KiB @ div=3         │
│  • Reclaimed for #[ram] statics (e.g. LOW)  │
│  • DMA descriptor chains                    │
└─────────────────────────────────────────────┘
         │ chunk copy (HAL requirement)
         ▼
┌─────────────────────────────────────────────┐
│ PSRAM (stacked 8 MB)                        │
│  • framebuffer: 466×466×2 = 424 KiB BE     │
│  • row_packed static: ~1 MiB @ div=3       │
│  • upmap: 217156 × 12 B ≈ 2.5 MiB        │
│  • (low_rgb565 moved to internal SRAM)     │
└─────────────────────────────────────────────┘
         │ GDMA @ 80 MHz QSPI
         ▼
      CO5300 AMOLED
```

**Allocator config** (`main.rs`):

```rust
esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 48 * 1024);
esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);
```

**Footgun:** Increasing internal heap to 96 KiB **overflowed `dram2_seg` by ~24 KiB** at link time. Do not grow internal heap without checking linker map.

**Footgun:** Large `Vec` allocations go to PSRAM when they exceed internal heap. `low_rgb565` (~48 KiB) does **not** fit in 48 KiB heap alongside DMA buffers → lives in PSRAM → upscale gathers are PSRAM-random-access bound.

---

## 8. DMA and framebuffer — how pixels reach the panel

### 8.1 What DMA does here

**DMA (Direct Memory Access)** lets the SPI peripheral stream bytes from RAM without the CPU moving each byte. Flow:

1. CPU renders into **PSRAM framebuffer** (RGB565 BE bytes)
2. CPU kicks `QspiBus::flush_bytes()` 
3. For each 8 KiB chunk: copy to internal DMA scratch → `SpiDmaBus::half_duplex_write` → GDMA → QSPI pins
4. CPU spins until chunk completes (`wait_for_idle`)

### 8.2 Why flush is no longer the bottleneck

| Generation | Flush time | Notes |
|------------|------------|-------|
| Early FIFO | hundreds of ms | 64-byte blocking writes |
| 4 KiB DMA chunks | ~38 ms | |
| 8 KiB DMA + BE framebuffer | **~25 ms** | eliminated per-pixel byte swap |

**Theoretical flush ceiling:** 424 KiB @ ~25 ms ≈ **40 FPS max** even with **zero** render time.

### 8.3 PSRAM DMA nuance

ESP32-S3 GDMA **can** access PSRAM (`dma_can_access_psram`). However, `esp-hal` `SpiDmaBus::half_duplex_write` **always** copies into an internal `tx_buf` first:

```rust
// esp-hal 1.1.1 spi/master/dma.rs (blocking wrapper)
self.tx_buf.as_mut_slice()[..buffer.len()].copy_from_slice(buffer);
```

So PSRAM → internal copy → DMA → SPI is unavoidable with current HAL API unless you drop to lower-level `SpiDma::half_duplex_write` with a PSRAM-resident `DmaTxBuf` (unexplored).

---

## 9. Raidal-2 shader — WebGL fidelity requirements

### 9.1 Source

21st.dev **Raidal-2** — 9-layer radial aurora, animated via `cos(i - t)`.

### 9.2 GLSL structure (simplified)

```glsl
vec2 p = FC.xy - r * 0.5;
for (float i, a; i++ < 9.0; ) {
  a = (i*i)/80.0 - length(p)/r.y;
  denom = max(a, -a*3.0) + 2.0/r.y;
  sm = smoothstep(cos(i-t), 2.0, cos(atan(p.y,p.x) + cos(i-t) + i*i));
  o += 0.03/denom * sm * (1.2 + sin(a + i + vec4(0,2,4,0)));
}
o = tanh(o);
```

### 9.3 Qualitative — must preserve for "looks like WebGL"

| Element | Why |
|---------|-----|
| 9 layers | Depth / parallax rings |
| `smoothstep` on `cos` | Soft band edges (signature) |
| Per-channel `sin` (+0,+2,+4) | Teal / violet / gold separation |
| `tanh` per channel | Highlight roll-off |
| Time-varying `cos(i-t)` | Rotation / life |
| Static `atan2`, `denom` | OK to cache (don't change look) |

### 9.4 Acceptable approximations (imperceptible on 466px)

| Approximation | Notes |
|---------------|-------|
| Eval grid div=2 or div=3 + bilinear upscale | Same as GPU render-to-texture + sample |
| 512-entry sin LUT | Standard |
| Q14 fixed-point eval | If carefully scaled |
| Integer RGB565 bilinear upscale | Matches GPU bilinear perceptually |

### 9.5 Rejected (visible quality loss)

| Change | User feedback |
|--------|---------------|
| **div=4** (117×117 eval) | Visible degradation, slight FPS gain |
| Fewer layers | Not tried — would flatten aurora |
| Frame skipping | Not tried — stutter |

---

## 10. Render pipeline evolution

### Gen 0 — Naive port
- Full-res float eval per pixel per frame
- PSRAM AoS struct cache, random access
- **~4000 ms/frame** (div=2)

### Gen 1 — Static cache + row staging
- SoA `atan_p` + layer-major `inv_denom`
- Row copy to internal SRAM
- Fused row eval + upscale
- Still **~4000 ms** — upscale hidden in "render"

### Gen 2 — "Extreme" (div=4)
- PSRAM byte framebuffer, 8 KiB DMA
- Packed rows, macro-unrolled layers
- **~1625 ms**, quality worse, flush **25 ms**

### Gen 3 — Two-pass + Q14 (current)
- Pass A: Q14 `eval_pass` → `low_rgb565`
- Pass B: precomputed `upmap` integer upscale
- Split timing: `eval_ms`, `upscale_ms`, `flush_ms`
- div=3 (156×156)
- **Slight improvement** over Gen 2; user wants **10× more FPS**

---

## 11. Current architecture (as of latest code)

```
anim_start.elapsed() → time_ms
        │
        ▼
update_frame_cos() ── 9× lut_cos_angle_q14 per frame (not per pixel)
        │
        ▼
┌───────────────────────────────────────┐
│ eval_pass()                           │
│  for each low row ly:                 │
│    copy row_packed[ly] → scratch      │
│    for each lx: eval_pixel_q14()      │
│      9 layers × LUT + smoothstep      │
│      → low_rgb565[ly*lw + lx]         │
└───────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────┐
│ upscale_pass()                        │
│  for each of 217156 UpPixel entries:  │
│    gather 4× RGB565 from low_rgb565   │
│    bilinear_rgb565() → BE bytes       │
│    → framebuffer[pix*2 .. pix*2+2]    │
└───────────────────────────────────────┘
        │
        ▼
flush_bytes(framebuffer) ── ~25 ms DMA QSPI
```

**Constants** (`main.rs`):

```rust
const RENDER_DIVISOR: u8 = 3;  // 156×156 eval grid
const LCD_WIDTH: u16 = 466;
const LCD_COL_OFFSET: u16 = 6;
```

---

## 12. Module and function reference

### 12.1 `src/bin/main.rs`

| Symbol | Purpose |
|--------|---------|
| `main()` | Init clocks, heaps, I2C/PMIC, SPI DMA, display, shader, loop |
| `render_timed()` | Times `eval_pass` + `upscale_pass` separately |
| `axp2101_enable_display_power()` | DC1 + ALDO1 @ 3.3 V |
| `delay_ms` / `delay_until` | Busy-wait timing; 60 FPS cap (no-op when slower) |

**Serial output format:**

```
fps~{ema} eval={eval_ms}ms upscale={upscale_ms}ms flush={flush_ms}ms total={total_ms}ms
```

### 12.2 `src/qspi_bus.rs`

| Symbol | Purpose |
|--------|---------|
| `QspiBus` | Wraps `SpiDmaBus` + manual CS |
| `DMA_CHUNK_BYTES` | 8192 |
| `write_command` / `write_c8d8` / `write_c8d16d16` | Register access (single SPI) |
| `flush_bytes` | Stream RGB565 BE bytes in 8 KiB DMA chunks (quad) |

### 12.3 `src/raidal.rs`

| Symbol | Purpose |
|--------|---------|
| `Raidal2Config` | `render_divisor`, `time_scale` |
| `Raidal2::new()` | Build static `row_packed`, `upmap`, alloc buffers |
| `init_time` / `update_time` | Update 9 `frame_cos_q14` values |
| `eval_pass` | Low-res Q14 shader → `low_rgb565` |
| `upscale_pass` | `upmap`-driven integer bilinear → byte FB |
| `eval_pixel_q14` | Hot loop: 9 layers, LUT, smoothstep, tanh |
| `build_upmap` | Precompute 217k `UpPixel` structs at init |
| `bilinear_rgb565` | Per-channel 5/6/5 weighted blend |
| `lut_sin_cos_q14` | Q14 sin with linear interp |
| `Scratch` | Internal `row_pack` staging buffer |

### 12.4 `build.rs`

| Output | Purpose |
|--------|---------|
| `SIN_LUT[512]` f32 | Legacy / reference |
| `SIN_LUT_I16[512]` i16 Q14 | Hot eval LUT |
| `linkall.x` | Linker script |

### 12.5 `src/plasma.rs`

Earlier velvet-aurora plasma effect. **Superseded** by `raidal.rs` but still exported from `lib.rs`. Safe to ignore or delete.

---

## 13. Decision log — what we tried and why

| # | Decision | Rationale | Outcome |
|---|----------|-----------|---------|
| 1 | QSPI quad + manual CS | CO5300 requirement | Display works |
| 2 | AXP2101 before display | Panel power | Required |
| 3 | CASET offset 6 | Waveshare 1.75 BSP | Correct alignment |
| 4 | PSRAM allocator for large buffers | 512 KiB internal too small | Works; latency cost |
| 5 | Static cache atan + inv_denom | Remove per-frame sqrt/atan | Good init-time trade |
| 6 | RENDER_DIVISOR=2 | Quality/speed balance | ~4 s/frame |
| 7 | DMA 4→8 KiB chunks | Fewer CS rounds | Flush 38→25 ms |
| 8 | Fused row upscale | Reduce passes | **Hidden 800ms upscale** |
| 9 | RENDER_DIVISOR=4 | Fewer eval pixels | Quality ↓, FPS ~0.6, upscale still ~800ms |
| 10 | Two-pass + upmap | Fix upscale bottleneck | upscale should drop; eval still heavy |
| 11 | Q14 fixed-point eval | Remove float from hot loop | Slight gain |
| 12 | Dual-core row split | 2× CPUs | **Deferred** — seam dependency on row N-1 |
| 13 | 96 KiB internal heap | Fit low_rgb in SRAM | **Link failed** dram2 overflow |
| 14 | div=3 over div=4 | User quality preference | Current setting |

---

## 14. Measured performance timeline

All from user serial logs unless noted.

| Config | render/eval | upscale | flush | total | FPS | Quality |
|--------|-------------|---------|-------|-------|-----|---------|
| div=2, early fused | ~3975 ms | (in render) | 38 ms | ~4013 ms | 0.2 | Good |
| div=4, extreme | ~1599 ms | (in render) | 25 ms | ~1625 ms | 0.6 | **Degraded** |
| div=3, two-pass Q14 | *pending user log* | *split timing* | ~25 ms | *TBD* | ~0.6–1? | Better than div=4 |

**Back-solved timing model (validates theory):**

- div=2: eval ~3200 ms + upscale ~800 ms ≈ 4000 ms
- div=4: eval ~800 ms + upscale ~800 ms ≈ 1600 ms
- **Upscale cost is independent of RENDER_DIVISOR** when output is always 466×466

---

## 15. Qualitative vs quantitative optimization framework

Use this when prioritizing next work:

**Only accept optimizations that are:**
- High quantitative gain (ms saved)
- Low/neutral qualitative cost (still looks like WebGL)

| Lever | Quant gain | Qual impact |
|-------|------------|-------------|
| Fix upscale (integer upmap) | **Huge** | Neutral |
| Fixed-point / tighter eval | Large | Neutral if Q15+ |
| div=3 vs div=2 | Medium | Minor softness |
| div=4+ | Large | **Visible — reject** |
| Dual-core eval | ~1.7× | Neutral |
| Dual-core eval‖upscale pipeline | ~1.5–2× | Neutral |
| Pre-baked video loop | **Massive** | Perfect if source is shader |
| Drop layers / skip tanh | Medium | **Visible — reject** |

**FPS targets (honest):**

| Target | Required total frame | Achievable? |
|--------|---------------------|-------------|
| 60 FPS | 16.7 ms | No (full-screen shader) |
| 30 FPS | 33 ms | No without pre-bake |
| 10 FPS | 100 ms | Maybe with eval < 75 ms |
| 6 FPS | 166 ms | Plausible post-optimization |
| 10× current (~6 FPS) | ~160 ms | **User's stated goal** |

---

## 16. Back-burner: pre-baked video frames

**Idea:** Offline-render Raidal-2 on PC (exact WebGL) → encode frame sequence → device plays back from flash/SD at DMA speed.

| Pros | Cons |
|------|------|
| 10–40 FPS trivial (flush-limited) | Not live mathematical shader |
| Pixel-perfect WebGL match | Storage: 424 KiB × frames (e.g. 10 s @ 30 FPS ≈ 1.2 GB raw) |
| CPU free for touch/UI | Loop seam / memory pressure |
| | Compression (RLE, delta, indexed) needed |

**Hybrid:** Pre-bake **one cycle** of animation (e.g. 2–4 s loop) + crossfade — common embedded trick.

**User preference:** Keep optimizing live shader first; keep video path as fallback.

**Full specification:** [`07-PREBAKE-PIPELINE.md`](07-PREBAKE-PIPELINE.md) — capture flow, compression, flash layout, firmware playback, hybrid modes.

**Storage math (raw RGB565):**

```
424 KiB/frame × 60 frames/s × 10 s = ~254 MiB (fits in 16 MB flash for short loop)
424 KiB × 300 frames (10 s @ 30 FPS) = ~127 MiB
```

---

## 17. Build, flash, debug

```powershell
# Dev (opt-level 3 in Cargo.toml dev profile)
cargo espflash flash --monitor

# Release (recommended for perf testing)
cargo espflash flash --release --monitor
```

**Port:** COM4 (user machine). Adjust for `espflash --port COMx`.

**Toolchain:** `rust-toolchain.toml` → `channel = "esp"` via espup.

---

## 18. Known issues and footguns

1. **`Spi::write()` ≠ QSPI** — always use `QspiBus` patterns
2. **GPIO 6/7 required** for quad pixels
3. **PMIC before pixels**
4. **Column offset 6** on CASET
5. **PSRAM random access** kills per-pixel hot loops — stage rows in internal SRAM
6. **Internal heap 96 KiB** overflows DRAM2 — stay at 48 KiB unless linker audited
7. **`SpiDmaBus` copies every chunk** to internal RAM — can't zero-copy PSRAM today
8. **upmap in PSRAM** — 217k iterations with 4 gathers each may still be tens of ms; profile `upscale_ms`
9. **Dual-core row split** has seam dependency — split eval by **pixel index** or pipeline eval‖flush across frames instead
10. **ESP32-S3 has no PPA/GPU** — hardware upscale (ESP32-P4 PPA) not available

---

## 19. Companion documentation (complete set)

| Doc | Contents |
|-----|----------|
| [`README.md`](README.md) | Master index, reading order, baseline timings |
| [`02-ANIMATION-BOTTLENECK.md`](02-ANIMATION-BOTTLENECK.md) | 10× FPS math, eval/upscale/flush, ranked tiers, dual-core |
| [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md) | **Master entry prompt** — copy into new chat |
| [`04-SHADER-MATH-MAPPING.md`](04-SHADER-MATH-MAPPING.md) | GLSL↔Rust fidelity contract, fixed-point formats |
| [`05-MEMORY-LINKER.md`](05-MEMORY-LINKER.md) | Buffer inventory, PSRAM vs SRAM, `dram2_seg` overflow |
| [`06-OPTIMIZATION-CHRONOLOGY.md`](06-OPTIMIZATION-CHRONOLOGY.md) | Phase 0–5 history, failed attempts, leverage table |
| [`07-PREBAKE-PIPELINE.md`](07-PREBAKE-PIPELINE.md) | Video/frame-loop fallback (back-burner) |

**For continuation:** start at [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md), not this file.

---

*Document version: Turn 3 of 3 — Jul 2026*  
*See [`README.md`](README.md) for the full documentation index.*