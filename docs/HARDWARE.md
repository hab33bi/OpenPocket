# OpenPocket Hardware Truth Map

Target board: **Waveshare ESP32-S3-Touch-AMOLED-1.75** (SKU 31261/31262 class). This firmware supports no other board. GPS variant out of scope.

Sources: [Waveshare wiki](https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.75), [waveshareteam/ESP32-S3-Touch-AMOLED-1.75](https://github.com/waveshareteam/ESP32-S3-Touch-AMOLED-1.75) (BSP `pin_config.h` + SensorLib, verified 2026-07-07), plus values proven on this hardware by the running firmware.

## SoC & memory

| | |
|---|---|
| SoC | ESP32-S3R8, dual Xtensa LX7 @ 240 MHz |
| SRAM | 512 KiB (+384 KiB ROM) |
| PSRAM | 8 MB octal, stacked (`esp_alloc::psram_allocator`) |
| Flash | 16 MB |
| Heap policy | 8 KiB SRAM heap; framebuffers in PSRAM; no alloc in hot paths |

## Display — CO5300 QSPI AMOLED, 466×466 round

| Signal | GPIO |
|--------|------|
| CS | 12 |
| SCLK | 38 |
| SIO0–SIO3 | 4, 5, 6, 7 |
| RESET | 39 |

- QSPI @ 80 MHz, DMA in 8 KiB chunks, RGB565 **big-endian** (display-ready, no per-pixel swap)
- **Column offset = 6**: the visible window is columns 6..471 (`0x2A` set accordingly)
- **Window alignment: 2-pixel** — column/row start must be even, end odd. The full-frame window happens to comply; every partial window must be expanded outward (see `QspiBus::flush_rect`). Violations displace pixels.
- Pixel writes: QSPI cmd `0x32`, address `0x003C00`, quad data; continuation writes (no cmd) with CS held
- Brightness: register `0x51` (init sets 0xFF)
- Init sequence: `src/bin/main.rs` (Waveshare reference sequence)

**Measured timings (release build, this firmware):** full-frame flush ≈ 24 ms; partial span flush during animation ≈ 0–9 ms; panel retains GRAM (skipping the flush on unchanged frames is safe and used).

## I2C bus (shared) — SDA GPIO15, SCL GPIO14 @ 400 kHz

| Device | Address | Notes |
|--------|---------|-------|
| AXP2101 PMIC | `0x34` | DC1 3.3 V (reg 0x82=18, enable 0x80 bit0), ALDO1 3.3 V (reg 0x92=28, enable 0x90 bit0) power the display path |
| CST9217 touch | `0x5A` | INT = **GPIO11**, RESET = **GPIO40**. Reference driver: SensorLib `TouchDrvCST92xx` (MIT) in the Waveshare BSP |
| PCF85063 RTC | `0x51` | Powered via AXP2101; keeps time while the board has power. Time regs 0x04–0x0A (BCD), VL flag = reg 0x04 bit 7 (clock-integrity lost) |
| QMI8658 IMU | — | Present on board; unused, kept powered down (out of scope) |
| TCA9554 expander | — | Present on board; not needed for display/touch/RTC on this board (touch RST is a direct GPIO) |

## Other on-board peripherals (out of scope for now)

- ES8311 audio codec + PA (I2S pins 8/9/10/16/45/46, PA enable 46)
- **TF (microSD) card slot** — SDMMC: CLK=GPIO2, CMD=GPIO1, DATA=GPIO3, CS=GPIO41.
  The board has a physical TF slot; a 32 GB card is available for this project.
  Planned as the storage-expansion stage (images, assets, app data) when flash
  capacity or dynamic content demands it — see ROADMAP "Storage stage".
- 8-pin header: 3 GPIOs + UART

## Safety rules (unchanged from day one)

- Application flashing only (`cargo espflash flash`) — never touch eFuses, secure boot, flash encryption, bootloader, or partition table
- No PMIC rail changes beyond the documented display rails
- Avoid prolonged bright static content (AMOLED burn-in); burn-in mitigation is a roadmap milestone
