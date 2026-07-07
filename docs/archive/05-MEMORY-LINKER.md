# Memory, Linker, and Buffer Placement Guide

> **Purpose:** Explain where every buffer lives, why PSRAM hurts performance, and how to place `low_rgb565` in internal SRAM without repeating the `dram2_seg` overflow failure.  
> **Doc index:** [`README.md`](README.md) · **Entry prompt:** [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md)

---

## 1. ESP32-S3 memory map (practical view)

**Board reality (Waveshare official):** Built-in **512KB SRAM + 384KB ROM**, with **stacked 8MB PSRAM** and external 16MB Flash.  
(The "stacked" PSRAM is the common ESP32-S3R8 integrated octal PSRAM.)

| Region | Approx size | Speed | Our usage |
|--------|-------------|-------|-----------|
| **Internal SRAM (DRAM/IRAM)** | 512 KiB total | Fast (1-cycle-ish) | Stack, code hot paths, DMA descriptors, reclaimed statics + heap (very constrained) |
| **PSRAM (stacked, octal/OPI)** | 8 MiB | 5–20× slower random access | Large Vecs: framebuffer, upmap, row_packed, (avoid for hot random data) |
| **Flash** | 16 MiB (external) | Read via cache | Code, const, future pre-baked frames |

esp-hal build flags confirm:
- `dma_can_access_psram` = **true**
- `psram_octal_spi` = **true**
- `psram_extmem_origin` = `0x3C000000` region

---

## 2. Current buffer inventory (@ RENDER_DIVISOR=3)

| Buffer | Elements | Bytes | Allocator | Access pattern |
|--------|----------|-------|-----------|----------------|
| `framebuffer` | 466×466×2 | **434,312** | PSRAM | Sequential (DMA chunks) |
| `upmap` | 217,156 × 12 B | **~2,605,872** | PSRAM | Sequential in upscale_pass |
| `row_packed` | 156×156×10 × 4 | **~975,936** | PSRAM | Sequential per row copy |
| `low_rgb565` | 156×156 | **48,672** | internal reclaimed SRAM* | **Random** (4 gathers/pixel upscale) — **critical for speed** |
| `Scratch.row_pack` | 156×10 × 4 | **6,240** | Internal heap | Sequential per row |
| DMA TX/RX | 8192×2 | **16,384** | Internal (dma_buffers!) | DMA |
| DMA descriptors | — | ~few KiB | Internal | DMA |

\* `low_rgb565` is 48 KiB but **does not fit** in the reduced internal heap alongside DMA buffers → esp-alloc places it in PSRAM (the "stacked 8MB PSRAM" on this ESP32-S3R8 board). This is why we use `#[ram(reclaimed)]` statics.

---

## 3. Why PSRAM random access kills upscale

`upscale_pass` per output pixel:

```rust
let c00 = low_rgb565[up.idx[0]];  // PSRAM read
let c10 = low_rgb565[up.idx[1]];  // PSRAM read — likely different cache line
let c01 = low_rgb565[up.idx[2]];  // PSRAM read
let c11 = low_rgb565[up.idx[3]];  // PSRAM read
```

× **217,156 pixels** = **~868k random PSRAM reads/frame**.

Even if each read costs 50–100 cycles effective, that's **43–86M cycles** = **180–360 ms** at 240 MHz — enough to explain upscale still being slow if `low_rgb565` is in PSRAM.

**Fix priority:** Get `low_rgb565` into internal SRAM.

---

## 4. The dram2_seg overflow incident

### What we tried

```rust
esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 96 * 1024);
```

### Linker error

```
section `.dram2_uninit' will not fit in region `dram2_seg'
region `dram2_seg' overflowed by 24560 bytes
```

### Interpretation

- Internal "reclaimed" DRAM2 is **finite** and shared with:
  - Global allocator arena
  - `static` data in DRAM
  - DMA buffer sections
  - Possibly stack-adjacent regions

Increasing heap from 48→96 KiB exceeded DRAM2 by **~24 KiB**.

### What NOT to do

- Blindly increase `heap_allocator!` size
- Put `upmap` (2.6 MiB) in internal — impossible

---

## 5. Strategies to place `low_rgb565` in internal SRAM

### Strategy A — Static array in reclaimed RAM (recommended)

