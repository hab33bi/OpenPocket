# Performance Architecture Plan — dual-core, DMA, and the fps ceiling

Status: PLAN (investigated 2026-07-22; measurements from live telemetry).
Goal: raise the whole OS's frame rate and input responsiveness to the
hardware's actual limits, cleanly, without sacrificing the retained-canvas
architecture that everything is built on.

## 1. Where every millisecond goes today (measured)

A wheel scroll frame today is **strictly serial** on core 0:

```
poll touch (I2C ~2-3 ms gated @7 ms) → render (2-18 ms) → flush (0-14 ms) → next
```

- **Render**: draw_scroll targeted clear + blits, 2-4 ms at rest, 10-18 ms
  during fast scroll (big union bbox). All CPU, all PSRAM read-modify-write.
- **Flush**: QspiBus ping-pong staging. The CPU copies each 16 KiB chunk
  PSRAM→SRAM while the previous chunk is on the wire, then **blocks on the
  final wait**. Full frame ≈ 13 ms of which ~10.9 ms is wire time
  (424 KiB @ 80 MHz quad ≈ 40 MB/s — this is the physical floor).
  Partial flushes scale down linearly.
- Because render and flush are serial, a fast-scroll frame costs
  **render + flush ≈ 25-30 ms → ~35-45 fps**. The wire and the CPU each
  idle while the other works.

Key observation: **the frame time we ship is `render + flush`; the frame
time the hardware supports is `max(render, flush)`.** Everything below is
about deleting that `+`.

## 2. The prize list (ordered by leverage)

### P1 — Asynchronous flush (overlap render N+1 with flush N)
The single biggest win, worth almost 2× on scroll fps.

Two implementation tiers:

**Tier A — direct-from-PSRAM DMA (preferred, investigate first).**
The ESP32-S3's GDMA can source SPI TX from external PSRAM (EDMA), with
64-byte alignment/burst constraints. If esp-hal 1.1's `DmaTxBuf` can be
built over a PSRAM region (or a raw descriptor chain can be constructed),
the staging copies disappear entirely: flush = pure DMA from the retained
framebuffer, CPU cost ≈ 0, and `flush()` returns immediately after
programming descriptors.
- Constraint: the fb rows for a rect flush aren't contiguous — a
  descriptor chain per row-run is needed (GDMA linked lists handle this;
  one descriptor per ≤4095 B run; a full frame is ~466 descriptors — fine
  in SRAM).
- Hazard: rendering INTO a region the DMA is reading → tearing for one
  frame at worst, never corruption. Discipline: the damage-tracked
  renderer rarely rewrites the same rect two frames running; accept, or
  gate with a cheap "rect overlaps in-flight flush" check.
- Cache coherency: PSRAM writes must be visible to DMA (writeback cache
  flush for the dirty range — esp-hal exposes this; cost µs-scale).

**Tier B — deferred-wait staging (fallback, small but free).**
Keep staging ping-pong, but return after the LAST chunk is queued instead
of waited; the wait moves to the next bus touch. Saves ~0.4 ms per flush
and unblocks nothing else — only do this if Tier A dead-ends.

Expected outcome (Tier A): wheel fast-scroll ~35-45 → **55-70 fps**
(render-bound), full-frame transitions 25→14 ms.

### P2 — Second core as the flush/compose engine
`esp_hal::system::CpuControl` can start core 1 with a closure. Two
candidate roles, mutually exclusive in spirit:

**Option 2a — flush service on core 1.** Core 0 renders frame N+1 while
core 1 stages+flushes frame N (today's exact bus code, moved). Needs a
tiny SPSC mailbox (spans list + generation counter) and a "flush busy"
flag core 0 checks before flushing again. With P1 Tier A this is mostly
redundant (DMA already freed the CPU) — only worth it if Tier A fails.

**Option 2b — sensor hub on core 1 (recommended alongside P1).** Move
touch I2C polling + gesture recognition + PMIC polling to core 1,
feeding a lock-free event queue. Core 0's render loop consumes events at
frame top with zero I2C stalls in the render path. This directly fixes:
- the 2-3 ms I2C read tax inside every interactive loop,
- missed lift-offs during long blocking composes (morph/settle frames),
- input latency floor: reports land in the queue the instant the chip
  raises INT, regardless of what the renderer is doing.
Caveats: I2C driver + AXP2101 share the bus — BOTH must move (one owner);
`Instant` is core-safe; the queue needs `critical_section` or atomics
(heapless::spsc). PSRAM/flash cache contention between cores is real but
touch traffic is tiny.

### P3 — SRAM compose cache for hot blits
`write_tinted`/`blend_px` do read-modify-write per pixel against PSRAM.
For dense sprites (glow 96², disc 164², big glyphs) compose the affected
rows in an internal-SRAM scratch strip (say 466×48 px = 44 KiB), then
memcpy whole rows back. Turns N scattered RMWs into one streaming read +
one streaming write per row. Expected: 2-3× on glow/text-heavy frames —
the wheel's worst frames get flatter, not just faster on average.

### P4 — Frame pacing polish (cheap, after P1)
With flush async, the run loop's fixed 25 ms animation cadence can drop
to vsync-ish 16 ms for coast/morph phases (the CO5300 self-refreshes;
we're not racing a scanout). Gate on measured render time so the cadence
never outruns the pipeline.

## 3. Sequencing

1. **Spike P1 Tier A** (1 session): try building a DmaTxBuf/descriptor
   chain over PSRAM; measure a full-frame flush. Go/no-go decides Tier A
   vs Tier B + Option 2a.
2. **P2 Option 2b sensor hub** (1 session): move I2C world to core 1,
   event queue in, recognizer feeds unchanged.
3. **P3 SRAM compose strip** (opportunistic, per-renderer).
4. **P4 pacing** last.

Non-goals: no RTOS, no interrupt-driven render, no triple buffering (a
second 424 KiB PSRAM canvas would double flush hazards for little gain).

## 4. Time app — planned improvements (W5 candidates)

- **Smooth 60 fps seconds arc** once P1 lands (tip redraw is already
  partial; the pacing cadence is the current limiter).
- **Three decorative selectable rings** (the standing W5 contract item):
  seconds / minutes / hours as concentric arcs, tap to cycle which one
  carries the glow tip.
- **Breathing tip**: the arc tip glow breathes on the shared saber tempo
  at rest (identical cadence to the wheel ring and Photos disc).
- **Midnight-blue face tint option** via the Settings Display row.
- **AM/PM + week number** ghosted under the date (TEXT 40%).
- **Anti-aliased sweep tail**: 2 px arc gains a 6 px fading tail behind
  the tip (idempotent MAX writes, same doctrine as today).

## 5. Risks

- PSRAM-sourced DMA on S3 has silicon errata around cache/EDMA
  interaction on some revisions — validate on this exact board early.
- Dual-core + PSRAM cache: both cores share the cache; heavy core-1
  traffic can steal render bandwidth. The sensor hub's traffic is
  negligible; a core-1 flush service's is not (staging copies) — another
  reason to prefer P1 Tier A over Option 2a.
- Everything here preserves: retained canvas, damage tracking, the
  idempotent-write doctrine, and the input model. No renderer rewrites.
