//! CO5300 QSPI transport — DMA pixel streaming from PSRAM framebuffer.
//!
//! ESP32-S3 GDMA can read external octal PSRAM (`dma_can_access_psram`). The
//! `SpiDmaBus` HAL wrapper still copies each chunk into an internal DMA-capable
//! buffer before TX; we minimise CPU work by storing the framebuffer as
//! display-ready RGB565 big-endian bytes (no per-pixel swap).

use esp_hal::gpio::Output;
use esp_hal::spi::master::{Address, Command, DataMode, SpiDmaBus};
use esp_hal::Blocking;

use crate::display::dmi::Span;

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

    /// Set the CO5300 drawing window (inclusive panel coords; caller applies
    /// the panel column offset to x).
    pub fn set_window(&mut self, x0: u16, x1: u16, y0: u16, y1: u16) {
        self.write_c8d16d16(0x2A, x0, x1);
        self.write_c8d16d16(0x2B, y0, y1);
    }

    /// Partial flush of dirty row spans (P3, docs/09): window the panel per
    /// dirty region and DMA only those bytes. Runs of consecutive rows with
    /// identical x-extent (what rect damage decomposes into) share one window,
    /// so command overhead is paid per rect, not per row.
    pub fn flush_spans(&mut self, fb: &[u8], spans: &[Span], fb_width: u16, col_offset: u16) {
        let mut i = 0usize;
        while i < spans.len() {
            let s0 = spans[i];
            let mut j = i + 1;
            while j < spans.len() {
                let c = spans[j];
                if c.x0 == s0.x0 && c.x1 == s0.x1 && c.y == spans[j - 1].y + 1 {
                    j += 1;
                } else {
                    break;
                }
            }
            self.flush_rect(fb, s0.x0, s0.x1, s0.y, spans[j - 1].y, fb_width, col_offset);
            i = j;
        }
    }

    /// Window one rect and stream its rows (each ≤ 932 B, under the DMA chunk).
    /// First row carries the pixel-write command; the rest continue with CS held.
    ///
    /// CO5300 windows must be 2-px aligned (start even, end odd — panel RAM is
    /// written in 2-pixel units; unaligned windows displace pixels). Expand the
    /// rect outward to alignment — extra flushed pixels carry correct fb data.
    /// The panel col offset (6) is even, so fb-space alignment holds on-panel.
    fn flush_rect(
        &mut self,
        fb: &[u8],
        x0: u16,
        x1: u16,
        y0: u16,
        y1: u16,
        fb_width: u16,
        col_offset: u16,
    ) {
        let fb_height = (fb.len() / (fb_width as usize * 2)) as u16;
        let x0 = x0.min(fb_width - 1) & !1;
        let x1 = (x1 | 1).min(fb_width - 1);
        let y0 = y0.min(fb_height - 1) & !1;
        let y1 = (y1 | 1).min(fb_height - 1);
        self.set_window(col_offset + x0, col_offset + x1, y0, y1);
        self.write_command(0x2C);

        let row_len = (x1 - x0 + 1) as usize * 2;
        self.cs_low();
        let mut first = true;
        for y in y0..=y1 {
            let start = (y as usize * fb_width as usize + x0 as usize) * 2;
            let Some(row) = fb.get(start..start + row_len) else {
                break;
            };
            let _ = if first {
                first = false;
                self.spi.half_duplex_write(
                    DataMode::Quad,
                    Command::_8Bit(QSPI_CMD_WRITE_PIXELS, DataMode::Single),
                    Address::_24Bit(QSPI_ADDR_PIXEL_RAM, DataMode::Single),
                    0,
                    row,
                )
            } else {
                self.spi
                    .half_duplex_write(DataMode::Quad, Command::None, Address::None, 0, row)
            };
        }
        self.cs_high();
    }

    /// Stream a PSRAM (or SRAM) RGB565 BE byte buffer to the panel via QSPI DMA.
    /// Direct from PSRAM slice (no scratch copy) to let GDMA read PSRAM directly where supported.
    /// This uses DMA + PSRAM better for higher FPS.
    pub fn flush_bytes(&mut self, pixels_be: &[u8]) {
        if pixels_be.is_empty() {
            return;
        }

        let mut idx = 0usize;
        let mut first = true;

        self.cs_low();
        while idx < pixels_be.len() {
            let chunk = (pixels_be.len() - idx).min(DMA_CHUNK_BYTES);
            let chunk_slice = &pixels_be[idx..idx + chunk];

            let _ = if first {
                first = false;
                self.spi.half_duplex_write(
                    DataMode::Quad,
                    Command::_8Bit(QSPI_CMD_WRITE_PIXELS, DataMode::Single),
                    Address::_24Bit(QSPI_ADDR_PIXEL_RAM, DataMode::Single),
                    0,
                    chunk_slice,
                )
            } else {
                self.spi.half_duplex_write(
                    DataMode::Quad,
                    Command::None,
                    Address::None,
                    0,
                    chunk_slice,
                )
            };

            idx += chunk;
        }
        self.cs_high();
    }
}