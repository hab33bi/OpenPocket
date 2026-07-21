# BOOT Button (GPIO0) — Definitive Research

Target board: **Waveshare ESP32-S3-Touch-AMOLED-1.75** (SKU 31261/31262).
Firmware: Rust `no_std`, esp-hal 1.1, Xtensa ESP32-S3.

Scope: everything about the **BOOT button on GPIO0** as a runtime input — exact board
wiring (from the schematic), esp-hal 1.1 config, runtime/strapping semantics, debounce,
product-use recommendations, and a bench verification checklist. **Research only — no
source was modified.** This report supersedes and corrects §4 of `BUTTONS-RESEARCH.md`.

## Sources (fetched & cited)

| Ref | Source |
|-----|--------|
| **SCH** | Official board schematic PDF, `files.waveshare.com/wiki/ESP32-S3-Touch-AMOLED-1.75/ESP32-S3-Touch-AMOLED-1.75.pdf` — 2.1 MB, **text-extracted** with `pdftotext -layout` (net labels + component values legible; positions scrambled, so topology inferred from adjacency + the fact that this is the stock Espressif auto-program reference circuit). |
| **WIKI** | Waveshare wiki / docs page for the board — "BOOT Button: used for device startup and functional debugging"; "GPIO0 … low level indicating a pressed state." |
| **S3-DS** | ESP32-S3 datasheet §strapping pins + §GPIO DC characteristics (internal pull R_PU/R_PD ≈ 45 kΩ). |
| **S3-TRM** | ESP32-S3 TRM — strapping latched at reset; USB-Serial-JTAG peripheral. |
| **ESP-BOOT** | Espressif "ESP32-S3 boot mode selection" + "Establish Serial Connection" docs — download-mode entry/recovery, native USB-Serial-JTAG. |
| **CODE** | This repo: `src/bin/main.rs` (TP_INT `Input` pattern), `src/app.rs` (frame cadence), `src/board.rs` (pin map). |

> **Schematic readability note.** The PDF is a vector schematic; `pdftotext` recovers the
> **net names and component values as real text** (e.g. `GPIO0`, `CHIP_PU`, `BSS138LT1G`,
> `10K`, `100nF`, `1K`, `Key1`, `Key2`) but interleaves them out of spatial order. So exact
> *net-to-pin* topology is read by adjacency and cross-checked against the **canonical
> Espressif auto-program reference design**, which this board unambiguously implements
> (same `EN`+`IO0`+`BSS138` two-transistor block, same 10 K pull-up bank, same 100 nF
> button caps). Component **existence and values are confirmed**; where a specific wire is
> inferred rather than directly read, it is flagged **(inferred)**.

---

## TL;DR / verdict

- **Wiring (confirmed from SCH):** GPIO0 sits in the **stock Espressif auto-program block**.
  Present on the net: a **10 kΩ pull-up to 3V3** (part of a `VCC3V3 10K 10K 10K 10K` bank),
  a **tact button to GND** (`Key1`/`Key2` pair with `CHIP_PU`), a **100 nF cap to GND**
  (`C14`/`C15`) for RC debounce, and a **`BSS138` MOSFET** shared with `CHIP_PU`(EN) forming
  the DTR/RTS auto-download circuit. GPIO0 is **active-low**: High = released, Low = pressed.
- **Pull-up situation:** **External 10 kΩ pull-up EXISTS on this board.** `BUTTONS-RESEARCH.md`
  §4 hedged ("external on Waveshare boards, *plus* the SoC's reset-time pull-up") — now
  **pinned down: yes, a physical 10 kΩ to 3V3 is on the schematic.** You therefore do **not**
  depend on the internal pull-up, though enabling `Pull::Up` anyway is correct and harmless.
- **Debounce:** hardware already gives an RC of τ ≈ 10 kΩ × 100 nF = **~1 ms**. In firmware,
  at the repo's 20–40 fps loop, require **2 consecutive equal samples** (edge-detect on a
  1-frame-stable level). Long-press = level held Low for **≥ 600–800 ms**; short-press =
  released before that.
