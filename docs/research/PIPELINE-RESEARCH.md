# Render-Pipeline Overlap Research — OpenPocket

**Goal:** reach ~40 fps (25 ms/frame) full-screen animation by overlapping frame
compose (~5–10 ms on core 0) with the blocking full-frame QSPI DMA flush (~24 ms).
Two candidate architectures researched at register/API level:

- **A. Dual-core pipeline** — core 1 owns the QSPI flush, core 0 composes ahead.
- **B. Single-core async DMA overlap** — non-blocking DMA start + wait, copy/compose while chunks fly.

Environment (verified from repo): `esp-hal = "~1.1.0"` with features `["esp32s3","unstable"]`,
edition 2024, Xtensa ESP32-S3 dual LX7 @ 240 MHz, 8 MB octal PSRAM.
(`Cargo.toml`, `src/bin/main.rs`.)

---

## 0. Current pipeline — measured ground truth (from our source)

Key files: `src/display/qspi_bus.rs`, `src/app.rs` (`flush_dirty`, line ~759),
`src/bin/main.rs` (SPI/DMA/PSRAM init, lines 116–172), `src/display/watch_fb.rs`.

- **Panel / framebuffer:** 466×466 RGB565 BE = **434,312 bytes = 424.13 KiB**, a single
  *retained* canvas (`WatchFb`) leaked into PSRAM (`main.rs:168 vec![...].leak()`).
  Composers write incremental deltas; damage tracked in the DMI.
- **SPI:** `SPI2` + `DMA_CH0`, `Rate::from_mhz(80)`, `SpiMode::_0`, half-duplex **Quad**
  (`sio0..sio3`), CS on GPIO12 driven manually (`QspiBus`). (`main.rs:116–134`.)
- **DMA buffers:** `dma_buffers!(DMA_CHUNK_BYTES)` with `DMA_CHUNK_BYTES = 8192`
  (`qspi_bus.rs:19`). This is an **8 KiB internal-SRAM TX bounce buffer** owned by the
  `SpiDmaBus`. (`main.rs:120–122`.)
- **Flush loop (`flush_bytes`, `qspi_bus.rs:160`):** walks the PSRAM framebuffer in 8 KiB
  slices; each slice is passed to `SpiDmaBus::half_duplex_write(&[u8])`, which **copies the
  slice into the 8 KiB internal DmaTxBuf, starts DMA, and blocks until done**. First chunk
  carries the pixel-write command (0x32 / addr 0x3C00), the rest continue with CS held low.
  A full frame = **`ceil(434312 / 8192) = 54` blocking DMA rounds**.

> The header comment in `qspi_bus.rs` ("Direct from PSRAM slice (no scratch copy)") is
> **misleading**: `SpiDmaBus::half_duplex_write` takes `&[u8]` and *must* stage it through
> its internal DmaTxBuf. The 8 KiB chunking exists precisely because the source has to be
> copied into that 8 KiB internal buffer. We are **not** DMAing from PSRAM today — we are
> CPU-copying PSRAM→SRAM per chunk, then DMAing from SRAM. (This is also why we never had to
> think about DMA/PSRAM cache coherency — see §3.)

### Wire-time budget (Q6)

Quad SPI at 80 MHz drives **4 bits/clock × 80 MHz = 320 Mbit/s = 40 MB/s effective**.

| Quantity | Value |
|---|---|
| Full frame | 434,312 B |
| Pure wire time @ 40 MB/s | **434312 / 40e6 = 10.86 ms** |
| Per 8 KiB chunk wire time | 8192 / 40e6 = 204.8 µs |
| 54 chunks wire | 54 × 204.8 µs ≈ 10.86 ms (matches) |
| **Measured flush** | **~24 ms** |
| **Non-wire overhead** | **~13 ms (~54 %)** |

The ~13 ms is **software overhead, not wire time**, spread over 54 rounds (~240 µs/chunk):
1. **PSRAM→SRAM `memcpy` of each 8 KiB chunk** (the dominant cost — reading octal PSRAM
   through cache into internal SRAM; PSRAM read bandwidth under CPU-mediated copy is well
   below the 40 MB/s SPI wire rate and stalls the core).
