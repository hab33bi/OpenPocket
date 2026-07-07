//! OpenPocket firmware entry — Waveshare ESP32-S3-Touch-AMOLED-1.75 only.
//!
//! Boot: PMIC rails → QSPI display init → lock-screen clock loop.
//! Rendering: single retained WatchFb canvas in PSRAM, incremental deltas,
//! DMI partial flush, fixed 20 fps cadence (see docs/HARDWARE.md + docs/ROADMAP.md).

#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]
#![deny(clippy::large_stack_frames)]

use alloc::vec;

use esp_hal::clock::CpuClock;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::main;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::{Duration, Instant, Rate};
use esp_println::println;

use openpocket::board::{LCD_COL_OFFSET, LCD_HEIGHT, LCD_WIDTH};
use openpocket::display::qspi_bus::{QspiBus, DMA_CHUNK_BYTES};
use openpocket::drivers::axp2101;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(clippy::large_stack_frames)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // 8 KiB SRAM heap (no format!/alloc in hot paths); framebuffers live in PSRAM.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 8 * 1024);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    println!("=== OpenPocket ===");
    // Hardware: ESP32-S3R8 — 512 KiB SRAM + 384 KiB ROM + stacked 8 MB PSRAM + 16 MB Flash
    // (https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.75)

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO15)
    .with_scl(peripherals.GPIO14);

    match axp2101::enable_display_power(&mut i2c) {
        Ok(()) => println!("AXP2101: OK"),
        Err(()) => println!("AXP2101: FAIL"),
    }

    let lcd_cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
    let mut lcd_reset = Output::new(peripherals.GPIO39, Level::High, OutputConfig::default());

    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_mhz(80))
        .with_mode(SpiMode::_0);

    let (rx_buf, rx_desc, tx_buf, tx_desc) = dma_buffers!(DMA_CHUNK_BYTES);
    let dma_rx = DmaRxBuf::new(rx_desc, rx_buf).unwrap();
    let dma_tx = DmaTxBuf::new(tx_desc, tx_buf).unwrap();

    let spi = Spi::new(peripherals.SPI2, spi_config)
        .unwrap()
        .with_sck(peripherals.GPIO38)
        .with_sio0(peripherals.GPIO4)
        .with_sio1(peripherals.GPIO5)
        .with_sio2(peripherals.GPIO6)
        .with_sio3(peripherals.GPIO7)
        .with_dma(peripherals.DMA_CH0)
        .with_buffers(dma_rx, dma_tx);

    let mut bus = QspiBus::new(spi, lcd_cs);
    println!("QSPI DMA {} KiB chunks @ 80 MHz", DMA_CHUNK_BYTES / 1024);

    lcd_reset.set_high();
    delay_ms(10);
    lcd_reset.set_low();
    delay_ms(200);
    lcd_reset.set_high();
    delay_ms(200);

    // CO5300 init sequence (Waveshare reference; see docs/HARDWARE.md).
    bus.write_c8d8(0xFE, 0x20);
    bus.write_c8d8(0x19, 0x10);
    bus.write_c8d8(0x1C, 0xA0);
    bus.write_c8d8(0xFE, 0x00);
    bus.write_c8d8(0xC4, 0x80);
    bus.write_c8d8(0x3A, 0x55);
    bus.write_c8d8(0x35, 0x00);
    bus.write_c8d8(0x53, 0x20);
    bus.write_c8d8(0x51, 0xFF);
    bus.write_c8d8(0x63, 0xFF);
    bus.write_c8d16d16(0x2A, LCD_COL_OFFSET, LCD_COL_OFFSET + LCD_WIDTH - 1);
    bus.write_c8d16d16(0x2B, 0, LCD_HEIGHT - 1);
    delay_ms(600);
    bus.write_command(0x11);
    delay_ms(600);
    bus.write_command(0x29);
    delay_ms(20);

    bus.write_c8d16d16(0x2A, LCD_COL_OFFSET, LCD_COL_OFFSET + LCD_WIDTH - 1);
    bus.write_c8d16d16(0x2B, 0, LCD_HEIGHT - 1);
    bus.write_command(0x2C);

    // Lock screen: single retained WatchFb canvas in PSRAM — incremental
    // ring/text deltas only, no per-frame clear or copy (flush is blocking,
    // so no tearing). Clean frames skip the flush entirely (the CO5300
    // retains its GRAM); the loop is paced to a fixed cadence so the
    // frame-indexed anim schedules run at their designed duration.
    let byte_count = (LCD_WIDTH as usize) * (LCD_HEIGHT as usize) * 2;
    let mut fb0 = vec![0u8; byte_count];

    println!("Lock screen init...");
    let init_start = Instant::now();
    let mut wfb = openpocket::display::watch_fb::WatchFb::new(&mut fb0, LCD_WIDTH, LCD_HEIGHT);
    let mut clock = openpocket::scenes::lock::Clock::new();
    println!(
        "Clock ready {} ms | bezel anim={} full={} offsets | retained FB {} KiB PSRAM",
        init_start.elapsed().as_millis(),
        clock.bezel_anim_len,
        clock.bezel_full_len,
        byte_count / 1024
    );

    let anim_start = Instant::now();
    // Fixed 20 fps cadence — matches TARGET_FPS in build.rs so the frame-indexed
    // ease schedules take exactly their designed wall-clock duration.
    const CLOCK_FRAME_US: u64 = 50_000;

    // Prime: WatchFb::new cleared the canvas and marked it fully dirty.
    clock.render(&mut wfb, 0);
    bus.flush_bytes(wfb.bytes());
    wfb.clear_damage();
    println!("First frame: {} ms", anim_start.elapsed().as_millis());

    let mut last_report = Instant::now();
    let mut ema_fps: f32 = 0.0;

    loop {
        let frame_start = Instant::now();
        let elapsed = anim_start.elapsed().as_millis() as u32;

        let render_start = Instant::now();
        clock.render(&mut wfb, elapsed);
        let render_ms = render_start.elapsed().as_millis() as u32;

        // Skip the flush when nothing changed — panel keeps showing its GRAM.
        // Otherwise: partial windowed flush of dirty spans when the dirty
        // area is small; full frame when the DMI overflowed or per-window
        // overhead would exceed a straight full flush.
        let mut flush_ms = 0u32;
        let mut span_count = 0usize;
        let flush_mode = if wfb.is_clean() {
            '-'
        } else {
            let spans = wfb.dmi.spans();
            span_count = spans.len();
            let dirty_bytes: usize = spans
                .iter()
                .map(|s| (s.x1 - s.x0 + 1) as usize * 2)
                .sum();
            let partial = !wfb.dmi.overflowed() && dirty_bytes < byte_count / 3;
            let flush_start = Instant::now();
            if partial {
                bus.flush_spans(wfb.bytes(), spans, LCD_WIDTH, LCD_COL_OFFSET);
            } else {
                // Partial flushes shrink the panel window — restore it first.
                bus.set_window(LCD_COL_OFFSET, LCD_COL_OFFSET + LCD_WIDTH - 1, 0, LCD_HEIGHT - 1);
                bus.write_command(0x2C);
                bus.flush_bytes(wfb.bytes());
            }
            flush_ms = flush_start.elapsed().as_millis() as u32;
            wfb.clear_damage();
            if partial { 'P' } else { 'F' }
        };
        let flushed = flush_mode != '-';

        let work_ms = frame_start.elapsed().as_millis() as u32;
        delay_until(frame_start + Duration::from_micros(CLOCK_FRAME_US));

        let inst_fps = if work_ms > 0 { 1000.0 / work_ms as f32 } else { 0.0 };
        if flushed {
            ema_fps = if ema_fps < 1.0 { inst_fps } else { ema_fps * 0.9 + inst_fps * 0.1 };
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            println!(
                "clock fps~{:.1} render={}ms flush={}ms({}) spans={} work={}ms | centers={} cdelta={} px_writes={}",
                ema_fps,
                render_ms,
                flush_ms,
                flush_mode,
                span_count,
                work_ms,
                clock.last_bezel_centers,
                clock.last_bezel_center_delta,
                clock.last_bezel_writes
            );
            last_report = Instant::now();
        }
    }
}

fn delay_until(deadline: Instant) {
    while Instant::now() < deadline {}
}

fn delay_ms(ms: u32) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms as u64) {}
}
