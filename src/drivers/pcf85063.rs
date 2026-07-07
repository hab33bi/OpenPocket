//! PCF85063 RTC @ I2C 0x51 (docs/HARDWARE.md).
//!
//! BCD time registers 0x04–0x0A (ss mm hh dd weekday MM yy, year 00–99 =
//! 2000–2099). Seconds register bit 7 is the VL (clock-integrity / voltage-low)
//! flag: set when the chip lost power since the time was last written. Writing
//! the seconds register clears it.

use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;

use crate::board::PCF85063_ADDR;

const REG_CONTROL_1: u8 = 0x00;
const REG_SECONDS: u8 = 0x04;

/// Broken-down local time as stored in the RTC.
#[derive(Clone, Copy)]
pub struct RtcTime {
    pub year: u16, // 2000..=2099
    pub month: u8, // 1..=12
    pub day: u8,   // 1..=31
    pub hour: u8,  // 0..=23
    pub minute: u8,
    pub second: u8,
}

/// Read the current time and the VL (integrity-lost) flag.
pub fn read(i2c: &mut I2c<'_, Blocking>) -> Result<(RtcTime, bool), ()> {
    let mut regs = [0u8; 7];
    i2c.write_read(PCF85063_ADDR, &[REG_SECONDS], &mut regs)
        .map_err(|_| ())?;
    let vl = regs[0] & 0x80 != 0;
    let t = RtcTime {
        second: bcd_to_bin(regs[0] & 0x7F),
        minute: bcd_to_bin(regs[1] & 0x7F),
        hour: bcd_to_bin(regs[2] & 0x3F),
        day: bcd_to_bin(regs[3] & 0x3F),
        // regs[4] = weekday, unused
        month: bcd_to_bin(regs[5] & 0x1F),
        year: 2000 + bcd_to_bin(regs[6]) as u16,
    };
    Ok((t, vl))
}

/// Set the time (24 h mode, clock running) and clear VL. `weekday` 0–6.
pub fn set(i2c: &mut I2c<'_, Blocking>, t: &RtcTime, weekday: u8) -> Result<(), ()> {
    // Control_1: normal mode, 24 h, STOP=0.
    i2c.write(PCF85063_ADDR, &[REG_CONTROL_1, 0x00])
        .map_err(|_| ())?;
    let buf = [
        REG_SECONDS,
        bin_to_bcd(t.second), // bit7=0 also clears VL
        bin_to_bcd(t.minute),
        bin_to_bcd(t.hour),
        bin_to_bcd(t.day),
        weekday & 0x07,
        bin_to_bcd(t.month),
        bin_to_bcd((t.year - 2000) as u8),
    ];
    i2c.write(PCF85063_ADDR, &buf).map_err(|_| ())
}

#[inline]
fn bcd_to_bin(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0F)
}

#[inline]
fn bin_to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}