2. **DMA descriptor prep + start** per chunk (`prepare()` + kick).
3. **Busy-wait** for each blocking transfer to finish (CPU spins, does no useful work).
4. Minor: CS toggling only at frame start/end (already amortized — good).

**The 13 ms is the reclaimable budget.** Both architectures target it; they differ in *how*
and at what risk.

---

## A. Dual-core pipeline

### A1. esp-hal 1.1 multicore API (module, types, lifetimes, example)

**Module / types:** `esp_hal::system::{CpuControl, Stack, AppCoreGuard, Cpu}`
(unstable — gated behind the `unstable` feature, which we already enable). Internals live in
`esp-hal/src/soc/esp32s3/cpu_control.rs`.

**Exact signature** (docs.espressif.com rust esp-hal, `esp32s3` build):

```rust
pub fn start_app_core<'a, const SIZE: usize, F>(
    &mut self,
    stack: &'static mut Stack<SIZE>,
    entry: F,
) -> Result<AppCoreGuard<'a>, Error>
where
    F: FnOnce() + Send + 'a
```

Notable, and better than older esp-hal: the closure bound is **`Send + 'a`, not `'static`**.
The returned `AppCoreGuard<'a>` ties the closure's lifetime to the guard, so core 1 *may*
capture non-`'static` references as long as the guard (and thus the borrowed data) lives long
enough. **The `Stack` must still be `&'static mut`.** Dropping the guard parks the APP core.

**Minimal working example** — verified from `qa-test/src/bin/multicore_flash.rs`
@ tag `esp-hal-v1.1.0`:

```rust
use core::ptr::addr_of_mut;
use esp_hal::system::{CpuControl, Stack};

let mut cpu_control = CpuControl::new(peripherals.CPU_CTRL);
static mut APP_CORE_STACK: Stack<{ 4096 * 8 }> = Stack::new();   // 32 KiB, 'static
let _guard = cpu_control
    .start_app_core(unsafe { &mut *addr_of_mut!(APP_CORE_STACK) }, move || {
        // runs on APP core (core 1). `move` closure; captured values moved here.
        loop { /* flush work */ }
    })
    .unwrap();
// _guard must stay alive; dropping it parks core 1.
```

**Stack placement:** a `static mut Stack<SIZE>` (lives in internal DRAM by default), passed by
`&'static mut` via `addr_of_mut!`. `Stack::new()` is `const`. The example uses 32 KiB
(`4096*8`); the `CpuControl` docstring example uses `Stack<8192>`. Size to our flush closure's
needs (small — a flush loop + a few locals; 16–32 KiB is ample).

**What the closure can capture:** anything `Send + 'a`. A `move` closure takes ownership of
what it names. To hand the display to core 1 we `move` the `QspiBus`/`SpiDmaBus` into it (see
A2 for Send). Boot sequence (from `cpu_control.rs` `start_core1_init`): core 1 starts with
**interrupts masked** (`xtensa_lx::interrupt::set_mask(0)`), resets its Xtensa CCOMPARE0/1/2
timer-compare regs, sets VECBASE + stack pointer, then calls `interrupt::init_vectoring()` — so
core 1 gets **its own interrupt vector table** but starts with peripheral interrupts off.

### A2. Sharing data core0 → core1

- **Moving the display to core 1:** `move` the whole `QspiBus<'d>` (owns `SpiDmaBus<'d,Blocking>`
  + `Output<'d>` CS) into the `start_app_core` closure. esp-hal peripheral/driver singletons are
  move-only ownership tokens and are `Send`; a `SpiDmaBus` + `Output` + `DMA_CH0` are owned by
  one core at a time, which is exactly the model. Because the bound is `Send + 'a`, this
  compiles as long as the guard outlives the data. **The framebuffer is `&'static mut [u8]`**
  (leaked) — core 1 can hold a `&'static [u8]` view for reading while core 0 holds a separate
  `&'static mut` for the *other* buffer (see double-buffer note, Recommendation).
