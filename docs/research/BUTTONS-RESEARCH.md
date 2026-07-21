# Hardware Button Support — Register-Level Research

Target board: **Waveshare ESP32-S3-Touch-AMOLED-1.75** (SKU 31261/31262).
Firmware: Rust `no_std`, esp-hal 1.1, Xtensa ESP32-S3. PMIC = **AXP2101 @ I2C 0x34**.

Scope: (1) PWR button on the AXP2101 PWRON pin, short-press detection for the app
switcher (Unlocked) / ring animation (Locked); (2) BOOT button (GPIO0) as a
runtime input. **Research only — no source was modified.**

## Sources (fetched & cited)

| Ref | Source |
|-----|--------|
| **BSP-PIN** | Waveshare BSP `examples/arduino/libraries/Mylibrary/pin_config.h` ([waveshareteam/ESP32-S3-Touch-AMOLED-1.75](https://github.com/waveshareteam/ESP32-S3-Touch-AMOLED-1.75)) |
| **BSP-PMU** | Waveshare esp-idf example `examples/esp-idf/01_AXP2101/main/port_axp2101.cpp` + `main.cpp` + `Kconfig.projbuild` (same repo) |
| **XPL-REG** | XPowersLib `src/REG/AXP2101Constants.h` ([lewisxhe/XPowersLib](https://github.com/lewisxhe/XPowersLib)) |
| **XPL-HPP** | XPowersLib `src/XPowersAXP2101.hpp` (was `.tpp`, renamed) — method implementations, line numbers cited inline |
| **XPL-PAR** | XPowersLib `src/XPowersParams.hpp` — IRQ + press-timing enums |
| **DS** | AXP2101 datasheet (X-Powers). Register names/semantics cross-checked against XPowersLib, which is the de-facto authoritative decode of the datasheet. |
| **ESP-S3** | ESP32-S3 TRM / datasheet — GPIO0 strapping |

> Note on the datasheet PDF: the X-Powers PDFs available online are image/encrypted
> and were **not machine-readable** in this environment. Every register address and
> bit below is taken from XPowersLib source (which the datasheet PDF corroborates in
> its register-name column). Where a *default/reset value* could not be confirmed
> from readable text, it is flagged **(UNVERIFIED — confirm on bench)**.

---

## TL;DR

- **IRQ GPIO: NO.** The AXP2101 IRQ/PMU_INT line is **not wired to any ESP32-S3 GPIO**
  on this board. `pin_config.h` has no PMU_INT pin, and Waveshare's own esp-idf demo
  sets `CONFIG_PMU_INTERRUPT_PIN` **default = -1** and *polls* over I2C (its GPIO-ISR
  path is commented out). **I2C status polling is the only path** → poll register
  `0x49` every frame. [BSP-PIN, BSP-PMU]
- **The 4 key registers**: `0x41` INTEN2 (IRQ enable), `0x49` INTSTS2 (IRQ status,
  **write-1-to-clear**), `0x27` IRQ_OFF_ON_LEVEL_CTRL (press timing), `0xA4`
  battery-percent. PWRON short/long press live in **bit 3 / bit 2** of `0x41`/`0x49`.
- **Safety**: a **short** PWRON press only sets an IRQ flag — it does **not** power off.
  Hardware power-off happens only on a **long** press ≥ OFFLEVEL (reg `0x27[3:2]`,
  4/6/8/10 s). **Do not touch `0x22`, `0x10`, `0x12`, `0x14`, `0x24`** — those govern
  shutdown / BATFET / brown-out. Leave the long-press-off escape hatch at factory.
- **BOOT/GPIO0**: usable as a runtime **active-low** input (external pull-up + BOOT
  button to GND). Strapping only matters at reset. Free on this board.

---

## 1. PWR press detection — registers, bits, config

### 1.1 IRQ register banks [XPL-REG]

```
XPOWERS_AXP2101_INTEN1   = 0x40   // IRQ enable  bank 1
XPOWERS_AXP2101_INTEN2   = 0x41   // IRQ enable  bank 2  <-- power key
XPOWERS_AXP2101_INTEN3   = 0x42   // IRQ enable  bank 3
XPOWERS_AXP2101_INTSTS1  = 0x48   // IRQ status  bank 1
XPOWERS_AXP2101_INTSTS2  = 0x49   // IRQ status  bank 2  <-- power key
XPOWERS_AXP2101_INTSTS3  = 0x4A   // IRQ status  bank 3
```

### 1.2 Power-key bits (bank 2 = reg 0x41 enable / 0x49 status) [XPL-PAR]

The XPowersLib enum encodes bit *positions across the 3-byte space*; bank 2 is the
**second byte**, so subtract 8 to get the in-register bit:

| Enum (`xpowers_axp2101_irq_t`) | Global bit | In reg 0x41/0x49 | Mask | Meaning |
|---|---|---|---|---|
| `PKEY_POSITIVE_IRQ` | `_BV(8)`  | bit 0 | `0x01` | PWRON **positive** edge (release) |
| `PKEY_NEGATIVE_IRQ` | `_BV(9)`  | bit 1 | `0x02` | PWRON **negative** edge (press)   |
| `PKEY_LONG_IRQ`     | `_BV(10)` | bit 2 | `0x04` | PWRON **long** press               |
| `PKEY_SHORT_IRQ`    | `_BV(11)` | bit 3 | `0x08` | PWRON **short** press              |

Same bit layout in the **enable** reg `0x41` and the **status** reg `0x49`.
(Other bits in this bank: `0x10` bat-remove, `0x20` bat-insert, `0x40` vbus-remove,
`0x80` vbus-insert — leave alone unless you also want charge-plug events.)

> **Source conflict, resolved:** an early web summary claimed short-press = bit 1 of
> `0x49`. That is **wrong** for AXP2101. XPowersLib (authoritative) puts **short = bit 3
> (0x08), long = bit 2 (0x04)**. The board's own vendor code uses these masks
> (`PKEY_SHORT_IRQ | PKEY_LONG_IRQ`). Trust XPowersLib. [XPL-PAR, BSP-PMU]

### 1.3 Enable / clear mechanics [XPL-HPP]

- **Enable**: read-modify-write `0x41`, OR-in the desired bits. `setInterruptImpl`
  (line ~3096) does exactly this per-bank; it preserves other banks/bits.
- **Read status**: `getIrqStatus()` (line ~2590) reads `0x48,0x49,0x4A`.
- **Clear = WRITE-1-TO-CLEAR.** `clearIrqStatus()` (line ~2602) writes **`0xFF`** to
  each of `0x48/0x49/0x4A`. Writing a `1` to a status bit clears it; writing `0`
  leaves it. So to consume only the power-key events, write back a mask with just
  bits `0x0C` set to `0x49`. **Confirmed W1C.**

### 1.4 Press-timing config — reg 0x27 `IRQ_OFF_ON_LEVEL_CTRL` [XPL-HPP, XPL-PAR]

`XPOWERS_AXP2101_IRQ_OFF_ON_LEVEL_CTRL = 0x27`.

| Bits | Field | Setter | Options (enum order 0..3) |
|---|---|---|---|
| `[1:0]` (mask `0x03`) | **ONLEVEL** — PWRON power-**on** hold time | `setPowerKeyPressOnTime` (line ~2182, `val&=0xFC \| opt`) | `128ms / 512ms / 1s / 2s` |
| `[3:2]` (mask `0x0C`) | **OFFLEVEL** — PWRON long-press power-**off** time | `setPowerKeyPressOffTime` (line ~2206, `val&=0xF3 \| opt<<2`) | `4s / 6s / 8s / 10s` |
| `[7:4]` | IRQ level / PWROK timing (chip-internal) | — | leave at factory |

Enum values [XPL-PAR]:
```
xpowers_press_on_time_t : POWERON_128MS=0, POWERON_512MS=1, POWERON_1S=2, POWERON_2S=3
xpowers_press_off_time_t: POWEROFF_4S=0,  POWEROFF_6S=1,   POWEROFF_8S=2,  POWEROFF_10S=3
```

**Important:** ONLEVEL is the *boot hold-to-power-on* threshold, **not** the
short-press threshold. The short-press IRQ (`0x49` bit 3) fires from the chip's
internal short/long discriminator **regardless of `0x27`**. **You do not need to
write `0x27` at all** to get reliable short-press detection. If you do write it
(e.g. to lengthen OFFLEVEL), use **read-modify-write** and never disturb bits `[3:2]`
carelessly (shortening OFFLEVEL shortens the hardware power-off escape hatch).

**Safe values**: leave `0x27` untouched (recommended). If you must set ONLEVEL,
`512ms` (=1) is the vendor-typical default. Never set OFFLEVEL below its factory
value without reason.

---

## 2. CRITICAL SAFETY — what powers the board down

### 2.1 Does a short press power off? **No.**
A short PWRON press latches the short-press IRQ (`0x49` bit 3) and continues running.
The AXP2101 only executes a hardware **power-off** when PWRON is held **continuously
≥ OFFLEVEL** (reg `0x27[3:2]`; default 4–6 s). This long-press-off is a **chip-internal
hardware behavior, independent of firmware** — it is the user's guaranteed
"force off / recover from a hung firmware" escape hatch. **Keep it intact.**

### 2.2 Default long-press behavior & the register that controls it — reg 0x22 `PWROFF_EN` [XPL-HPP]
`XPOWERS_AXP2101_PWROFF_EN = 0x22`:

| Bit | Mask | XPowersLib method | Meaning |
|---|---|---|---|
| 0 | `0x01` | `setLongPressRestart` (set) / `setLongPressPowerOFF` (clr) | Long-press action **select**: 1 = restart, 0 = full power-off |
| 1 | `0x02` | `enableLongPressShutdown` (set) / `disableLongPressShutdown` (clr) | **Long-press shutdown ENABLE** ← escape-hatch bit |
| 2 | `0x04` | `enableOverTemperatureLevel2PowerOff` (set) | Over-temp level-2 auto power-off |

**Rule: do NOT write register `0x22`.** Leaving it at factory keeps the long-press
power-off escape hatch working. Clearing bit 1 (`disableLongPressShutdown`) would
**remove** the hardware off — never do that. Default/reset value **(UNVERIFIED — confirm
on bench)**; factory config on shipping boards leaves long-press-off functional.

### 2.3 Registers that can power down / brown-out the board — DO NOT WRITE
| Reg | Name | Hazard |
|---|---|---|
| `0x10` | `COMMON_CONFIG` | **bit0 = software shutdown**, **bit1 = reset/restart**. `shutdown()`/`reset()` set these. A stray write powers the board off *instantly*. |
| `0x12` | `BATFET_CTRL` | Disabling BATFET cuts battery→system power (board dies when unplugged). |
| `0x14` | `MIN_SYS_VOL_CTRL` | Min-system-voltage; wrong value → brown-out reset loop. |
| `0x24` | `VOFF_SET` | System under-voltage power-off threshold; too high → premature shutoff. |
| `0x22` | `PWROFF_EN` | §2.2 — long-press-off & over-temp-off. |
| `0x25` | `PWROK_SEQU_CTRL` | PWROK/reset sequencing. |
| `0x26` | `SLEEP_WAKEUP_CTRL` | Sleep enable → can put PMU to sleep. |
| `0x17` | `RESET_FUEL_GAUGE` | Resets the E-gauge (loses SoC estimate). |
| `0x80`/`0x90` | `DC_ONOFF` / `LDO_ONOFF0` | **DC1 (bit0) & ALDO1 (bit0) are the display 3.3 V rails** already managed by `axp2101::enable_display_power`. Clearing them kills the display; DC1 may be the main 3.3 V. Only ever *set* the documented bits. |
| `0x82`/`0x92` | `DC_VOL0` / `LDO_VOL0` | Rail voltages — already set to `18`/`28`. Wrong value → over/under-volt. |

The button feature touches **only** `0x41`, `0x48/0x49/0x4A`, and *reads* of
`0x00/0x01/0xA4` — none of which can change a power rail. This is inherently safe.

---

## 3. The IRQ line — is it wired to a GPIO? **No.**

Evidence:
1. **`pin_config.h` (BSP-PIN)** defines every used pin (I2C SDA=15 SCL=14, TP_INT=11,
   TP_RESET=40, LCD, I2S, SD) — **there is no `PMU_INT` / `AXP_IRQ` / `PWR_INT`**.
2. **Waveshare esp-idf demo (BSP-PMU)**: `Kconfig.projbuild` sets
   `config PMU_INTERRUPT_PIN … default -1` (i.e. *not connected*), and `main.cpp`
   runs a **polling loop** — `while(1){ pmu_isr_handler(); vTaskDelay(1000ms); }` —
   with the GPIO-ISR install code (`gpio_evt_queue`, `irq_init()`) **commented out**.
3. A code search of the repo for `PMU_INT` returns only Kconfig/library plumbing,
   no board wiring.

**Conclusion:** the AXP2101 `IRQ` (open-drain, active-low) output is not broken out
to the ESP32-S3 on this board. **Poll register `0x49` over I2C.**

**Recommended cadence:** poll once per frame in the main loop (**25–50 ms**). The chip
latches the event until W1C, so a 20–40 fps poll never misses a press and adds
latency ≤ one frame (≤ 50 ms) — well within "low latency." One extra 1-byte
`write_read` per frame on the 400 kHz bus is negligible (~60 µs).

*(If a future board revision does wire IRQ to a GPIO: configure that GPIO as
`Input` with `Pull::Up` — the line is open-drain active-low — and on a falling edge
do the same `0x49` read/decode/W1C. The I2C read is still required to know *which*
event fired; the GPIO only tells you *that* something fired. Given the poll is
already cheap, the GPIO adds little here.)*

---

## 4. BOOT button (GPIO0) as a runtime input

**Usable: yes**, as an active-low momentary input. [ESP-S3, BSP-PIN]

- **Strapping**: GPIO0 selects boot mode **only at reset/power-on** — sampled once:
  high (default, internal pull-up) = SPI/normal boot; low = ROM serial-download
  mode. After boot it is a **normal GPIO** with no strapping effect. Confirmed:
  strapping pins are latched at reset only.
- **Board wiring**: the BOOT button ties GPIO0 to **GND** through the button, with a
  pull-up (external on Waveshare boards, plus the SoC's reset-time pull-up). So it
  reads **High = released, Low = pressed** (active-low).
- **Pull requirement**: configure `Input` with `Pull::Up` (matches the existing
  `tp_int` pattern in `main.rs`). Debounce in software (BOOT buttons bounce
  ~5–20 ms); require N consecutive stable samples or a 20–30 ms settle.
- **Conflict check**: GPIO0 is **absent from `pin_config.h`** — not used by display,
  touch, I2C, I2S, or SD. **Free for runtime use.**
- **Caveats**:
  - Holding BOOT **during reset/flash** enters download mode — expected, harmless at
    runtime, but tell users not to hold it while resetting.
  - GPIO0 also emits a **clock on some boot paths**; irrelevant once the app runs.
  - It is a *SoC* input, so it is available even if I2C is wedged — a useful
    independent fallback, but it is **not** the PWR button the product spec wants
    (that one is the AXP2101 PWRON). Treat GPIO0/BOOT as a secondary/dev input.

---

## 5. Battery percentage & charge status

### 5.1 Battery percent — reg 0xA4 [XPL-REG, XPL-HPP]
```
XPOWERS_AXP2101_BAT_PERCENT_DATA = 0xA4   // 1 byte, 0..100 (%), direct integer
```
`getBatteryPercent()` (line ~2392): returns `-1` if no battery, else the raw byte of
`0xA4` (already a 0–100 percentage from the on-chip **E-gauge** — no scaling).
**Guard:** only valid when a battery is present — check STATUS1 bit 3 first.

### 5.2 Status registers [XPL-HPP]
`XPOWERS_AXP2101_STATUS1 = 0x00`:
| Bit | Method | Meaning |
|---|---|---|
| 5 | `isVbusGood` | VBUS present & good |
| 4 | `getBatfetState` | BATFET on |
| 3 | `isBatteryConnect` | **Battery present** (gate for 0xA4 / voltage) |
| 2 | `isBatInActiveMode` | battery activation mode |
| 1 | `getThermalRegulationStatus` | thermal regulation active |

`XPOWERS_AXP2101_STATUS2 = 0x01`:
| Bits | Method | Meaning |
|---|---|---|
| `[7:5]` | `isCharging`/`isDischarge`/`isStandby` | `>>5`: **1 = charging**, 2 = discharging, 0 = standby |
| 3 | `isVbusIn` | VBUS in = **bit3 == 0** *and* `isVbusGood` |
| `[2:0]` | `getChargerStatus` | 0 tri, 1 pre, 2 CC, 3 CV, **4 done**, 5 stop (`xpowers_chg_status_t`) |

### 5.3 Battery voltage (optional, ADC) [XPL-HPP]
`getBattVoltage()` (line ~2384) reads `ADC_DATA_RELUST0/1` = `0x34/0x35` (H5+L8,
mV) — **requires the battery-voltage ADC channel enabled**: set bit 0 of
`ADC_CHANNEL_CTRL = 0x30` (`enableBattVoltageMeasure`). The **percentage (0xA4) does
not require ADC enable** — the E-gauge runs internally — but if `0xA4` ever reads 0 on
the bench, enabling the ADC channel (`0x30` bit0) and battery detection
(`BAT_DET_CTRL = 0x68` bit0) is the fix Waveshare's init applies. [BSP-PMU]

---

## 6. Implementation sketch (esp-hal blocking I2C, matching `drivers/pcf85063.rs`)

New file would be `src/drivers/axp2101_key.rs` (or extend `axp2101.rs`). Uses the
existing `crate::board::AXP2101_ADDR` (= 0x34) and the `i2c.write` /
`i2c.write_read` style already in `axp2101.rs`.

```rust
//! AXP2101 power-key (PWRON) events + battery status — poll-only.
//! IRQ line is NOT wired to a GPIO on this board (see BUTTONS-RESEARCH.md);
//! we poll INTSTS2 (0x49) every frame. Write-1-to-clear.

use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;
use crate::board::AXP2101_ADDR;

const REG_STATUS1:  u8 = 0x00; // bit3 = battery present, bit5 = vbus good
const REG_STATUS2:  u8 = 0x01; // [7:5] charge dir, bit3 vbus, [2:0] chg state
const REG_INTEN2:   u8 = 0x41; // IRQ enable  bank 2
const REG_INTSTS1:  u8 = 0x48;
const REG_INTSTS2:  u8 = 0x49; // IRQ status  bank 2  (W1C)
const REG_INTSTS3:  u8 = 0x4A;
const REG_BAT_PCT:  u8 = 0xA4;

// bank-2 power-key bits (reg 0x41 / 0x49)
const PKEY_LONG:  u8 = 0x04;   // bit2
const PKEY_SHORT: u8 = 0x08;   // bit3

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PowerKey { None, ShortPress, LongPress }

/// One-time init: enable short+long PWRON IRQs, then clear any stale flags.
/// Does NOT touch power-rail, shutdown, or press-timing registers.
pub fn init_power_key(i2c: &mut I2c<'_, Blocking>) -> Result<(), ()> {
    // Read-modify-write INTEN2 so we don't disturb other enabled IRQs.
    let mut en = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_INTEN2], &mut en).map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_INTEN2, en[0] | PKEY_SHORT | PKEY_LONG])
        .map_err(|_| ())?;

    // CRITICAL ORDERING: clear stale flags LAST. The chip latches a PWRON press
    // that happened before we flashed/booted; without this the first poll would
    // fire a phantom ShortPress. Write-1-to-clear all three status banks.
    i2c.write(AXP2101_ADDR, &[REG_INTSTS1, 0xFF]).map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_INTSTS2, 0xFF]).map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_INTSTS3, 0xFF]).map_err(|_| ())?;
    Ok(())
}

/// Call once per frame (~25–50 ms). Returns the newest PWRON event, if any.
/// Long press wins over short if both are somehow latched.
pub fn poll_power_key(i2c: &mut I2c<'_, Blocking>) -> PowerKey {
    let mut sts = [0u8];
    if i2c.write_read(AXP2101_ADDR, &[REG_INTSTS2], &mut sts).is_err() {
        return PowerKey::None; // bus hiccup — touch layer already re-inits on errors
    }
    let s = sts[0];
    let hit = s & (PKEY_SHORT | PKEY_LONG);
    if hit == 0 {
        return PowerKey::None; // avoid a needless write when nothing latched
    }
    // W1C: write back ONLY the key bits we consumed, so we don't clobber
    // bat/vbus insert-remove flags (0x10/0x20/0x40/0x80) another module may read.
    let _ = i2c.write(AXP2101_ADDR, &[REG_INTSTS2, hit]);
    if s & PKEY_LONG != 0 { PowerKey::LongPress } else { PowerKey::ShortPress }
}

/// Battery %: None if no battery present. (E-gauge is internal; 0xA4 is 0..100.)
pub fn battery_percent(i2c: &mut I2c<'_, Blocking>) -> Option<u8> {
    let mut st1 = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_STATUS1], &mut st1).ok()?;
    if st1[0] & 0x08 == 0 { return None; }          // bit3: battery present?
    let mut pct = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_BAT_PCT], &mut pct).ok()?;
    Some(pct[0].min(100))
}

/// True while charging (STATUS2 [7:5] == 1). Also expose vbus if the UI wants it.
pub fn is_charging(i2c: &mut I2c<'_, Blocking>) -> bool {
    let mut st2 = [0u8];
    if i2c.write_read(AXP2101_ADDR, &[REG_STATUS2], &mut st2).is_err() { return false; }
    (st2[0] >> 5) == 0x01
}
```

**GPIO0/BOOT variant** (independent, no I2C):
```rust
// in main.rs bring-up, alongside tp_int:
let boot_btn = Input::new(peripherals.GPIO0,
    InputConfig::default().with_pull(Pull::Up)); // active-low
// per frame: debounce N stable samples
let pressed = boot_btn.is_low(); // require ~2-3 consecutive lows before acting
```

**Ordering constraints (recap):**
1. Enable IRQs **before** clearing, then clear stale flags **last** in init — kills
   the pre-flash latched-press phantom.
2. Per-frame: read `0x49` → decode → **W1C only the consumed bits**.
3. Never write `0x22/0x10/0x12/0x14/0x24/0x25/0x26` or the rail regs.

---

## 7. Risks & unknowns — verify on bench

| Item | Status | Bench check / log line |
|---|---|---|
| Exact **reset/default value** of `0x27` (ONLEVEL/OFFLEVEL) | UNVERIFIED (PDF unreadable) | Log at boot: `println!("AXP 0x27={:#04x}", read(0x27))`. Confirm OFFLEVEL bits before assuming factory long-press-off time. |
| Exact default of `0x22` (`PWROFF_EN`) | UNVERIFIED | `println!("AXP 0x22={:#04x}", read(0x22))`. Confirm bit1 (long-press shutdown) is set before shipping. |
| **Long-press-off actually works** & at what hold time | Must confirm empirically | Hold PWR; measure seconds to power-off. Expect ~4–6 s. This is the escape hatch. |
| Short vs long **discrimination threshold** (where the chip decides short→`0x08` vs long→`0x04`) | Datasheet ambiguous; not the same as ONLEVEL | Log every `0x49` read with duration: `println!("PWRON sts={:#04x}", s)` while tapping vs holding. Characterize the boundary. |
| Whether a **long** press emits short *then* long, or long only | Unknown | Same log — watch if `0x08` precedes `0x04`. If both, prioritize long (sketch already does) and consider swallowing the paired short. |
| `0xA4` reads **0** without ADC/gauge enable | Possible | If 0, set `0x30` bit0 + `0x68` bit0 (Waveshare does) and re-read. Log `bat%`. |
| **Silicon revision** differences | AXP2101 has known datasheet revisions (V0.2→V1.4) with a few fixed bits; XPowersLib carries a "datasheet v1.4 fixed" note on BATFET | Trust XPowersLib decode; verify chip ID `0x03`==`0x4A`. Log it. |
| **Positive-edge (release) events** if we later want press-and-release UX | Not enabled in sketch | Enable `0x01`/`0x02` in `0x41` if the product wants edge timing; otherwise short-press IRQ is enough. |
| I2C contention with the 20 ms software timeout | Low risk (1-byte reads) | Watch for `poll_power_key` Err spikes under touch bursts; the existing re-init path covers it. |

Suggested one-time boot diagnostic block (remove after characterization):
```rust
// TEMP: dump power-key related regs at boot
for r in [0x00u8,0x01,0x22,0x27,0x41,0x49,0xA4] {
    let mut b=[0u8]; let _=i2c.write_read(AXP2101_ADDR,&[r],&mut b);
    println!("AXP {:#04x} = {:#04x}", r, b[0]);
}
```

---

## 8. Recommended integration plan (for OUR codebase)

**Init (once, in `main.rs` after `axp2101::enable_display_power`):**
1. `axp2101_key::init_power_key(&mut i2c)` — enables PWRON short+long IRQs
   (RMW `0x41 |= 0x0C`) and clears stale flags (`0x48/0x49/0x4A = 0xFF`).
2. Do **not** write `0x27`, `0x22`, or any power/shutdown register. Factory
   long-press-off escape hatch stays intact by default.
3. (Optional, only if `0xA4` reads 0 on bench) enable ADC batt channel `0x30|=0x01`
   and batt detection `0x68|=0x01`.
4. (Optional) add `boot_btn = Input(GPIO0, Pull::Up)` as a secondary/dev input.

**Per frame (in `App::run`'s loop, once per rendered frame, 25–50 ms):**
1. `match axp2101_key::poll_power_key(&mut i2c) {`
   - `ShortPress =>` if **Unlocked** → open app switcher; if **Locked** → trigger ring
     animation (the product behaviors).
   - `LongPress =>` ignore in firmware (let the AXP2101 hardware handle real power-off);
     optionally use as a distinct gesture, but never call `shutdown()`/write `0x10`.
   - `None =>` nothing.
   `}`
2. For the app-switcher status line: `battery_percent(&mut i2c)` (cache it, refresh
   every ~1–2 s not every frame) and `is_charging(&mut i2c)` for a charging glyph.

**Latency**: ≤ 1 frame (≤ 50 ms) — the chip latches the event, so the poll cannot
miss it. **Safety**: the feature reads/writes only IRQ + status + percent registers;
no rail or shutdown register is touched, so it cannot brown out or power off the board.
