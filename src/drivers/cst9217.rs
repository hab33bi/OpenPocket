//! CST9217 capacitive touch @ I2C 0x5A (docs/HARDWARE.md).
//!
//! Ported verbatim from SensorLib `TouchDrvCST92xx.cpp` (MIT, Lewis He) in the
//! Waveshare BSP — register values and timing are theirs, not invented:
//! - Init: RST pulse (low 10 ms → high, 30 ms boot wait) → command mode
//!   `[0xD1,0x01]` → checkcode `[0xD1,0xFC]` (hi16 must be 0xCACA) → resolution
//!   `[0xD1,0xF8]` → chip type `[0xD2,0x04]` (0x9217) → fw version `[0xD2,0x08]`
//!   → normal mode `[0xD1,0x09]`.
//! - Report: write `[0xD0,0x00]`, read 15 bytes, then ack `[0xD0,0x00,0xAB]`.
//!   buf[6] must be 0xAB; finger count = buf[5] & 0x7F; finger 0 in buf[0..5]:
//!   evt = b0 & 0x0F (0x06 = contact), x = b1<<4 | b3>>4, y = b2<<4 | b3&0x0F.
//!   The raw Y axis is inverted vs the panel — `read_touch` flips it so
//!   callers always get display coordinates (y=0 at the physical top).
//! - INT (GPIO11) pulses per report; it is not a continuous level.

use esp_hal::gpio::Output;
use esp_hal::i2c::master::I2c;
use esp_hal::time::{Duration, Instant};
use esp_hal::Blocking;

use crate::board::{CST9217_ADDR, LCD_HEIGHT};

const CHIP_ID_CST9217: u16 = 0x9217;
const CHIP_ID_CST9220: u16 = 0x9220;
const ACK: u8 = 0xAB;

/// Attributes read during init (logged as the bring-up go/no-go gate).
#[derive(Clone, Copy)]
pub struct Attributes {
    pub chip_type: u16,
    pub res_x: u16,
    pub res_y: u16,
    pub fw_version: u32,
}

/// One touch report (first finger).
#[derive(Clone, Copy)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub fingers: u8,
    /// evt == 0x06 (contact). false = lift-off report.
    pub pressed: bool,
}

/// Reset + attribute handshake. Chip-ID mismatch or missing firmware → Err.
pub fn init(i2c: &mut I2c<'_, Blocking>, rst: &mut Output) -> Result<Attributes, ()> {
    rst.set_low();
    delay_ms(10);
    rst.set_high();
    delay_ms(30); // exit boot mode

    // Enter command mode.
    i2c.write(CST9217_ADDR, &[0xD1, 0x01]).map_err(|_| ())?;
    delay_ms(10);

    let mut buf4 = [0u8; 4];
    i2c.write_read(CST9217_ADDR, &[0xD1, 0xFC], &mut buf4)
        .map_err(|_| ())?;
    let checkcode = u32::from_le_bytes(buf4);
    if checkcode & 0xFFFF_0000 != 0xCACA_0000 {
        return Err(());
    }

    i2c.write_read(CST9217_ADDR, &[0xD1, 0xF8], &mut buf4)
        .map_err(|_| ())?;
    let res_x = u16::from_le_bytes([buf4[0], buf4[1]]);
    let res_y = u16::from_le_bytes([buf4[2], buf4[3]]);

    i2c.write_read(CST9217_ADDR, &[0xD2, 0x04], &mut buf4)
        .map_err(|_| ())?;
    let chip_type = u16::from_le_bytes([buf4[2], buf4[3]]);
    if chip_type != CHIP_ID_CST9217 && chip_type != CHIP_ID_CST9220 {
        return Err(());
    }

    let mut buf8 = [0u8; 8];
    i2c.write_read(CST9217_ADDR, &[0xD2, 0x08], &mut buf8)
        .map_err(|_| ())?;
    let fw_version = u32::from_le_bytes([buf8[0], buf8[1], buf8[2], buf8[3]]);
    if fw_version == 0xA5A5_A5A5 {
        return Err(()); // no firmware in the touch IC
    }

    // Back to normal reporting mode.
    i2c.write(CST9217_ADDR, &[0xD1, 0x09]).map_err(|_| ())?;

    // Disable the auto low-power scan mode (SensorLib reg 0xD106): the chip
    // otherwise drops its scan/report rate after ~10 s idle, which presented
    // as "touch stops working once the startup animation ends". We are not
    // power-constrained on the dev bench; revisit with the M5 power milestone.
    i2c.write(CST9217_ADDR, &[0xD1, 0x06]).map_err(|_| ())?;

    Ok(Attributes {
        chip_type,
        res_x,
        res_y,
        fw_version,
    })
}

/// Read the current report. `Err` = I2C transaction failed (bus health signal);
/// `Ok(None)` = no valid/new report; a report with `pressed == false` is an
/// explicit lift-off event.
pub fn read_touch(i2c: &mut I2c<'_, Blocking>) -> Result<Option<TouchPoint>, ()> {
    let mut buf = [0u8; 15]; // 2 fingers × 5 + 5, per reference driver
    i2c.write_read(CST9217_ADDR, &[0xD0, 0x00], &mut buf)
        .map_err(|_| ())?;
    i2c.write(CST9217_ADDR, &[0xD0, 0x00, ACK]).map_err(|_| ())?;

    if buf[6] != ACK {
        return Ok(None);
    }
    let fingers = buf[5] & 0x7F;
    if fingers == 0 || fingers > 2 {
        return Ok(None);
    }
    let evt = buf[0] & 0x0F;
    let x = ((buf[1] as u16) << 4) | ((buf[3] as u16) >> 4);
    let y_raw = ((buf[2] as u16) << 4) | ((buf[3] as u16) & 0x0F);
    // The controller's Y axis is inverted relative to the panel on this board
    // (raw y=0 = physical BOTTOM; hardware-verified 2026-07-21: a physical
    // bottom-edge swipe-up read as a top-zone swipe-down). Flip here so every
    // consumer sees panel-true coordinates. X orientation is unverified —
    // nothing direction-sensitive reads it yet (drag classification is
    // Y-dominant; X only feeds the symmetric |dx| slop check).
    let y = (LCD_HEIGHT - 1).saturating_sub(y_raw);
    Ok(Some(TouchPoint {
        x,
        y,
        fingers,
        pressed: evt == 0x06,
    }))
}

fn delay_ms(ms: u32) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms as u64) {}
}