- **Frame-job handoff (core0 → core1):** in `no_std` the practical options are
  - **`heapless::spsc::Queue`** (lock-free single-producer/single-consumer ring; ideal:
    core 0 = producer of "flush job" descriptors, core 1 = consumer). Requires `heapless` dep.
  - **Atomics** (`AtomicBool`/`AtomicU32`/`AtomicPtr` in a `static`) as a lightweight
    "frame N ready / which buffer" flag + generation counter.
  - **`critical-section::Mutex<RefCell<…>>`** (we already depend on `critical-section`) for a
    shared job struct.
- **Native atomics on dual-core Xtensa S3:** the LX7 has the **`S32C1I`** compare-and-swap
  instruction, so `core::sync::atomic` CAS **works natively for addresses in internal RAM**.
  The well-known caveat: **hardware atomics do *not* work for data placed in PSRAM** on
  S3/ESP32 (bus limitation). Therefore **keep all cross-core sync variables (flags, spsc
  buffers) in internal SRAM, never in the PSRAM framebuffer.** `portable-atomic` is the
  standard belt-and-suspenders in the esp-rs ecosystem if a target/toolchain lacks native
  CAS; for S3 internal-RAM atomics it is not strictly required but is harmless.
- **Completion signal back (core1 → core0):** the same primitives in reverse — an
  `AtomicBool DONE`/generation counter core 0 polls, or a second spsc queue of "buffer freed"
  tokens. Core 0 waits on it before reusing a buffer.
- **`critical-section` correctness on SMP:** on a dual-core chip, *disabling interrupts alone
  is not a critical section* — the other core can still touch shared state. esp-hal ships a
  **multicore-aware `critical-section` implementation that takes a spinlock** in addition to
  masking interrupts, so `critical_section::with` is safe across both cores. Keep sections
  ultra-short (a spinlock held on one core stalls the other).

### A3. PSRAM + cache coherency between cores (the pivotal fact)

- **The ESP32-S3 L1 cache is *shared* by both CPU cores** (a single unified instruction cache
  and a single unified data cache, bank-partitioned), unlike the original ESP32 which has
  per-core caches. **Consequence: there is *no* CPU↔CPU cache-coherency problem for the PSRAM
  framebuffer.** When core 0 writes framebuffer pixels (cached) and core 1 later *CPU-reads*
  them, both go through the *same* data cache — core 1 sees core 0's writes with only normal
  memory-ordering care (an atomic/critical-section release/acquire on the handoff flag).
- **DMA is a different story — DMA bypasses cache.** If core 1 DMAs *directly from PSRAM*, the
  DMA engine reads PSRAM main memory, which may be stale relative to dirty cache lines. On the
  Rust side **esp-hal handles this automatically**: `DmaTxBuf::prepare()` contains
  `#[cfg(dma_can_access_psram)]` (set for S3) code that detects a PSRAM-resident buffer
  (`!is_valid_ram_address(ptr)`) and calls **`crate::soc::cache_writeback_addr(ptr, len)`**
  before starting the transfer — i.e. it writes the CPU cache back to PSRAM so the DMA reads
  fresh data. Only writeback is needed on the TX path (CPU writes, DMA reads); no invalidate.
  (Source: `esp-hal/src/dma/buffers.rs`, `DmaTxBuf::prepare`.) This is the ESP-IDF
  `esp_cache_msync(..., ESP_CACHE_MSYNC_FLAG_DIR_C2M)` behavior, done for us.
- **Is DMA-from-PSRAM for SPI even supported on S3? Yes**, with constraints
  (ESP-IDF "Support for External RAM"; esp-hal DMA docs):
  - **DMA descriptors must live in internal RAM** (cannot be in PSRAM).
  - **32-byte (cache-line) alignment**: a PSRAM DMA buffer's address *and* length must be
    multiples of 32 bytes on S3.
  - **Bandwidth is limited and contended**: "the bandwidth that DMA accesses external RAM is
    very limited, especially when the core is trying to access the external RAM at the same
    time." **This is the crux risk for A** (below).
