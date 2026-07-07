//! CO5300 QSPI transport — DMA pixel streaming from PSRAM framebuffer.
//!
//! ESP32-S3 GDMA can read external octal PSRAM (`dma_can_access_psram`). The
//! `SpiDmaBus` HAL wrapper still copies each chunk into an internal DMA-capable
//! buffer before TX; we minimise CPU work by storing the framebuffer as
//! display-ready RGB565 big-endian bytes (no per-pixel swap).

use esp_hal::gpio::Output;
use esp_hal::spi::master::{Address, Command, DataMode, SpiDmaBus};
use esp_hal::Blocking;

const QSPI_CMD_WRITE_REG: u16 = 0x02;
const QSPI_CMD_WRITE_PIXELS: u16 = 0x32;
const QSPI_ADDR_PIXEL_RAM: u32 = 0x003C00;

/// 8 KiB chunks — fewer CS/DMA rounds than 4 KiB (Waveshare reference uses 8 KiB).
pub const DMA_CHUNK_BYTES: usize = 8192;

pub struct QspiBus<'d> {
    spi: SpiDmaBus<'d, Blocking>,
    cs: Output<'d>,
}

impl<'d> QspiBus<'d> {
    pub fn new(spi: SpiDmaBus<'d, Blocking>, cs: Output<'d>) -> Self {
        Self { spi, cs }
    }

    #[inline]
    fn cs_low(&mut self) {
        self.cs.set_low();
    }

    #[inline]
    fn cs_high(&mut self) {
        self.cs.set_high();
    }

    pub fn write_command(&mut self, reg: u8) {
        self.cs_low();
        let _ = self.spi.half_duplex_write(
            DataMode::Single,
            Command::_8Bit(QSPI_CMD_WRITE_REG, DataMode::Single),
            Address::_24Bit((reg as u32) << 8, DataMode::Single),
            0,
            &[],
        );
        self.cs_high();
    }

    pub fn write_c8d8(&mut self, reg: u8, data: u8) {
        self.cs_low();
        let _ = self.spi.half_duplex_write(
            DataMode::Single,
            Command::_8Bit(QSPI_CMD_WRITE_REG, DataMode::Single),
            Address::_24Bit((reg as u32) << 8, DataMode::Single),
            0,
            &[data],
        );
        self.cs_high();
    }

    pub fn write_c8d16d16(&mut self, reg: u8, d1: u16, d2: u16) {
        let data = [(d1 >> 8) as u8, d1 as u8, (d2 >> 8) as u8, d2 as u8];
        self.cs_low();
        let _ = self.spi.half_duplex_write(
            DataMode::Single,
            Command::_8Bit(QSPI_CMD_WRITE_REG, DataMode::Single),
            Address::_24Bit((reg as u32) << 8, DataMode::Single),
            0,
            &data,
        );
        self.cs_high();
    }

    /// Stream a PSRAM (or SRAM) RGB565 BE byte buffer to the panel via QSPI DMA.
    pub fn flush_bytes(&mut self, pixels_be: &[u8], scratch: &mut [u8]) {
        if pixels_be.is_empty() {
            return;
        }

        let mut idx = 0usize;
        let mut first = true;

        self.cs_low();
        while idx < pixels_be.len() {
            let chunk = (pixels_be.len() - idx).min(DMA_CHUNK_BYTES);
            scratch[..chunk].copy_from_slice(&pixels_be[idx..idx + chunk]);

            let _ = if first {
                first = false;
                self.spi.half_duplex_write(
                    DataMode::Quad,
                    Command::_8Bit(QSPI_CMD_WRITE_PIXELS, DataMode::Single),
                    Address::_24Bit(QSPI_ADDR_PIXEL_RAM, DataMode::Single),
                    0,
                    &scratch[..chunk],
                )
            } else {
                self.spi.half_duplex_write(
                    DataMode::Quad,
                    Command::None,
                    Address::None,
                    0,
                    &scratch[..chunk],
                )
            };

            idx += chunk;
        }
        self.cs_high();
    }
}