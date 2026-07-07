# OpenPocket

Bespoke pocket-watch firmware for the **Waveshare ESP32-S3-Touch-AMOLED-1.75** — and only that board.

Pure Rust `no_std` (esp-hal, no LVGL, no Slint, no ESP-IDF runtime) with a custom display stack built for watch-grade motion on a 466×466 round AMOLED: a retained framebuffer compositor (`WatchFb`), a dirty-span index for partial DMA flushes (`DMI`), and build-time cubic-Bezier animation schedules so every frame's work is bounded by construction.

## What it does today

A premium lock screen: digital clock (Inter Display Bold, scale-to-gray antialiasing) with a bezel ring that sweeps in on an S-curve with rounded, faded comet-tip ends, heals into a solid ring, and re-runs on every minute change — locked at a fixed 20 fps cadence with zero-cost idle frames (clean frames skip the flush entirely; the CO5300 retains its GRAM).

**In progress** (see [docs/ROADMAP.md](docs/ROADMAP.md)): real RTC time (PCF85063), CST9217 touch, and iPhone-style swipe-up-to-unlock revealing a fullscreen image.

## Hardware

| Component | Part | Where |
|-----------|------|-------|
| SoC | ESP32-S3R8 (dual LX7 @ 240 MHz, 8 MB PSRAM, 16 MB flash) | — |
| Display | CO5300 QSPI AMOLED, 466×466 round | QSPI pins in [docs/HARDWARE.md](docs/HARDWARE.md) |
| Touch | CST9217 | I2C `0x5A`, INT GPIO11, RST GPIO40 |
| PMIC | AXP2101 | I2C `0x34` |
| RTC | PCF85063 | I2C `0x51` |

Full pin map, I2C addresses, panel init sequence, and measured display timings: [docs/HARDWARE.md](docs/HARDWARE.md).

## Build & flash

Requires the espup Xtensa toolchain (`rust-toolchain.toml` pins channel `esp`) and [cargo-espflash](https://github.com/esp-rs/espflash).

```sh
cargo build --release              # xtensa-esp32s3-none-elf (default target)
cargo espflash flash --release --monitor
```

Application flashing only — no bootloader, partition-table, or eFuse changes, ever.

## Repository layout

- `src/` — firmware (display stack, lock-screen clock, drivers)
- `build.rs` — all build-time codegen: font glyph subset, sine LUT, animation ease schedules
- `assets/` — Inter Display Bold (font), Spike.jpg (unlock image source)
- `docs/` — [HARDWARE.md](docs/HARDWARE.md), [ROADMAP.md](docs/ROADMAP.md), and `archive/` (the experimental-era engineering journals; deep performance history lives there)
- `Animation-*` branches — earlier full-screen shader experiments (cloud, gradient, light rays, prebaked radial loop) kept as reference

## Performance culture

Nothing is called "smooth" without numbers. Every milestone logs frame time, flush time, and pixel writes over serial; the journals in `docs/archive/` record every optimization attempt including the failed ones.