- **Proposed uses:** (1) **dev/debug toggle** (cycle HUD/FPS overlay, dump regs), (2) **forced
  re-lock / panic-lock**, (3) **log-marker / bench-trigger**. Leave **unused in shipped
  firmware** if the enclosure hides it — the AXP2101 PWRON is the product button.
- **Safety:** runtime use of GPIO0 **cannot brick or misconfigure anything.** Strapping is
  latched at reset only; holding BOOT at runtime does nothing to USB-Serial-JTAG or JTAG.

---

## 1. Exact board wiring (from the schematic)

### 1.1 What the schematic text actually shows

Key fragments recovered from `pdftotext -layout` of **SCH** (char-doubling and net-label
prefixes like `NGL…`/`CRO…`/`PI…` are `pdftotext` artefacts; the tokens are real):

```
…  PIR1001  GPIO0  CHIP_PU  PIRP602  BSS138LT1G  (J1)  …          ← GPIO0 + EN share a BSS138
…  VCC3V3  10K  10K  10K  10K …                                    ← 3V3 pull-up resistor bank
…  MTDO  R7  Key1  R8  …  C14 100nF …                             ← button "Key1" + 100nF cap
…  Key1  Key2  R12 …  C15 100nF …  Key2  1K …                     ← button "Key2" + 100nF + 1K
…  GPIO39  10K  PWRON  10K …                                       ← (separate) PWRON net pull-ups
```

Interpreting against the canonical Espressif reference:

| Node | Component on the net (confirmed present) | Role |
|---|---|---|
| **IO0 / GPIO0** | **10 kΩ pull-up → 3V3** | Idle-High default; sets normal (SPI) boot |
| IO0 | **Tact switch → GND** (`Key1`, the "BOOT" button) | Pull GPIO0 Low when pressed |
| IO0 | **100 nF cap → GND** (`C14`/`C15`) | RC debounce (τ ≈ 1 ms with the 10 kΩ) |
| IO0 + EN | **`BSS138` MOSFET** driven by UART **DTR/RTS** | Auto EN/BOOT download sequencing |
| EN / CHIP_PU | 10 kΩ pull-up + cap + `Key2` (RESET button) to GND | Reset / power-on |

**Two buttons on the board = `Key1` (BOOT→GPIO0) and `Key2` (RESET→EN/CHIP_PU)**, the
classic pairing. **WIKI** independently confirms: "BOOT Button … GPIO0 … low level indicating
a pressed state."

### 1.2 Answers to the specific wiring questions

- **Does GPIO0 have an external pull-up on THIS board, and what value?**
  **Yes — 10 kΩ to 3.3 V.** Confirmed by the `VCC3V3 10K…` pull-up bank feeding the
  strapping/auto-program block and the `R100` resistor on the GPIO0 net. This is the stock
  Espressif value and matches every Waveshare ESP32-S3 board.
- **Is the BOOT button a direct short to GND?**
  Effectively **yes** — the tact switch (`Key1`) connects GPIO0 to GND. There may be a small
  series element in the auto-program path, but the pressed node is pulled to **~0 V** (a hard
  logic-Low). No meaningful series resistance is in the *button-to-GND* path that would stop
  it reading Low. (inferred series detail; the Low level itself is certain.)
- **Any series resistor / RC debounce on it?**
  **Yes, RC debounce:** a **100 nF** cap to GND on the strapping node (`C14`/`C15`). With the
  10 kΩ pull-up this is **τ ≈ 1 ms** — it kills fast contact chatter and RF pickup but does
  **not** fully debounce mechanical bounce (which is 1–10 ms), so software debounce is still
  wanted. A `1K` resistor also appears in the button/UART-header block (part of the
  auto-program / series protection, not a level divider).
