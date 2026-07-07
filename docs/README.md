# OpenPocket Documentation

| Document | What's in it |
|----------|--------------|
| [`../README.md`](../README.md) | Product overview, build & flash |
| [`HARDWARE.md`](HARDWARE.md) | Board truth map: pins, I2C addresses, panel protocol, measured timings, safety rules |
| [`ROADMAP.md`](ROADMAP.md) | Done ✅ + milestones M1–M5 (RTC, touch, unlock image, swipe-to-unlock, burn-in) |
| [`archive/`](archive/) | Engineering journals from the experimental era — every optimization attempt with numbers, including the failed ones |

## Archive guide (read when you need the why behind the display stack)

| Archived doc | Still relevant for |
|--------------|--------------------|
| `archive/08-TIME-DISPLAY-HANDOFF.md` | The full clock/bezel chronology: P0 ease schedules → P1 WatchFb → P3 partial flush → 20 fps cadence, all hardware logs |
| `archive/09-BESPOKE-FRAMEBUFFER-PROMPT.md` | WatchFb + DMI architecture rationale, bitbank2-inspired patterns |
| `archive/02`, `archive/05`, `archive/06` | Frame-time anatomy, PSRAM/SRAM/linker constraints, failed optimization paths (do not repeat) |
| `archive/01`, `archive/03`, `archive/04`, `archive/07` | Raidal shader era (see `Animation-*` branches) |
| `archive/Outdated-plan.txt` | The original watch-OS plan; superseded by `ROADMAP.md` |
