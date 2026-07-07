//! Board constants — Waveshare ESP32-S3-Touch-AMOLED-1.75 ONLY.
//!
//! Pin numbers and addresses are documented (with sources) in docs/HARDWARE.md.
//! GPIO peripherals themselves are claimed in `bin/main.rs`; these constants are
//! the numeric truth shared across modules.

/// CO5300 round AMOLED panel.
pub const LCD_WIDTH: u16 = 466;
pub const LCD_HEIGHT: u16 = 466;
/// Visible window starts at panel column 6 (CASET offset).
pub const LCD_COL_OFFSET: u16 = 6;

/// I2C bus (SDA=GPIO15, SCL=GPIO14, 400 kHz) device addresses.
pub const AXP2101_ADDR: u8 = 0x34;
pub const PCF85063_ADDR: u8 = 0x51;
pub const CST9217_ADDR: u8 = 0x5A;

/// Touch controller pins (driver claims them at init).
pub const TP_INT_GPIO: u8 = 11;
pub const TP_RESET_GPIO: u8 = 40;
