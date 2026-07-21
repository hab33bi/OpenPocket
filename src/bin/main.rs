//! OpenPocket firmware entry — Waveshare ESP32-S3-Touch-AMOLED-1.75 only.
//!
//! Hardware bring-up (PMIC rails → RTC → touch → QSPI display init), then
//! hands off to the app scene machine (`openpocket::app::App::run`).
//! Pins and addresses: docs/HARDWARE.md.

#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]
#![deny(clippy::large_stack_frames)]

use alloc::vec;

use esp_hal::clock::CpuClock;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c, SoftwareTimeout};
use esp_hal::main;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::{Duration, Instant, Rate};
use esp_println::println;

use openpocket::app::App;
use openpocket::board::{LCD_COL_OFFSET, LCD_HEIGHT, LCD_WIDTH};
use openpocket::display::qspi_bus::{QspiBus, DMA_CHUNK_BYTES};
use openpocket::display::watch_fb::WatchFb;
use openpocket::drivers::{axp2101, cst9217};
use openpocket::input::gestures::SwipeTracker;
use openpocket::scenes::lock::Clock;
use openpocket::time::WallClock;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Print, don't freeze silently — dev builds have overflow checks and a
    // silent panic looks exactly like a hang.
    println!("PANIC: {}", info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(clippy::large_stack_frames)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // 8 KiB SRAM heap (no format!/alloc in hot paths); framebuffer in PSRAM.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 8 * 1024);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    println!("=== OpenPocket ===");

    // Both esp-hal I2C timeouts default OFF on the S3 — a clock-stretching or
    // wedged device then blocks a transaction FOREVER (observed: whole-firmware
    // freeze under touch-read bursts). Bounded timeouts turn hangs into Err,
    // which the touch layer counts and recovers from via chip re-init.
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default()
            .with_frequency(Rate::from_khz(400))
            .with_timeout(BusTimeout::Maximum)
            // 5 ms: every transaction on this bus (touch 15 B, PMIC 2 B, RTC
            // 7 B) completes in <1 ms at 400 kHz — 5 ms is ~10× margin. The
            // CST9217 clock-stretches/wedges transactions when a finger sits
            // at the panel edge (hardware-observed); the timeout caps that
            // stall, and 20 ms of it was a visible hitch at the end of drags.
            .with_software_timeout(SoftwareTimeout::Transaction(Duration::from_millis(5))),
    )
    .unwrap()
    .with_sda(peripherals.GPIO15)
    .with_scl(peripherals.GPIO14);

    match axp2101::enable_display_power(&mut i2c) {
        Ok(()) => println!("AXP2101: OK"),
        Err(()) => println!("AXP2101: FAIL"),
    }
    // PWR-key events (W0): enable PWRON short/long IRQs, clear stale latched
    // flags (ordering matters — see drivers/axp2101.rs). Poll-only: the IRQ
    // line is not wired to a GPIO on this board.
    match axp2101::init_power_key(&mut i2c) {
        Ok(()) => println!(
            "AXP2101 key: OK batt={:?}% charging={}",
            axp2101::battery_percent(&mut i2c),
            axp2101::is_charging(&mut i2c) as u8
        ),
        Err(()) => println!("AXP2101 key: FAIL"),
    }

    // RTC-backed wall clock: seeds the PCF85063 from the build timestamp when
    // the chip lost power (VL) or lags the build (logs the decision).
    let wall = WallClock::init(&mut i2c);

    // CST9217 touch: reset pulse + attribute handshake (chip-ID gate).
    let mut tp_reset = Output::new(peripherals.GPIO40, Level::High, OutputConfig::default());
    let tp_int = Input::new(
        peripherals.GPIO11,
        InputConfig::default().with_pull(Pull::Up),
    );
    match cst9217::init(&mut i2c, &mut tp_reset) {
        Ok(a) => println!(
            "touch: chip=0x{:04X} res={}x{} fw=0x{:08X}",
            a.chip_type, a.res_x, a.res_y, a.fw_version
        ),
        Err(()) => println!("touch: INIT FAILED"),
    }

    // QSPI display bus + CO5300 init (Waveshare reference sequence).
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

    // Retained framebuffer in PSRAM + lock-screen clock.
    let byte_count = (LCD_WIDTH as usize) * (LCD_HEIGHT as usize) * 2;
    let fb0 = vec![0u8; byte_count].leak();

    println!("Lock screen init...");
    let init_start = Instant::now();
    let wfb = WatchFb::new(fb0, LCD_WIDTH, LCD_HEIGHT);
    let clock = Clock::new();
    println!(
        "Clock ready {} ms | bezel anim={} full={} offsets | retained FB {} KiB PSRAM",
        init_start.elapsed().as_millis(),
        clock.bezel_anim_len,
        clock.bezel_full_len,
        byte_count / 1024
    );

    App {
        bus,
        wfb,
        i2c,
        tp_int,
        tp_reset,
        wall,
        clock,
        swipe: SwipeTracker::new(LCD_HEIGHT),
    }
    .run()
}

fn delay_ms(ms: u32) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms as u64) {}
}
