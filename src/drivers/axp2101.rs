//! AXP2101 PMIC — display power rails only (docs/HARDWARE.md).
//!
//! DC1 @ 3.3 V and ALDO1 @ 3.3 V power the AMOLED path. No other rail is
//! touched, ever (safety rule: no PMIC changes beyond the documented rails).

use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;

use crate::board::AXP2101_ADDR;

const REG_DC_ONOFF: u8 = 0x80;
const REG_DC_VOL0: u8 = 0x82;
const REG_LDO_ONOFF0: u8 = 0x90;
const REG_LDO_VOL0: u8 = 0x92;

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