- **Today we sidestep all of this**: our flush copies PSRAM→internal SRAM (CPU read hits the
  coherent cache) and DMAs from SRAM, so no `cache_writeback`, no alignment constraint — at
  the cost of the 13 ms copy.

### A4. Interrupts, esp-println, Instant/timers on core 1; footguns

- **`Instant` / `Duration` / delays are safe on both cores.** esp-hal's time base is the
  **SYSTIMER**, a *global* peripheral with a shared 52-bit counter — not the per-core Xtensa
  CCOMPARE. `Instant::now()` reads the same counter from either core, so all our
  `Instant::now()` / `Duration` / `Delay` timing in `app.rs` works unchanged on core 1. (Note
  core-1 boot resets the Xtensa internal CCOMPARE regs, but that does *not* affect SYSTIMER.)
- **`esp-println` on core 1:** usable — our `esp-println` has the `critical-section` feature,
  so prints take the multicore spinlock and won't corrupt each other. Interleaving of lines
  across cores is possible; keep hot-loop prints on one core.
- **Interrupts:** core 1 starts with peripheral interrupts masked, then re-inits vectoring. If
  the flush stays **blocking/polled** on core 1 (recommended for A), we need *no* interrupts on
  core 1 at all — simplest and avoids per-core interrupt-routing footguns. GDMA/SPI interrupts
  are routed per-core; if you later go async on core 1 you must enable them *on core 1*.
- **Known footguns (esp-hal multicore, this version):**
  - Closure/captured data must be `Send`; the `Stack` must be `&'static mut`.
  - **Flash writes while both cores run** need `multicore_auto_park()` (see
    `multicore_flash.rs`) — the writing core stalls the other to avoid it executing stale
    cache during the flash operation. Not our flush path, but relevant if we ever persist
    settings while animating.
  - Keep sync vars out of PSRAM (A2). Keep spinlock/critical sections tiny (A2).
  - The whole multicore API is `unstable` — may shift between minor versions.

---

## B. Single-core async DMA overlap

### B5. Does esp-hal 1.1 expose a split start/wait DMA API? — Yes.

The lower-level **`SpiDma`** (what `SpiDmaBus` wraps; recover it via `SpiDmaBus::split()` →
`(SpiDma, DmaRxBuf, DmaTxBuf)`) exposes **non-blocking one-shot transfers that consume `self`
and return a transfer handle**:

```rust
// esp_hal::spi::master::SpiDma
pub fn half_duplex_write<TX: DmaTxBuffer>(
    self, data_mode: DataMode, cmd: Command, address: Address, dummy: u8,
    bytes_to_write: usize, buffer: TX,
) -> Result<SpiDmaTransfer<'d, Dm, TX>, (Error, Self, TX)>;

pub fn write<TX: DmaTxBuffer>(self, len: usize, buffer: TX)
    -> Result<SpiDmaTransfer<'d, Dm, TX>, (Error, Self, TX)>;
// SpiDmaTransfer::wait(self) -> (SpiDma, TX)  // blocks for completion, returns bus + buffer
// SpiDmaTransfer::is_done() -> bool           // poll without blocking
```

- These **return immediately** after kicking DMA; the `SpiDmaTransfer` owns the `SpiDma` + the
  `DmaTxBuf` until `wait()` (or drop) reclaims them. **Max single transfer = 32,736 bytes.**
- **Overlap pattern (the win): double-buffer the internal TX bounce buffer.** Own **two**
  `DmaTxBuf`s (A and B, 8 KiB each in SRAM). Loop:
  1. `memcpy` PSRAM chunk 0 → buf A.
  2. start DMA of buf A → get transfer T.
  3. while T flies: `memcpy` PSRAM chunk 1 → buf B **on the CPU, concurrently with the wire**.
  4. `T.wait()` → reclaim bus + A; start DMA of buf B → T'; copy chunk 2 → A; …
  This **hides the ~13 ms of PSRAM→SRAM copy behind the ~10.9 ms of wire time**, and replaces
  busy-waiting with useful copying. Flush wall-time collapses toward
  `max(copy_total, wire_total) + one tail chunk ≈ 12–14 ms` — **roughly half**, on **one core**.
