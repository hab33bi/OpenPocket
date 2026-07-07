//! I2C peripheral drivers (shared bus: SDA=GPIO15, SCL=GPIO14 @ 400 kHz).

#[cfg(feature = "esp")]
pub mod axp2101;
#[cfg(feature = "esp")]
pub mod pcf85063;
