//! AXP2101 PMIC — display power rails + PWRON key events + battery status
//! (docs/HARDWARE.md, docs/research/BUTTONS-RESEARCH.md).
//!
//! DC1 @ 3.3 V and ALDO1 @ 3.3 V power the AMOLED path. No other rail is
//! touched, ever (safety rule: no PMIC changes beyond the documented rails).
//! The power-key feature reads/writes ONLY the IRQ enable/status registers
//! and reads the fuel gauge — it cannot affect power. The chip's long-press
//! hardware power-off (reg 0x27 OFFLEVEL) is deliberately left untouched as
//! the escape hatch.
//!
//! The AXP2101 IRQ line is NOT wired to any ESP32-S3 GPIO on this board, so
//! PWRON events are polled over I2C once per frame: the chip latches events
//! in INTSTS2 (0x49, write-1-to-clear) until read, so polling never misses a
//! press.

use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;

use crate::board::AXP2101_ADDR;

const REG_DC_ONOFF: u8 = 0x80;
const REG_DC_VOL0: u8 = 0x82;
const REG_LDO_ONOFF0: u8 = 0x90;
const REG_LDO_VOL0: u8 = 0x92;

const REG_STATUS1: u8 = 0x00; // bit3 = battery present
const REG_STATUS2: u8 = 0x01; // [7:5] charge direction (1 = charging)
const REG_INTEN2: u8 = 0x41; // IRQ enable bank 2 (power key)
const REG_INTSTS1: u8 = 0x48;
const REG_INTSTS2: u8 = 0x49; // IRQ status bank 2 (W1C)
const REG_INTSTS3: u8 = 0x4A;
const REG_BAT_PCT: u8 = 0xA4; // 0..100 direct

/// Bank-2 power-key bits (same layout in enable 0x41 and status 0x49).
const PKEY_LONG: u8 = 0x04; // bit 2
const PKEY_SHORT: u8 = 0x08; // bit 3

/// Enable DC1 (3.3 V) + ALDO1 (3.3 V) for the display. Values per Waveshare
/// reference: DC_VOL0=18, LDO_VOL0=28.
pub fn enable_display_power(i2c: &mut I2c<'_, Blocking>) -> Result<(), ()> {
    i2c.write(AXP2101_ADDR, &[REG_DC_VOL0, 18]).map_err(|_| ())?;
    let mut dc_ctrl = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_DC_ONOFF], &mut dc_ctrl)
        .map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_DC_ONOFF, dc_ctrl[0] | 0x01])
        .map_err(|_| ())?;

    i2c.write(AXP2101_ADDR, &[REG_LDO_VOL0, 28]).map_err(|_| ())?;
    let mut ldo_ctrl = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_LDO_ONOFF0], &mut ldo_ctrl)
        .map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_LDO_ONOFF0, ldo_ctrl[0] | 0x01])
        .map_err(|_| ())?;

    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PowerKey {
    None,
    ShortPress,
    LongPress,
}

/// One-time init: enable short+long PWRON IRQs, then clear stale flags.
/// CRITICAL ORDERING: the clear comes LAST — the chip latches a PWRON press
/// from before flashing/boot, which would otherwise fire a phantom press on
/// the first poll. Touches no power/shutdown/press-timing registers.
pub fn init_power_key(i2c: &mut I2c<'_, Blocking>) -> Result<(), ()> {
    let mut en = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_INTEN2], &mut en)
        .map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_INTEN2, en[0] | PKEY_SHORT | PKEY_LONG])
        .map_err(|_| ())?;

    i2c.write(AXP2101_ADDR, &[REG_INTSTS1, 0xFF]).map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_INTSTS2, 0xFF]).map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_INTSTS3, 0xFF]).map_err(|_| ())?;
    Ok(())
}

/// Poll once per frame. Returns the newest latched PWRON event (long wins if
/// both are latched). Clears ONLY the key bits it consumed, so battery/VBUS
/// insert-remove flags stay readable by other code.
pub fn poll_power_key(i2c: &mut I2c<'_, Blocking>) -> PowerKey {
    let mut sts = [0u8];
    if i2c
        .write_read(AXP2101_ADDR, &[REG_INTSTS2], &mut sts)
        .is_err()
    {
        return PowerKey::None; // bus hiccup — the touch layer already re-inits on errors
    }
    let hit = sts[0] & (PKEY_SHORT | PKEY_LONG);
    if hit == 0 {
        return PowerKey::None;
    }
    let _ = i2c.write(AXP2101_ADDR, &[REG_INTSTS2, hit]);
    if hit & PKEY_LONG != 0 {
        PowerKey::LongPress
    } else {
        PowerKey::ShortPress
    }
}

/// Battery percentage (0..=100), or None when no battery is present.
pub fn battery_percent(i2c: &mut I2c<'_, Blocking>) -> Option<u8> {
    let mut st1 = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_STATUS1], &mut st1).ok()?;
    if st1[0] & 0x08 == 0 {
        return None;
    }
    let mut pct = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_BAT_PCT], &mut pct).ok()?;
    Some(pct[0].min(100))
}

/// True while the battery is charging (STATUS2 [7:5] == 1).
pub fn is_charging(i2c: &mut I2c<'_, Blocking>) -> bool {
    let mut st2 = [0u8];
    if i2c
        .write_read(AXP2101_ADDR, &[REG_STATUS2], &mut st2)
        .is_err()
    {
        return false;
    }
    (st2[0] >> 5) == 0x01
}