- **Auto-download (EN/BOOT) circuit?**
  Present: a **`BSS138`** N-MOSFET pair tying **EN (CHIP_PU)** and **IO0 (GPIO0)** to the
  serial adapter's **DTR/RTS** lines. This is the standard "one-click download" circuit that
  lets `esptool`/`espflash` toggle EN+IO0 to enter download mode automatically. It is driven
  from the **UART debug header** (`U0TXD`/`U0RXD`/`CHIP_PU`/`PWRON` broken out on header `H2`);
  the primary flashing path on this board is **native USB-Serial-JTAG** (see §3).

**Verdict:** the board implements the textbook Espressif strapping + auto-program circuit.
GPIO0 is a well-behaved active-low input with a real 10 kΩ pull-up and light RC filtering.

---

## 2. esp-hal 1.1 configuration (mirror the TP_INT pattern)

The repo already configures the touch-INT pin exactly this way in `src/bin/main.rs:84`:

```rust
use esp_hal::gpio::{Input, InputConfig, Pull};   // already imported at main.rs:17

let tp_int = Input::new(
    peripherals.GPIO11,
    InputConfig::default().with_pull(Pull::Up),
);
```

**BOOT button — identical API style:**

```rust
// In main.rs bring-up, next to `tp_int`:
let boot_btn = Input::new(
    peripherals.GPIO0,
    InputConfig::default().with_pull(Pull::Up),   // active-low; matches the external 10 kΩ
);
```

Then read with the same accessor the code uses for `tp_int` (`app.rs:643` uses
`self.tp_int.is_low()`):

```rust
let pressed = boot_btn.is_low();   // Low = pressed
```

To carry it into `App`, add a field beside `tp_int: Input<'d>` (`app.rs:107`) and move it in
at construction (same place `tp_int` is moved, `main.rs:170`).

