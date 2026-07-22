//! CO5300 QSPI transport — ping-pong DMA pixel streaming (docs/PIPELINE-PLAN.md).
//!
//! The old `SpiDmaBus` path copied every 8 KiB chunk PSRAM→SRAM and then
//! blocked on its DMA — 54 serialized copy+wait rounds ≈ 24 ms per full
//! frame, of which only ~10.9 ms is wire time (research: docs/research/
//! PIPELINE-RESEARCH.md). This bus owns the lower-level `SpiDma` plus TWO
//! SRAM staging buffers: while chunk N is on the wire, the CPU copies chunk
//! N+1 into the other buffer, hiding the copy overhead behind the wire.
//! Full-frame flush ≈ 12–14 ms, single core, retained canvas unchanged.
//! Small command writes stay blocking (same driver path as before).

use esp_hal::dma::DmaTxBuf;
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Address, Command, DataMode, SpiDma, SpiDmaTransfer};
use esp_hal::Blocking;

use crate::display::dmi::Span;

const QSPI_CMD_WRITE_REG: u16 = 0x02;
const QSPI_CMD_WRITE_PIXELS: u16 = 0x32;
const QSPI_ADDR_PIXEL_RAM: u32 = 0x003C00;

/// Staging chunk size — two of these live in internal SRAM. 16 KiB halves
/// the round count vs the old 8 KiB while staying well under the 32,736 B
/// single-DMA-transfer cap.
pub const DMA_CHUNK_BYTES: usize = 16384;

type Transfer<'d> = SpiDmaTransfer<'d, Blocking, DmaTxBuf>;

pub struct QspiBus<'d> {
    spi: Option<SpiDma<'d, Blocking>>,
    bufs: [Option<DmaTxBuf>; 2],
    cs: Output<'d>,
}

impl<'d> QspiBus<'d> {
    pub fn new(
        spi: SpiDma<'d, Blocking>,
        cs: Output<'d>,
        buf_a: DmaTxBuf,
        buf_b: DmaTxBuf,
    ) -> Self {
        Self {
            spi: Some(spi),
            bufs: [Some(buf_a), Some(buf_b)],
            cs,
        }
    }

    fn pixel_cmd(first: bool) -> (Command, Address) {
        if first {
            (
                Command::_8Bit(QSPI_CMD_WRITE_PIXELS, DataMode::Single),
                Address::_24Bit(QSPI_ADDR_PIXEL_RAM, DataMode::Single),
            )
        } else {
            (Command::None, Address::None)
        }
    }