```rust
use esp_hal::ram;

#[ram(reclaimed)]
static mut LOW_RGB565: [u16; 24336] = [0; 24336];  // div=3: 156×156
```

- **48,672 bytes** — does not go through heap
- Raidal2 holds `&mut [u16]` slice into this static
- **Caveat:** div must be fixed at compile time OR use max size `[u16; 54289]` for div=2 (106 KiB — probably too big)

**For div=3 only:** `156*156 = 24336` — fits if DRAM2 has ~49 KiB free outside heap.

### Strategy B — Shrink DMA buffers

Current: `DMA_CHUNK_BYTES = 8192` → 16 KiB total TX+RX.

Try 4096 → save 8 KiB internal, may cost +5 ms flush.

Trade: internal room vs flush time.

### Strategy C — Reduce heap, use static scratch

Move `Scratch.row_pack` to `#[ram(reclaimed)] static` (~6 KiB), reduce heap to 32 KiB.

### Strategy D — Row-slice upscale without full low buffer in SRAM

Keep only **2–4 rows** of low_rgb565 in internal (~1–2 KiB), upscale output bands as eval produces rows. Requires refactoring upscale_pass to row bands instead of full-frame gather from complete low buffer.

**Eliminates 48 KiB static** — best long-term if Strategy A doesn't fit.

---

## 6. Verifying placement at runtime

Add diagnostic (temporary):

```rust
println!("low_rgb565 ptr: {:p}", low_rgb565.as_ptr());
println!("fb ptr: {:p}", framebuffer.as_ptr());
```

Compare against PSRAM range from esp-hal (`psram_extmem_origin` ≈ `0x3C000000`).

If low_rgb565 pointer is in `0x3Cxxxxxx` → PSRAM → **fix placement**.

---

## 7. esp-alloc behavior

```rust
esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 48 * 1024);
esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);
```

**Order matters:** First allocator (internal) serves small allocs; large allocs fall through to PSRAM.

Threshold is roughly the remaining internal heap capacity at alloc time — not documented precisely. Vectors > ~32–40 KiB reliably hit PSRAM.

---

## 8. DMA buffer placement

From `dma_buffers!(8192)` in `main.rs`:

- TX/RX buffers **must** be DMA-capable (internal)
- Descriptors **must** be internal
- `flush_bytes` copies PSRAM framebuffer → internal scratch → SPI DMA

**Cannot eliminate** the chunk copy without lower-level esp-hal API.

---

## 9. upmap size reduction options

Current: 12 bytes × 217k = 2.6 MiB.

### Pack indices as u32 bitfields?

Low index max = 156×156 = 24336 < 2¹⁵ — could pack 4×15-bit indices into 64 bits + 4×8 weights = 16 bytes (worse).

### Row-major upmap slices

Store `upmap_row[oy]: Vec<UpPixel466>` — same total size, better cache if processing row-by-row.

### On-the-fly upscale indices

Recompute `i00,i10,i01,i11` from `ux[ox], uy[oy]` during upscale — **saves 2.6 MiB PSRAM** but adds ALU per pixel. Trade memory for compute — may be worth it if upmap pollutes cache.

---

## 10. Memory budget diagram

```
PSRAM (stacked 8 MB)
├── framebuffer     0.42 MiB  ████
├── upmap           2.49 MiB  ████████████████████████
├── row_packed      0.93 MiB  █████████
├── low_rgb565      0.05 MiB  █  ← MOVED TO INTERNAL SRAM
└── free            ~3.1 MiB

Internal ~512 KiB (not all allocatable)
├── code/stack      (large)
├── DMA             16 KiB
├── heap arena      48 KiB
│   └── scratch     6 KiB
└── reclaimed free  ???  ← target for LOW_RGB565 48 KiB
```

---

## 11. Action checklist for next model

- [ ] Print pointer addresses for `low_rgb565`, confirm PSRAM vs internal
- [ ] Try `#[ram(reclaimed)] static LOW_RGB565: [u16; 24336]`
- [ ] If link fails, read `xtensa-esp32s3-elf-size -A` / map file for `dram2_seg` usage
- [ ] Measure `upscale_ms` before/after — expect 5–20× drop if placement fixed
- [ ] If still slow, implement row-band upscale (Strategy D)

---

*Appendix document — Turn 3 of 3 — see [`README.md`](README.md)*