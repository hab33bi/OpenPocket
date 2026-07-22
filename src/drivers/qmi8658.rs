//! QMI8658 6-axis IMU @ I2C 0x6B (docs/WATER-APP-PLAN.md).
//!
//! The accelerometer supplies the gravity vector that drives the Water
//! liquid simulation ("which way is down" as the watch tilts). Full scale
//! ±4g = 8192 LSB/g; output registers 0x35..0x3A hold X/Y/Z as signed
//! 16-bit little-endian. Shares the 400 kHz bus with the RTC/PMIC/touch.
//!
//! Register map + init profile confirmed against the QST QMI8658C
//! datasheet and the Waveshare ESP32-S3-Touch reference firmware (the
//! ball-physics demo uses the same chip).

use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;

use crate::board::QMI8658_ADDR;

const REG_WHO_AM_I: u8 = 0x00; // = 0x05 on a healthy chip
const REG_CTRL1: u8 = 0x02;
const REG_CTRL2: u8 = 0x03; // accel: [6:4] full-scale, [3:0] ODR
const REG_CTRL3: u8 = 0x04; // gyro
const REG_CTRL7: u8 = 0x08; // enable bits (aEN=0, gEN=1)
const REG_ACC_X_L: u8 = 0x35;

/// Accelerometer sensitivity at the ±4g full-scale we configure.
pub const ACC_LSB_PER_G: i32 = 8192;

/// Configure the IMU: accel ±4g @ 1000 Hz + gyro on. Returns WHO_AM_I
/// (0x05 healthy) or Err on a bus fault — a missing IMU never panics the
/// boot, the Water app simply falls back to a fixed down-vector.
pub fn init(i2c: &mut I2c<'_, Blocking>) -> Result<u8, ()> {
    let mut id = [0u8; 1];
    i2c.write_read(QMI8658_ADDR, &[REG_WHO_AM_I], &mut id)
        .map_err(|_| ())?;
    // CTRL1: address auto-increment on (0x60), the reference profile.
    i2c.write(QMI8658_ADDR, &[REG_CTRL1, 0x60]).map_err(|_| ())?;
    // CTRL2: accel ±4g (0x10) @ 1000 Hz (0x03).
    i2c.write(QMI8658_ADDR, &[REG_CTRL2, 0x13]).map_err(|_| ())?;
    // CTRL3: gyro reference config (unused by the sim today).
    i2c.write(QMI8658_ADDR, &[REG_CTRL3, 0x43]).map_err(|_| ())?;
    // CTRL7: enable accelerometer (bit0) + gyroscope (bit1).
    i2c.write(QMI8658_ADDR, &[REG_CTRL7, 0x03]).map_err(|_| ())?;
    Ok(id[0])
}

/// Read the raw accelerometer vector (signed 16-bit; +1g ≈ ACC_LSB_PER_G
/// along that axis). Axis→screen mapping and rest-offset calibration are
/// the caller's job (see the Water plan).
pub fn read_accel(i2c: &mut I2c<'_, Blocking>) -> Result<(i16, i16, i16), ()> {
    let mut b = [0u8; 6];
    i2c.write_read(QMI8658_ADDR, &[REG_ACC_X_L], &mut b)
        .map_err(|_| ())?;
    let ax = i16::from_le_bytes([b[0], b[1]]);
    let ay = i16::from_le_bytes([b[2], b[3]]);
    let az = i16::from_le_bytes([b[4], b[5]]);
    Ok((ax, ay, az))
}