- **Alternative: DMA straight from PSRAM, no bounce at all.** Build `DmaTxBuf`s pointing at the
  PSRAM framebuffer (32-byte aligned, descriptors in SRAM); esp-hal auto-`cache_writeback`s
  (§A3). Chunk ≤ 32,736 B ⇒ ~14 transfers, **zero memcpy**. Downside: the DMA now reads PSRAM,
  which **contends with the CPU's own PSRAM traffic during compose** — the same contention that
  is architecture A's main risk, now on one core. The bounce+ping-pong variant is safer because
  DMA reads SRAM (no PSRAM contention with the next compose).

### B6. Where the 24 ms goes (recap with the API in view)

`10.86 ms` wire (unavoidable at Quad/80 MHz) + `~13 ms` overhead that is **almost entirely the
54× PSRAM→SRAM `memcpy` plus per-chunk busy-wait**. Both are CPU work that the split start/wait
API lets us overlap with the wire or eliminate. Descriptor setup and CS toggling are minor
(CS toggles only at frame boundaries in our code — already good). **Conclusion: most of the
reclaimable 13 ms is recoverable without a second core.**

---

## Risk list

| Risk | Affects | Severity | Mitigation |
|---|---|---|---|
| PSRAM bandwidth contention (core-0 compose writes vs. flush PSRAM reads) | A (and B-direct-PSRAM) | **High** | Flush from **internal SRAM** (bounce), not PSRAM, so the wire-side reads don't hit PSRAM; only the copy does. Keep compose and copy from fighting by pipelining chunk-wise. |
| DMA/PSRAM cache coherency (stale DMA reads) | A/B if DMAing from PSRAM | Medium | esp-hal `DmaTxBuf::prepare` auto-`cache_writeback_addr`; obey **32-byte addr+len alignment**; descriptors in SRAM. If bouncing through SRAM, N/A. |
| Cross-core data race on the retained canvas | A | **High** | **Double-buffer** (§Recommendation). Never let core 0 write the buffer core 1 is reading. Handoff via atomic generation flag / spsc. |
| Atomics in PSRAM don't work on S3 | A | Medium | Keep all sync vars + spsc storage in **internal SRAM**. |
| `Send` / `'static` on the closure & stack | A | Low | Bound is `Send + 'a`; `move` the `QspiBus`; `Stack` as `static mut` via `addr_of_mut!`. |
| `unstable` API churn (`CpuControl`, `SpiDma` one-shot) | A & B | Low | Already on `unstable`; pin `~1.1.0`; both APIs present at `esp-hal-v1.1.0`. |
| Interrupt routing per-core if flush goes async on core 1 | A | Low | Keep core-1 flush **blocking/polled** — needs no interrupts. |
| Spinlock/critical-section stalls other core | A | Low | Keep sections to a pointer swap + counter bump. |

---

## Recommended architecture

**Primary: B — single-core, double-buffered async DMA overlap.** It meets the 40 fps target
with dramatically less risk and no new concurrency surface.

**Rationale / expected numbers**
- Flush wall-time drops from ~24 ms toward **~12–14 ms** by hiding the PSRAM→SRAM copy behind
  the 10.86 ms wire time and eliminating busy-wait (§B5).
- Frame = compose + overlapped-flush ≈ `5–10 ms + ~13 ms = 18–23 ms` ⇒ **~43–55 fps**, i.e.
  the 25 ms budget is met on **one core**.
- **Zero** exposure to the two highest-severity risks: cross-core canvas races and PSRAM
  bandwidth contention (the wire reads SRAM, not PSRAM). Cache coherency is a non-issue
  (bounce path). No `Send`/`'static`/stack/interrupt-routing footguns.