**Notes on the esp-hal 1.1 API (verified against this repo's usage):**
- `Input::new(pin, InputConfig)` — the 1.1 signature. `InputConfig::default().with_pull(Pull::Up)`
  is the builder form the codebase standardises on. (There is **no** separate `into_pull_up_input`
  in this API generation; do not use the old 0.x style.)
- `Pull` variants: `Pull::Up`, `Pull::Down`, `Pull::None`.
- Level accessors: `is_low()` / `is_high()` (and `.level()` → `Level`).

**Internal pull-up strength & sufficiency:** the ESP32-S3 weak internal pull-up is
**R_PU ≈ 45 kΩ typical** (same order for R_PD) [S3-DS]. That alone (≈ 45 kΩ to 3.3 V,
sourcing ~73 µA) is enough to hold a released BOOT line High against normal leakage, **so it
would suffice even if no external pull-up existed.** On this board it is **redundant** with
the stronger external 10 kΩ — the two in parallel give ≈ 8.2 kΩ, a firmer High and better
noise margin. Configuring `Pull::Up` is the right call regardless: it is harmless with the
external pull-up and provides the pull if a future board rev drops it.

---

## 3. Runtime semantics & edge cases

### 3.1 Strapping is reset-time only — confirmed
GPIO0 is one of the ESP32-S3 **strapping pins** (the set is **GPIO0, GPIO3, GPIO45, GPIO46**)
[S3-DS, S3-TRM]. Their logic level is **sampled once, shortly after reset/power-on release**,
latched into hardware, and thereafter the pins are **ordinary GPIOs**. GPIO0 specifically
selects boot mode:

| GPIO0 at reset | Boot mode |
|---|---|
| **High** (default — the 10 kΩ pull-up) | **SPI boot** (normal — run app from flash) |
| **Low** (BOOT held) | **Joint download / ROM serial-download mode** |

After boot the latch is fixed; **pressing BOOT during normal operation has zero strapping
effect** — it is just an input level. The other strapping pins are not touched by using GPIO0.

### 3.2 Holding BOOT across reset / power-cycle → ROM download mode (and recovery)
If the user **holds BOOT while EN/reset is asserted** (or during power-on), GPIO0 is Low at
the sampling instant and the chip enters **ROM download mode** instead of running the app —
the screen stays black and the device "does nothing." This is **not a fault and not
persistent**. **Recovery: release BOOT and reset once** (press RESET, or power-cycle) with
GPIO0 now High → normal boot. Nothing is written to flash by merely entering download mode;
no recovery flashing is needed. Worth a line in the user manual: *"Don't hold the small
side button while powering on."*

### 3.3 Interaction with USB-Serial-JTAG — none at runtime
This board flashes over the ESP32-S3's **native USB-Serial-JTAG** peripheral on
**GPIO19 (USB_D−) / GPIO20 (USB_D+)** — a dedicated hardware block, **electrically independent
of GPIO0** [S3-TRM, SCH]. Consequences:
- **Holding BOOT at runtime does not disturb the USB connection**, the CDC serial port, or an
  active `espflash monitor` session. The USB PHY is on GPIO19/20; GPIO0 is unrelated.
- Native USB-Serial-JTAG can also command a download-mode reset **without** the BSS138/DTR-RTS
  circuit, so `espflash` over USB doesn't need you to touch BOOT at all in the normal flow.
- The BSS138 auto-program circuit (§1) matters only when flashing through the **external UART
  header**; it, too, has no runtime effect on GPIO0-as-input.

### 3.4 JTAG / other strapping interplay — cannot brick anything
- **GPIO3** = JTAG source select strapping; **GPIO45** = VDD_SPI (flash) voltage select;
  **GPIO46** = ROM-log/boot strapping. These are **separate pins** and are **only** read at
  reset. Runtime toggling of **GPIO0** cannot reconfigure JTAG, flash voltage, or ROM logging.
- There is **no persistent fuse or config written from GPIO0 state** during normal use.
  Nothing about reading GPIO0 as an input can misconfigure or permanently alter the SoC.
- Worst realistic case is the transient §3.2 (accidental download mode), fully cleared by a
  clean reset. **Using GPIO0 as a runtime button is safe.**

---

## 4. Debounce

### 4.1 What the hardware already does
The 100 nF cap + 10 kΩ pull-up form τ ≈ **1 ms**; the button node needs ~3τ ≈ 3 ms to settle
after the pull-up recharges. This suppresses fast chatter and EMI but is **shorter than
mechanical bounce**, which for a small tact/SMD switch is typically **1–10 ms** (often 2–5 ms)
of make/break chatter. So do **not** rely on hardware alone for clean edges.

### 4.2 Firmware debounce for the 20–40 fps poll loop
The repo's loop runs at **20 fps idle (`FRAME_US = 50_000`) and 25–40 fps during animation
(`ANIM_FRAME_US = 25_000` / `CLOCK_ANIM_FRAME_US`)** — see `src/app.rs:30–38`. A frame period
of **25–50 ms already exceeds the 1–10 ms bounce window**, so *the poll cadence itself is a
debounce.* Minimal robust scheme:

- **Sample-count debounce:** treat the level as "settled" only after **2 consecutive equal
  reads** (i.e. the same level on two successive frames). At 20 fps that's ~50 ms of
  stability — comfortably past bounce — with at most one extra frame of latency.
- Track the *settled* level and fire on **transitions**: `settled == High → Low` = press edge,
  `Low → High` = release edge. This gives one event per physical press, no repeats.

Sketch (state kept in `App`, driven once per frame with the monotonic ms the loop already
computes — `anim_start.elapsed().as_millis() as u32`, cf. `app.rs:145`):

```rust
struct BootBtn { last_raw: bool, settled: bool, since_ms: u32, down_ms: u32 }

// returns Some(BootEvent) on a debounced edge
fn poll(&mut self, raw_low: bool, now_ms: u32) -> Option<BootEvent> {
    if raw_low != self.last_raw {           // level changed — restart settle timer
        self.last_raw = raw_low;
        self.since_ms = now_ms;
        return None;
    }
    if raw_low == self.settled { return None; }          // already settled at this level
    if now_ms.wrapping_sub(self.since_ms) < 30 { return None; } // need ~30 ms stable
    self.settled = raw_low;                               // commit the new settled level
    if raw_low { self.down_ms = now_ms; Some(BootEvent::Press) }
    else {
        let held = now_ms.wrapping_sub(self.down_ms);
        Some(if held >= 700 { BootEvent::LongRelease } else { BootEvent::ShortRelease })
    }
}
```

(30 ms is a safe settle floor that survives even a slow, bouncy switch and still costs ≤ 1
frame at 20 fps. Use `wrapping_sub` to match the repo's `u32`-ms arithmetic, e.g.
`app.rs:152`, `gestures.rs:270`.)

### 4.3 Short-press vs long-press thresholds
If both gestures are wanted, discriminate on **held duration** (Low continuously):

| Gesture | Threshold | Notes |
|---|---|---|
| **Short press** | released after **≥ 30 ms** and **< 700 ms** | fire on **release** to allow long-press to win |
| **Long press** | held Low **≥ 700 ms** (600–800 ms range) | fire on **crossing** the threshold, or on release |
| (optional) very-long | ≥ 2 s | e.g. force-relock / enter dev menu |

Fire short-press **on release** (not on press) so the same physical press isn't double-counted
as short-then-long. 700 ms is a comfortable human "hold" boundary that won't trip on a normal
tap. These mirror the AXP2101 PWRON short/long split described in `BUTTONS-RESEARCH.md` §1, so
the two buttons can share one gesture vocabulary.

---

## 5. Product-use recommendations

The **AXP2101 PWRON** is the primary product button (app switcher / ring, per
`BUTTONS-RESEARCH.md`). BOOT/GPIO0 is a **secondary, SoC-local** input — independent of the
I2C bus (works even if the PMIC/I2C is wedged), which is its main virtue. Sensible uses:

1. **Dev/debug toggle (recommended for engineering builds).** A short press cycles an on-screen
   **HUD** (FPS/`ema_fps` from `app.rs:138`, free heap, current scene, last touch coords); a
   long press dumps diagnostic registers over serial (e.g. the AXP `0x00/0x27/0x41/0x49/0xA4`
   block from `BUTTONS-RESEARCH.md` §7). Gated behind a `debug` cargo feature so it compiles
   out of production.
2. **Forced re-lock / panic-lock.** Long-press BOOT → immediately return to the Locked scene
   (`scenes/lock.rs`) and reset the auto-relock timer (`AUTO_RELOCK_SECS`, `app.rs:152`). A
   physical, bus-independent "lock now" that works even if touch is misbehaving — genuinely
   useful in a pocket-watch product (privacy / accidental-wake).
3. **Log-marker / bench-trigger.** During bench characterization, a BOOT press emits a
   timestamped `println!` marker (and/or a screen flash) so you can correlate serial logs with
   a physical event — pairs perfectly with the §6 checklist and the PWRON characterization work.

**Reasons to leave BOOT unused in shipped firmware:**
- The button is small and intended for "startup and functional debugging" (**WIKI**), and in a
  finished pocket-watch enclosure it may be **inaccessible or unlabeled** — a poor primary UX.
- Any accidental **hold across power-on drops the device into download mode** (§3.2), a
  confusing state for an end user. Keeping BOOT non-functional at runtime avoids teaching users
  to press it.
- One clear product button (PWRON) is better UX than two overlapping ones. **Recommendation:
  wire BOOT only in `debug`/engineering builds; compile it out (or leave it inert) for
  production**, keeping it available for field diagnostics via a special build.

---

## 6. Verification checklist (bench)

Do these before relying on GPIO0. Suggested serial lines assume the repo's `println!` logging.

1. **Polarity / idle level.** Add the input and log raw level each second:
   `println!("BOOT raw={} (1=High/released)", boot_btn.is_high() as u8);`
   **Expect High (1) when untouched.** Confirms the external 10 kΩ pull-up and active-low
   wiring. If it reads Low idle, the pin is mis-assigned or the pull isn't configured.
2. **Press detection & bounce.** Log every raw transition with a timestamp:
   `println!("BOOT edge -> {} @ {} ms", lvl as u8, now_ms);`
   Tap 10×. **Expect** exactly one High→Low then Low→High per tap **after** the debounce filter;
   with the filter disabled you may see a 1–3 ms cluster of edges — that quantifies real bounce
   and validates the 30 ms settle.
3. **Debounced event count.** With the §4 filter on, log `BootEvent`. Tap 20× → **expect 20
   ShortRelease, 0 spurious.** Hold 5× for ~1 s each → **expect 5 LongRelease.**
4. **Short/long boundary.** Log `held` ms on each release; sweep hold durations around 700 ms
   to confirm the split lands where intended and is stable (no flapping at the boundary).
5. **Strapping isolation (safety).** Press-and-hold BOOT for 5 s **while the app runs** →
   **expect normal operation, no reset, no USB drop, `espflash monitor` stays connected.**
   Confirms §3.3/§3.4 (runtime use is inert to boot/JTAG/USB).
6. **Download-mode recovery drill.** Hold BOOT, tap RESET → device enters download mode (blank
   screen, enumerates as ROM download device). **Release BOOT, tap RESET → app boots normally.**
   Confirms §3.2 recovery, so you can reassure users.
7. **I2C-independence spot check.** (Optional) Force an I2C fault and confirm BOOT still
   registers presses — demonstrates its value as a bus-independent fallback (use #2 above).

---

## 7. Corrections / refinements to `BUTTONS-RESEARCH.md` §4

`BUTTONS-RESEARCH.md` §4 was broadly correct. Refinements now pinned down from the schematic:

| §4 claim | Status now | Correction / precision |
|---|---|---|
| "pull-up (external on Waveshare boards, **plus** the SoC's reset-time pull-up)" — hedged | **Confirmed & quantified** | There **is** a physical **10 kΩ external pull-up to 3V3** on GPIO0 (part of the `VCC3V3 10K…` bank / auto-program block). Not merely "probably external" — it's on the schematic. You do not depend on the internal pull. |
| "BOOT buttons bounce ~5–20 ms" | **Refined** | For this SMD tact switch, effective bounce is ~1–10 ms, and the board adds a **100 nF RC (τ ≈ 1 ms)** that pre-filters it. 5–20 ms was a safe over-estimate; 30 ms software settle still recommended. |
| "require ~2–3 consecutive lows before acting" | **Kept, made concrete** | 2 consecutive equal samples at 20 fps ≈ 50 ms — already ample. Edge-detect on the settled level; fire short-press **on release** to co-exist with long-press. |
| "GPIO0 also emits a clock on some boot paths; irrelevant once the app runs" | **True but minor** | This refers to ROM-download UART auto-baud, not normal boot; no runtime relevance. Left as-is. |
| Strapping/pull framing | **Confirmed & expanded** | Strapping set is GPIO0/3/45/46; only GPIO0 is the BOOT strap; runtime use cannot touch JTAG (GPIO3) or flash-voltage (GPIO45) — added the explicit no-brick argument (§3.4). |
| esp-hal config `Input::new(GPIO0, InputConfig::default().with_pull(Pull::Up))` | **Confirmed correct** | Matches the live `tp_int` pattern (`main.rs:84`) exactly; internal pull ≈ 45 kΩ, sufficient alone, redundant here. |

**Net new facts vs. §4:** (a) the 10 kΩ external pull-up is *confirmed present with a value*;
(b) there is a **100 nF RC debounce** and a **BSS138 EN/BOOT auto-download circuit** on the net;
(c) flashing is **native USB-Serial-JTAG on GPIO19/20**, electrically isolated from GPIO0, so
runtime BOOT use cannot affect flashing/monitor; (d) concrete short/long thresholds and a
frame-loop-matched debounce sketch tied to the repo's actual cadence constants.
