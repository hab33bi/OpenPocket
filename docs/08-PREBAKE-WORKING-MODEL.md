# Prebake Working Model (from animation experiments)

## Overview
The prebake approach was developed when live Raidal-2 shader (WebGL parity aurora) was too slow on ESP32-S3 (~1 FPS live math).

## Key Components
- **Host Baker** (`src/bin/baker.rs`): 
  - Renders frames using the Rust Raidal2 port at 30 FPS.
  - Finds best seamless period using frame_diff (sum squared diffs) over 1.5-4s.
  - Encodes with delta (XOR from prev) + spatial delta (prev byte) + zero-run RLE.
  - Keyframes every 15 frames.
  - Header + table + payload format (magic "RAID", version, w/h/fps/frames, format=1, kf_interval, payload_offset, entries with offset/len/is_key/is_rle).
  - Time scale tweak or % for periodicity in some experiments.

- **On-Device Player** (`src/prebake.rs`):
  - `PrebakePlayer::from_asset` parses header/table (table_start=28, 10B/entry).
  - On-fly decode in `next_frame`:
    - zero_rle_decode_into (reuses buffer, no per-frame full alloc).
    - spatial_undelta.
    - For key: direct to out.
    - For delta: undelta to out then XOR with prev in place.
  - Keeps `prev` buffer for deltas (434KB).
  - No full pre-decode of all frames (RAM efficient for long loops).

- **Integration in main.rs** (prebake feature):
  - `include_bytes!` for asset.
  - Direct sync flush (bypassed worker for reliability after "first frame glitch").
  - Fixed 30 FPS target or max speed loop.
  - `bus.write_command(0x2C); bus.flush_bytes(...)`

## Optimizations Applied (lessons for future)
- Delta + RLE for compression (raw 19.5MB -> ~9-13MB for 45-60f).
- On-fly decode to save RAM.
- Sync flush to fix stuck animation.
- Spatial delta for better compression on smooth gradients.
- Cross-blend in baker for seam (but later removed as it messed visuals; preferred natural seam or math periodicity via t % period).
- 60 frames chosen as good balance for seamless potential vs flash size (avoid 80% flash usage).

## Issues Encountered & Fixes
- First frame glitch: Switched to direct sync flush.
- Animation messed (bad offsets): Fixed table_start=28, payload_offset parse (20-24).
- RLE bloat: Used zero-run only + conditional.
- Not seamless: Used long bake + best seam search for min diff(last,first), or t modulo for periodicity in generation.
- Flash bloat: Moved away from prebake for new animations; live preferred.
- 45f not seamless: Shader not perfectly periodic at 30fps samples; best seam ~3-6x normal interframe.

## Current Status (as of branch switch)
- Worked for high FPS seamless-ish with 60f live decode.
- Asset ~13MB for 60f fit in 16MB ( ~80% with firmware bad, so avoided for new).
- For light rays etc: Try live first with similar opts (Q14, LUTs, low-res eval + upscale, dual core, direct flush).
- Prebake only if needed for infinite loop seamless (this one can be good for it).

## How to Reproduce / Port Model
1. Implement shader math in fixed point (Q14), reuse/build LUTs for sin/cos.
2. Low res (div=2/3) eval to LOW buffer.
3. On-fly upscale (tables for weights).
4. Delta encode in baker if prebaking: temporal XOR + spatial + zero RLE.
5. Player: parse, decode on fly with prev XOR for deltas.
6. Main loop: update_time, eval low, upscale to fb, direct flush.
7. For seamless: bake long, search min seam for fixed p, or mod t in generation for exact period.

See 07-PREBAKE-PIPELINE.md for original spec.