**Concrete integration sketch for our code** (`src/display/qspi_bus.rs`)
- Replace the `SpiDmaBus`-based `QspiBus` with a `SpiDma` + **two owned `DmaTxBuf`s** (ping/pong),
  both from `dma_buffers!` sized 8 KiB (or a bit larger, ≤ 32,736 B, to cut round count).
- Rewrite `flush_bytes`/`flush_rect` as the ping-pong loop in §B5: `half_duplex_write(...) ->
  SpiDmaTransfer`, copy the *next* chunk during `is_done()==false`, then `wait()`; first chunk
  carries cmd 0x32 / addr 0x3C00, rest `Command::None`. `flush_dirty` in `app.rs` is unchanged
  above the bus boundary; the DMI partial/full decision stays.
- Keep `WatchFb` as the single retained PSRAM canvas — **no second framebuffer needed for B**
  (we only double-buffer the tiny 8 KiB SRAM staging buffers, not the 424 KiB canvas). This
  preserves the retained-canvas / incremental-delta design exactly.

**If compose later grows past the flush time** (heavier scenes than today's 5–10 ms),
escalate to a **hybrid**: keep B's async flush, and additionally move it to **core 1** (A) with
**two full framebuffers** (424 KiB each ≈ 848 KiB — trivially affordable in 8 MB PSRAM). Core 0
composes canvas B (applying the same incremental deltas it would to A — or a cheap full copy
after settle) while core 1 async-flushes canvas A from SRAM bounce buffers; handoff via an
`AtomicU32` generation flag + a `heapless::spsc` "buffer-free" token, all in internal SRAM.
That caps the frame at `max(compose, flush)` and buys headroom to ~40 fps even when compose is
heavy — at the cost of the double-buffer discipline and the multicore risks above. **Do A only
if B's measured numbers fall short**; today they should not.

---

## Sources

- esp-hal `CpuControl` / `start_app_core` (esp32s3):
  https://docs.espressif.com/projects/rust/esp-hal/1.0.0/esp32/esp_hal/system/struct.CpuControl.html
- esp-hal `SpiDma` one-shot `write`/`half_duplex_write`/`SpiDmaTransfer` (32,736-byte max):
  https://docs.rs/esp-hal/latest/esp_hal/spi/master/struct.SpiDma.html
- esp-hal `SpiDmaBus` (`split()` back to `SpiDma`+bufs):
  https://docs.espressif.com/projects/rust/esp-hal/1.0.0/esp32c3/esp_hal/spi/master/struct.SpiDmaBus.html
- esp-hal source, APP-core boot (interrupt mask, CCOMPARE reset, vectoring):
  `esp-hal/src/soc/esp32s3/cpu_control.rs` @ tag `esp-hal-v1.1.0`
- esp-hal working multicore example (`Stack`, `start_app_core`, `move` closure):
  `qa-test/src/bin/multicore_flash.rs` @ tag `esp-hal-v1.1.0`
- esp-hal `DmaTxBuf::prepare` PSRAM `cache_writeback_addr` + 32-byte alignment:
  https://docs.espressif.com/projects/rust/esp-hal/1.1.1/esp32h2/src/esp_hal/dma/buffers.rs.html
- ESP-IDF "Support for External RAM" (DMA/PSRAM limits, descriptors in internal RAM, contention):
  https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-guides/external-ram.html
- ESP-IDF "Memory Synchronization" (`esp_cache_msync`, C2M/M2C):
  https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-reference/system/mm_sync.html
- ESP32-S3 shared L1 cache between both cores (datasheet / TRM System & Memory):
  https://www.espressif.com/sites/default/files/documentation/esp32-s3_datasheet_en.pdf
- Xtensa S32C1I / native-atomic caveat in PSRAM (esp-rs):
  https://github.com/esp-rs/esp-idf-sys/issues/406
- portable-atomic on esp targets: https://github.com/taiki-e/portable-atomic/issues/148
