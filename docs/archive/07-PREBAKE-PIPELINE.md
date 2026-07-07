# Pre-Bake Pipeline — Video / Frame-Loop Fallback (Back-Burner)

> **Purpose:** Full specification for offline-rendered Raidal-2 playback if live shader math cannot reach 6 FPS.  
> **User preference:** Optimize live evaluation first. Use this path only after quantifying the gap or explicit user approval.  
> **Companion docs:** [`02-ANIMATION-BOTTLENECK.md`](02-ANIMATION-BOTTLENECK.md) §12, [`01-PROJECT-HANDOFF.md`](01-PROJECT-HANDOFF.md) §16, [`05-MEMORY-LINKER.md`](05-MEMORY-LINKER.md)

---

## Table of contents

1. [When to use pre-bake](#1-when-to-use-pre-bake)
2. [What "success" looks like](#2-what-success-looks-like)
3. [Offline capture pipeline](#3-offline-capture-pipeline)
4. [On-device pixel format contract](#4-on-device-pixel-format-contract)
5. [Storage math](#5-storage-math)
6. [Compression strategies](#6-compression-strategies)
7. [Flash partition layout](#7-flash-partition-layout)
8. [Firmware playback architecture](#8-firmware-playback-architecture)
9. [Hybrid modes](#9-hybrid-modes)
10. [Loop seam and temporal continuity](#10-loop-seam-and-temporal-continuity)
11. [Quality validation checklist](#11-quality-validation-checklist)
12. [Integration plan (minimal disruption)](#12-integration-plan-minimal-disruption)
13. [Risks and mitigations](#13-risks-and-mitigations)

---

## 1. When to use pre-bake

### Trigger conditions

Use pre-bake only when **all** of the following are true:

1. Live path profiled with `--release` and split timings (`eval=`, `upscale=`, `flush=`)
2. Best-effort Tier S optimizations applied (see [`02-ANIMATION-BOTTLENECK.md`](02-ANIMATION-BOTTLENECK.md) §10):
   - `low_rgb565` in internal SRAM
   - Dual-core eval by pixel index
   - i32-only inner loop
   - Eval‖flush pipeline if applicable
3. Total frame time still **> 170 ms** (~6 FPS ceiling) **or** user explicitly accepts non-live shader
4. User approves visual trade-off (playback is pixel-perfect WebGL but not mathematically live)

### When NOT to use

- Before confirming Gen 5 `upscale_ms` — if upscale still ~800 ms, that's a **bug**, not a fundamental limit
- As a shortcut around display bring-up (already solved)
- To justify div≥4 quality loss (user rejected)

---

## 2. What "success" looks like

| Metric | Live target | Pre-bake target |
|--------|-------------|-----------------|
| FPS | ≥ 6 (stretch 10+) | **25–40** (flush-limited) |
| Visual | WebGL parity at div=2–3 | **Exact** WebGL @ 466×466 |
| CPU during anim | 100% render | **~0%** (DMA memcpy + index advance) |
| Storage | Firmware only | Short loop in flash or SD |
| Shader math on device | Yes | **No** |

Pre-bake **wins on FPS and fidelity**; **loses on live reactivity** (time always loops, no arbitrary `t`).

---

## 3. Offline capture pipeline

### 3.1 Source of truth

Capture from the **canonical WebGL** Raidal-2 shader at **466×466** — not from the Rust port. The Rust port is a fidelity target; WebGL is the reference for pre-bake.

### 3.2 Recommended capture flow

```
┌─────────────────────────────────────────────────────────────┐
│  PC: Headless WebGL2 (Puppeteer / Playwright / custom)      │
│  Resolution: 466×466                                        │
│  time t: 0 .. T_loop (seconds)                              │
│  step: 1/FPS_target (e.g. 33.33 ms @ 30 FPS)                │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────┐
│  Per frame: read RGBA float [0,1] from framebuffer          │
│  Apply same tone mapping as shader (already in fragment)    │
│  Quantize → RGB565 (5/6/5) big-endian bytes                 │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────┐
│  Encode: raw | RLE | delta-RGB565 | LZ4 block               │
│  Package: .bin + header (magic, frames, fps, format)        │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────┐
│  esptool / cargo-espflash embed in partition OR             │
│  ship separate asset blob flashed at offset                 │
└─────────────────────────────────────────────────────────────┘
```

### 3.3 Capture script requirements

The offline tool must record:

| Field | Value |
|-------|-------|
| Width × height | 466 × 466 |
| `time_scale` | Match `Raidal2Config.time_scale` (default `1.0`) |
| Layer count | 9 |
| Loop period `T_loop` | Must be chosen so **frame 0 ≈ frame N** visually |
| FPS | 24–30 recommended (balance size vs smoothness) |

### 3.4 Rust port cross-check (optional)

After capture, decode frame `k` and compare against live Rust `eval_pass` + `upscale_pass` at the same `time_ms`:

```
max_delta_per_channel ≤ 2 RGB565 levels  (per 04-SHADER-MATH-MAPPING.md)
```

Discrepancies indicate port drift — fix Rust before trusting hybrid modes.

---

## 4. On-device pixel format contract

Must match existing `flush_bytes` path exactly.

| Property | Value |
|----------|-------|
| Color format | RGB565 |
| Byte order | **Big-endian** per pixel (`out[o]=hi`, `out[o+1]=lo`) |
| Frame size | 466 × 466 × 2 = **434,312 bytes** (~424 KiB) |
| Panel command | RAMWR `0x2C` then DMA stream (same as live path) |
| CASET offset | Column offset **6** (unchanged) |

Pre-baked frames should be stored **display-ready** — no per-frame conversion on device.

---

## 5. Storage math

### 5.1 Raw RGB565 (no compression)

```
frame_bytes = 434,312
```

| Duration | FPS | Frames | Raw size |
|----------|-----|--------|----------|
| 1.0 s | 30 | 30 | **12.4 MiB** |
| 2.0 s | 30 | 60 | **24.8 MiB** |
| 4.0 s | 30 | 120 | **49.6 MiB** |
| 10.0 s | 30 | 300 | **124 MiB** |

**16 MB flash** holds roughly:

```
16 MiB firmware + assets overhead
→ ~1.2–1.5 s raw @ 30 FPS (after firmware ~1–2 MiB)
```

Longer loops require **compression** or **external storage (SD)**.

### 5.2 Practical loop lengths @ 30 FPS

| Available asset flash | Raw loop max |
|----------------------|--------------|
| 4 MiB | ~0.35 s (10 frames) — too short |
| 8 MiB | ~0.7 s (21 frames) |
| 12 MiB | ~1.1 s (33 frames) |
| + SD card | Multi-second / minute loops |

**Recommendation:** Target **2–4 s loop** with compression → fits 16 MB flash with headroom.

---

## 6. Compression strategies

Ranked by decode CPU cost on ESP32-S3 (lower = better for playback FPS).

### 6.1 Frame differencing (delta RGB565)

Store frame 0 raw; frame `k` stores XOR or per-channel delta from frame `k-1`.

| Pros | Cons |
|------|------|
| 5–15× compression on aurora (mostly static background) | Decoder must apply delta sequentially |
| Simple C/Rust loop | Random access to frame `k` requires chain from nearest keyframe |

**Decode cost:** ~434 KiB XOR/add per frame → **< 5 ms** at 240 MHz if sequential.

**Enhancement:** Keyframe every 15 frames + deltas between.

### 6.2 Run-length encoding (RLE) per row

Good for large dark regions in aurora background.

| Pros | Cons |
|------|------|
| Tiny decoder | Modest compression on textured aurora |
| Row-major matches DMA | Custom format |

### 6.3 LZ4 block per frame

| Pros | Cons |
|------|------|
| 2–4× typical compression | LZ4 decode ~20–50 ms/frame at 434 KiB — may eat FPS budget |
| Standard library ports exist | Needs ~434 KiB decode buffer |

**Verdict:** Prefer **delta + keyframes** over LZ4 for 30 FPS playback.

### 6.4 Indexed palette (per frame)

Unlikely to help — aurora has continuous color gradients.

### 6.5 Suggested container format

```c
// File header (fixed 64 B)
struct PrebakeHeader {
    magic: [4]u8,      // "RAID"
    version: u16,      // 1
    width: u16,        // 466
    height: u16,       // 466
    fps: u16,          // 30
    frame_count: u32,
    format: u8,        // 0=raw, 1=delta, 2=delta+keyframe
    keyframe_interval: u8,
    flags: u16,
    payload_offset: u32,
    crc32: u32,
    reserved: [32]u8,
};
```

Per-frame header (delta mode):

```c
struct FrameEntry {
    offset: u32,       // from payload start
    compressed_len: u32,
    is_keyframe: u8,
    reserved: [3]u8,
};
```

---

## 7. Flash partition layout

### 7.1 Option A — Embedded in firmware image

Use `esp-idf` partition table or `build.rs` + `include_bytes!`:

```rust
const LOOP_ASSET: &[u8] = include_bytes!("../assets/raidal_loop.bin");
```

| Pros | Cons |
|------|------|
| Single flash command | Rebuild firmware to change loop |
| Simple | Bloats ELF link step |

### 7.2 Option B — Separate data partition

```
# partitions.csv (example)
nvs,      data, nvs,     0x9000,  0x6000
phy_init, data, phy,     0xf000,  0x1000
factory,  app,  factory, 0x10000, 0x200000   # 2 MiB app
assets,   data, spiffs,  0x210000, 0xD00000  # 13 MiB assets
```

Flash assets independently:

```powershell
esptool write_flash 0x210000 assets/raidal_loop.bin
```

### 7.3 Option C — SD card (Waveshare board has SD slot)

| Pros | Cons |
|------|------|
| Unlimited loop length | SDIO init, power, file IO |
| Swap loops without reflash | Mechanical complexity |

**Back-burner within back-burner** — only if flash too small.

### 7.4 Runtime memory during playback

| Buffer | Placement | Size |
|--------|-----------|------|
| `framebuffer` | PSRAM | 434 KiB (reuse existing) |
| Decode scratch | Internal or PSRAM | ≤ 434 KiB for keyframe decode |
| Frame index table | Flash mmap or RAM | `frame_count × 8` bytes |

Playback does **not** need: `row_packed`, `upmap`, `low_rgb565` (~4 MiB PSRAM freed).

---

## 8. Firmware playback architecture

### 8.1 Module sketch

New file: `src/prebake.rs` (or feature-gated module)

```rust
pub struct PrebakePlayer {
    header: PrebakeHeader,
    frame_table: &'static [FrameEntry],
    payload: &'static [u8],
    decode_buf: &'static mut [u8],  // 434312 bytes in PSRAM
    current: u32,
}

impl PrebakePlayer {
    pub fn next_frame(&mut self, out: &mut [u8]) { /* decode → out */ }
    pub fn frame_at(&mut self, index: u32, out: &mut [u8]) { /* seek */ }
}
```

### 8.2 Main loop (playback mode)

```rust
loop {
    let frame_start = Instant::now();
    player.next_frame(&mut framebuffer);
    bus.write_command(0x2C);
    bus.flush_bytes(&framebuffer, &mut dma_scratch);
    // No eval, no upscale
    delay_until(frame_start + frame_period);
}
```

**Expected timing:**

```
decode: 0–5 ms (raw) or 5–15 ms (delta)
flush:  ~25 ms
total:  ~30–40 ms → 25–33 FPS
```

### 8.3 Feature flag

```toml
# Cargo.toml
[features]
default = ["live-shader"]
live-shader = []
prebake = []
```

```rust
#[cfg(feature = "live-shader")]
{ render_timed(...) }

#[cfg(feature = "prebake")]
{ player.next_frame(...) }
```

Keeps live path as default; user preference preserved.

---

## 9. Hybrid modes

If user wants **some** live character with **most** of the FPS:

### 9.1 Pre-bake background + live overlay

- Pre-bake: slow-moving deep aurora layers (layers 1–5)
- Live: fast edge layers (6–9) at div=4 on small mask

**Risk:** Compositing cost + seam artifacts. High implementation cost.

### 9.2 Short pre-bake loop + live time offset

- Play 2 s loop at full speed
- Restart loop; advance `time_scale` slightly each cycle for slow drift

**Risk:** Visible loop seam unless period matches perfectly.

### 9.3 Dual-mode firmware

- Boot: live shader (current)
- Long-press or compile flag: switch to pre-bake

**Recommended hybrid** if live path reaches 2–3 FPS but not 6 — honest fallback without abandoning live work.

---

## 10. Loop seam and temporal continuity

Raidal-2 is quasi-periodic in `t` but **not** automatically loop-seamless.

### Finding loop period

1. Render at high FPS (60+) on PC for `t ∈ [0, 10]` s
2. Compute per-frame MSE or SSIM against frame 0
3. Pick `T_loop` at local minimum (e.g. ~3.7 s — **measure, don't guess**)
4. Crossfade last 3 frames into first 3 frames (optional):

```
out = lerp(frame[i], frame[0], smoothstep(0, 1, i / 3))
```

### Acceptance

- No visible "jump" when `i` wraps `N → 0`
- Aurora rotation direction continuous

---

## 11. Quality validation checklist

Before shipping pre-bake build:

- [ ] Colors match WebGL reference screenshot at `t=0`, `t=T/2`, `t=T`
- [ ] RGB565 BE displays correctly (no red/blue swap)
- [ ] Round display mask: content centered (466×466 full panel)
- [ ] Playback FPS ≥ 25 sustained in serial log
- [ ] Loop seam invisible or crossfaded
- [ ] PMIC + QSPI init unchanged — panel still boots
- [ ] Live shader feature still builds (`--features live-shader`)

---

## 12. Integration plan (minimal disruption)

### Phase 1 — Asset tooling only (no firmware change)

1. Write PC capture script
2. Encode 2 s loop @ 30 FPS with delta compression
3. Measure compressed size; confirm fits partition budget

### Phase 2 — Playback prototype

1. Add `prebake` feature flag
2. `include_bytes!` small test pattern (solid gradient) — verify DMA path
3. Swap in real loop asset
4. Serial log: `mode=prebake fps~X decode=Yms flush=Zms`

### Phase 3 — Coexistence

1. Default remains `live-shader`
2. Document flash commands for both builds
3. Quantify live gap in `03-FOLLOWUP-PROMPT.md` optional context

### Do NOT

- Remove `raidal.rs` or static cache init
- Change QSPI init sequence
- Replace live path as default without user approval

---

## 13. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Flash too small for loop | Delta compression; shorten loop; SD card |
| Decode too slow | Raw frames + PSRAM mmap; keyframes only |
| Loop seam visible | Measure `T_loop`; crossfade |
| Rust/WebGL drift | Capture from WebGL, not Rust |
| User rejects non-live | Keep `live-shader` default; pre-bake opt-in |
| Asset update friction | Separate partition flash (Option B) |

---

## Summary

Pre-bake is the **guaranteed FPS path** (flush-limited **25–40 FPS**) with **perfect WebGL visuals**, at the cost of **non-live** animation and **storage engineering**. It is explicitly **back-burner** until the live path — after Tier S optimizations — is profiled and still misses **~170 ms/frame**.

**Next live-path actions** (preferred): see [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md).

---

*Appendix document — Turn 3 of 3 — Jul 2026*