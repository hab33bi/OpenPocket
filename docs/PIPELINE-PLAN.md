# Render Pipeline Upgrade — Plan (pre-implementation)

Goal: full-screen animation at 40+ fps (wheel scroll, morphs). Today's frame
= ~5–10 ms compose + **24 ms blocking full flush** ≈ 30 fps ceiling.
Research: `docs/research/PIPELINE-RESEARCH.md` (2026-07-22).

## The key discovery

We never DMA from PSRAM: `SpiDmaBus::half_duplex_write` copies each 8 KiB
slice PSRAM→internal SRAM, then DMAs from SRAM — 54 blocking copy+wait
rounds per full frame. The 24 ms decomposes as:

- **~10.9 ms** unavoidable wire time (434,312 B ÷ 40 MB/s quad @ 80 MHz)
- **~13 ms** reclaimable software overhead (the 54 memcpys + busy-waits)

## Chosen architecture: B — single-core async-DMA ping-pong (research-recommended)

NOT dual-core. esp-hal 1.1's `SpiDma` (recovered from our `SpiDmaBus` via
`.split()`) has a non-blocking one-shot API: `half_duplex_write` consumes
self → `SpiDmaTransfer` with `is_done()`/`wait()`, max 32,736 B per shot.

Pipeline: own TWO `DmaTxBuf` staging buffers (internal SRAM, ~16–32 KiB
each); while chunk N's DMA is on the wire, the CPU copies chunk N+1 from
the PSRAM canvas into the other buffer. The ~13 ms of copying hides behind
the ~10.9 ms of wire time → **flush ≈ 12–14 ms**, frames ≈ 18–24 ms ≈
**42–55 fps**, one core, no cache-coherency risk, retained-canvas design
unchanged (no second framebuffer).

Why not dual-core (A): PSRAM bandwidth contention between core-0 compose
writes and core-1 flush reads (the S3's octal PSRAM bus is the shared
bottleneck), plus a forced double-buffer of the retained canvas to avoid
cross-core races. All risk, and B already reaches the target. Escalate to a
hybrid (A + two 424 KiB framebuffers — affordable) only if compose ever
outgrows the flush time.

## Implementation steps (each measured before the next)

1. **P1 — split the bus**: rework `QspiBus` to hold `SpiDma` + two
   `DmaTxBuf`s instead of `SpiDmaBus`; keep the existing blocking behavior
   (write chunk, wait, next) as a correctness baseline. Command writes
   (`write_command`, `write_c8d8`, window setup) stay blocking as today.
   Verify: pixel-identical output, same 24 ms (no regression).
2. **P2 — ping-pong overlap** in `flush_bytes`: start DMA on buffer A,
   copy the next slice into buffer B while A flies, `wait()` A, swap,
   repeat. Chunk size tuning (8 → 16–32 KiB) to amortize per-shot setup.
   Verify: `flush: full frame` drops to ~12–14 ms; visual integrity
   (window alignment, chunk seams) across lock/unlock/wheel/flourish.
3. **P3 — partial-flush path**: apply the same overlap to `flush_spans`
   (span runs batched into the staging buffers). Verify: drag/scrub paths
   unchanged visually, span flushes stay ≤ current times.
4. **P4 — cadence lift**: wheel scroll + morphs target 40 fps (25 ms
   frames); measure real fps in the drag/scroll logs; retune momentum/dt
   constants if frame times shift feel.

Rollback: each step is internal to `display/`; the blocking path stays
compilable behind the old methods until P4 lands.

## Risks

- DMA descriptor + alignment rules (descriptors in internal RAM; esp-hal
  handles via `DmaTxBuf`); 32,736 B per-transfer cap → chunking stays.
- `mem2mem` copy from PSRAM while DMA reads SRAM: no contention (different
  memories) — the whole point of B.
- The QSPI continuation-write protocol (CS held, no command between
  chunks) must survive the split-transfer API — P1 proves it before any
  overlap exists.