    /// Kick a one-shot DMA write. A start error is a programming error
    /// (length over the DMA cap) — panic loudly rather than limp.
    fn start(
        spi: SpiDma<'d, Blocking>,
        buf: DmaTxBuf,
        len: usize,
        data_mode: DataMode,
        cmd: Command,
        addr: Address,
    ) -> Transfer<'d> {
        match spi.half_duplex_write(data_mode, cmd, addr, 0, len, buf) {
            Ok(t) => t,
            Err((e, _spi, _buf)) => panic!("qspi dma start: {:?}", e),
        }
    }

    /// Blocking register write (init sequences, windows, brightness): the
    /// exact same driver path the old SpiDmaBus wrapper used.
    fn write_reg_blocking(&mut self, reg: u8, data: &[u8]) {
        let spi = self.spi.take().unwrap();
        let mut buf = self.bufs[0].take().unwrap();
        buf.as_mut_slice()[..data.len()].copy_from_slice(data);
        self.cs.set_low();
        let t = Self::start(
            spi,
            buf,
            data.len(),
            DataMode::Single,
            Command::_8Bit(QSPI_CMD_WRITE_REG, DataMode::Single),
            Address::_24Bit((reg as u32) << 8, DataMode::Single),
        );
        let (spi, buf) = t.wait();
        self.cs.set_high();
        self.spi = Some(spi);
        self.bufs[0] = Some(buf);
    }

    pub fn write_command(&mut self, reg: u8) {
        self.write_reg_blocking(reg, &[]);
    }

    pub fn write_c8d8(&mut self, reg: u8, data: u8) {
        self.write_reg_blocking(reg, &[data]);
    }

    pub fn write_c8d16d16(&mut self, reg: u8, d1: u16, d2: u16) {
        self.write_reg_blocking(reg, &[(d1 >> 8) as u8, d1 as u8, (d2 >> 8) as u8, d2 as u8]);
    }

    /// Set the CO5300 drawing window (inclusive panel coords; caller applies
    /// the panel column offset to x).
    pub fn set_window(&mut self, x0: u16, x1: u16, y0: u16, y1: u16) {
        self.write_c8d16d16(0x2A, x0, x1);
        self.write_c8d16d16(0x2B, y0, y1);
    }

    /// Partial flush of dirty row spans: window the panel per dirty region
    /// and DMA only those bytes. Runs of consecutive rows with identical
    /// x-extent share one window, so command overhead is paid per rect.
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

    /// Window one rect and stream its rows, batching as many rows as fit
    /// into each staging buffer and ping-ponging the two buffers (copy the
    /// next batch while the previous is on the wire). Row boundaries mean
    /// nothing to the panel — the window fills from a continuous burst.
    ///
    /// CO5300 windows must be 2-px aligned (start even, end odd — panel RAM
    /// is written in 2-pixel units; unaligned windows displace pixels).
    /// Expand outward; extra flushed pixels carry correct fb data.
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
        let mut y = y0 as usize;
        let y_end = y1 as usize;

        let spi = self.spi.take().unwrap();
        let mut a = self.bufs[0].take().unwrap();
        let b = self.bufs[1].take().unwrap();
        let cap = a.capacity().min(DMA_CHUNK_BYTES);

        let fill = |buf: &mut DmaTxBuf, y: &mut usize| -> usize {
            let mut n = 0usize;
            while *y <= y_end && n + row_len <= cap {
                let start = (*y * fb_width as usize + x0 as usize) * 2;
                if let Some(row) = fb.get(start..start + row_len) {
                    buf.as_mut_slice()[n..n + row_len].copy_from_slice(row);
                    n += row_len;
                }
                *y += 1;
            }
            n
        };

        self.cs.set_low();
        let n0 = fill(&mut a, &mut y);
        if n0 == 0 {
            self.cs.set_high();
            self.spi = Some(spi);
            self.bufs = [Some(a), Some(b)];
            return;
        }
        let (cmd, addr) = Self::pixel_cmd(true);
        let mut t = Self::start(spi, a, n0, DataMode::Quad, cmd, addr);
        let mut spare = b;
        loop {
            let n = fill(&mut spare, &mut y);
            if n == 0 {
                break;
            }
            let (s, used) = t.wait();
            let (cmd, addr) = Self::pixel_cmd(false);
            t = Self::start(s, spare, n, DataMode::Quad, cmd, addr);
            spare = used;
        }
        let (s, used) = t.wait();
        self.cs.set_high();
        self.spi = Some(s);
        self.bufs = [Some(used), Some(spare)];
    }

    /// Stream a full RGB565-BE buffer to the panel: ping-pong DMA — the CPU
    /// copies chunk N+1 from PSRAM into one staging buffer while chunk N
    /// flies from the other.
    pub fn flush_bytes(&mut self, pixels_be: &[u8]) {
        if pixels_be.is_empty() {
            return;
        }
        let spi = self.spi.take().unwrap();
        let mut a = self.bufs[0].take().unwrap();
        let b = self.bufs[1].take().unwrap();
        let cap = a.capacity().min(DMA_CHUNK_BYTES);
        let len = pixels_be.len();

        self.cs.set_low();
        let n0 = len.min(cap);
        a.as_mut_slice()[..n0].copy_from_slice(&pixels_be[..n0]);
        let (cmd, addr) = Self::pixel_cmd(true);
        let mut t = Self::start(spi, a, n0, DataMode::Quad, cmd, addr);
        let mut idx = n0;
        let mut spare = b;
        while idx < len {
            let n = (len - idx).min(cap);
            spare.as_mut_slice()[..n].copy_from_slice(&pixels_be[idx..idx + n]);
            idx += n;
            let (s, used) = t.wait();
            let (cmd, addr) = Self::pixel_cmd(false);
            t = Self::start(s, spare, n, DataMode::Quad, cmd, addr);
            spare = used;
        }
        let (s, used) = t.wait();
        self.cs.set_high();
        self.spi = Some(s);
        self.bufs = [Some(used), Some(spare)];
    }
}
