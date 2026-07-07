//! Display stack: QSPI transport, retained framebuffer, dirty-span index.

pub mod dmi;
#[cfg(feature = "esp")]
pub mod qspi_bus;
pub mod watch_fb;
