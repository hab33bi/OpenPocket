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
use esp_hal::system::{CpuControl, Stack};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::Blocking;
use esp_println::println;


use core::sync::atomic::{fence, AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use pocket_watch_smoke_test::qspi_bus::{QspiBus, DMA_CHUNK_BYTES};
use pocket_watch_smoke_test::raidal::{Scratch, LOW_W};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const LCD_WIDTH: u16 = 466;
const LCD_HEIGHT: u16 = 466;
const LCD_COL_OFFSET: u16 = 6;
const AXP2101_ADDR: u8 = 0x34;

const FB_LEN: usize = (LCD_WIDTH as usize) * (LCD_HEIGHT as usize) * 2;

/// div=3 (156×156 eval) — WebGL parity compromise per plan.
const RENDER_DIVISOR: u8 = 3;

// Atomics for dual-core row-split eval coordination (core0 drives, signals core1).
static EVAL_ROW_START: AtomicUsize = AtomicUsize::new(0);
static EVAL_ROW_END: AtomicUsize = AtomicUsize::new(0);
static CORE1_READY: AtomicBool = AtomicBool::new(false);
static CORE1_DONE: AtomicBool = AtomicBool::new(false);

// For parallel upscale after eval (reuse cores).
static UPSCALE_ROW_START: AtomicUsize = AtomicUsize::new(0);
static UPSCALE_ROW_END: AtomicUsize = AtomicUsize::new(0);
static UPSCALE_CORE1_READY: AtomicBool = AtomicBool::new(false);
static UPSCALE_CORE1_DONE: AtomicBool = AtomicBool::new(false);

// For flush on app core.
static mut BUS_STATIC: Option<&'static mut QspiBus> = None;
static FLUSH_READY: AtomicBool = AtomicBool::new(false);
static mut FLUSH_FB_PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static FLUSH_DONE: AtomicBool = AtomicBool::new(false);

#[allow(clippy::large_stack_frames)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Reduced heap (8 KiB) + two reclaimed static row buffers (SCRATCH_ROW0/1) for
    // LOW_RGB565 (48KiB) + scratch (~6KiB each). Avoids heap overlap in dram2.
    // Board: ESP32-S3R8 — 512KB SRAM + 384KB ROM + stacked 8MB PSRAM.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 8 * 1024);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    // Dual-core support (esp-hal CpuControl + atomics for coordination).
    // Stack must be 16-byte multiple. Persistent worker loop on app core for row-split eval.
    static mut APP_CORE_STACK: Stack<4096> = Stack::new();
    let mut cpu_control = CpuControl::new(peripherals.CPU_CTRL);

    // Launch persistent app-core worker (never returns; spins on atomics).
    // SAFETY + contract: closure runs on core1; uses secondary scratch + global RAIDAL_PTR.
    let _app_core_guard = unsafe {
        cpu_control.start_app_core(&mut *core::ptr::addr_of_mut!(APP_CORE_STACK), || {
            let mut scratch1 = Scratch::new_secondary(LOW_W);
            loop {
                fence(Ordering::SeqCst);
                let mut did_work = false;

                if CORE1_READY.load(Ordering::SeqCst) {
                    let rs = EVAL_ROW_START.load(Ordering::SeqCst);
                    let re = EVAL_ROW_END.load(Ordering::SeqCst);
                    let p = &raw mut pocket_watch_smoke_test::raidal::RAIDAL_PTR;
                    let shader_ptr = (*p).load(Ordering::SeqCst);
                    if !shader_ptr.is_null() {
                        let shader = &mut *shader_ptr;
                        shader.eval_rows(&mut scratch1, rs, re);
                    }
                    CORE1_DONE.store(true, Ordering::SeqCst);
                    CORE1_READY.store(false, Ordering::SeqCst);
                    did_work = true;
                }
                if UPSCALE_CORE1_READY.load(Ordering::SeqCst) {
                    let rs = UPSCALE_ROW_START.load(Ordering::SeqCst);
                    let re = UPSCALE_ROW_END.load(Ordering::SeqCst);
                    let shader_p = &raw mut pocket_watch_smoke_test::raidal::RAIDAL_PTR;
                    let fb_p = &raw mut pocket_watch_smoke_test::raidal::FB_PTR;
                    let shader_ptr = (*shader_p).load(Ordering::SeqCst);
                    let fb_ptr = (*fb_p).load(Ordering::SeqCst);
                    if !shader_ptr.is_null() {
                        let shader = &mut *shader_ptr;
                        if !fb_ptr.is_null() {
                            let fb_slice = core::slice::from_raw_parts_mut(fb_ptr, FB_LEN);
                            shader.upscale_rows(fb_slice, rs, re);
                        }
                    }
                    UPSCALE_CORE1_DONE.store(true, Ordering::SeqCst);
                    UPSCALE_CORE1_READY.store(false, Ordering::SeqCst);
                    did_work = true;
                }
                if FLUSH_READY.load(Ordering::SeqCst) {
                    let fb_p = &raw mut FLUSH_FB_PTR;
                    let fb_ptr = (*fb_p).load(Ordering::SeqCst);
                    if !fb_ptr.is_null() {
                        let fb_slice = core::slice::from_raw_parts_mut(fb_ptr, FB_LEN);
                        let bus_opt = &raw mut BUS_STATIC;
                        if let Some(bus) = (*bus_opt).as_mut() {
                            bus.flush_bytes(fb_slice);
                        }
                    }
                    FLUSH_DONE.store(true, Ordering::SeqCst);
                    FLUSH_READY.store(false, Ordering::SeqCst);
                    did_work = true;
                }

                if !did_work {
                    core::hint::spin_loop();
                }
            }
        })
    };
    println!("CPU control ready for APP core (dual-core row-split worker running)");

    println!("=== Raidal-2 (WebGL parity) ===");
    println!("Q14 eval + integer upscale | div={RENDER_DIVISOR}");
    // Hardware: ESP32-S3R8 — 512 KiB SRAM + 384 KiB ROM + stacked 8 MB PSRAM + 16 MB Flash
    // (https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.75)

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO15)
    .with_scl(peripherals.GPIO14);

    match axp2101_enable_display_power(&mut i2c) {
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
    // Make bus available to app core for flush pipeline.
    unsafe {
        BUS_STATIC = Some(&mut *(&mut bus as *mut QspiBus));
    }
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

    #[cfg(not(feature = "prebake"))]
    {
    // time-display (P1): single retained WatchFb canvas in PSRAM — incremental
    // ring/text deltas only, no per-frame clear or ping-pong copy (flush is
    // blocking, so no tearing). Clean frames skip the flush entirely (the
    // CO5300 retains its GRAM); the loop is paced to a fixed cadence so the
    // frame-indexed anim schedules run at their designed duration.
    let byte_count = (LCD_WIDTH as usize) * (LCD_HEIGHT as usize) * 2;
    let mut fb0 = vec![0u8; byte_count];

    println!("Entering Time Display (black + scale-to-gray AA Inter text + dual-list solid bezel).");
    println!("Clock (live, no prebake) init...");
    let init_start = Instant::now();
    let mut wfb = pocket_watch_smoke_test::watch_fb::WatchFb::new(&mut fb0, LCD_WIDTH, LCD_HEIGHT);
    let mut clock = pocket_watch_smoke_test::clock::Clock::new();
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
        // Otherwise: partial windowed flush of dirty spans (P3) when the dirty
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

    #[cfg(feature = "prebake")]
    {
        // prebake disabled for blue-gradient (live focus, minimal flash).
        loop { /* never */ }
    }
}

// render_timed removed for animation-cloud live cloud path (simpler single-core eval for now;
// full dual-core row split from main branch can be re-applied to Cloud::eval_rows for max FPS).

fn delay_until(deadline: Instant) {
    while Instant::now() < deadline {}
}

fn axp2101_enable_display_power(i2c: &mut I2c<'_, Blocking>) -> Result<(), ()> {
    const REG_DC_ONOFF: u8 = 0x80;
    const REG_DC_VOL0: u8 = 0x82;
    const REG_LDO_ONOFF0: u8 = 0x90;
    const REG_LDO_VOL0: u8 = 0x92;

    i2c.write(AXP2101_ADDR, &[REG_DC_VOL0, 18]).map_err(|_| ())?;
    let mut dc_ctrl = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_DC_ONOFF], &mut dc_ctrl)
        .map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_DC_ONOFF, dc_ctrl[0] | 0x01])
        .map_err(|_| ())?;

    i2c.write(AXP2101_ADDR, &[REG_LDO_VOL0, 28]).map_err(|_| ())?;
    let mut ldo_ctrl = [0u8];
    i2c.write_read(AXP2101_ADDR, &[REG_LDO_ONOFF0], &mut ldo_ctrl)
        .map_err(|_| ())?;
    i2c.write(AXP2101_ADDR, &[REG_LDO_ONOFF0, ldo_ctrl[0] | 0x01])
        .map_err(|_| ())?;

    Ok(())
}

fn delay_ms(ms: u32) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms as u64) {}
}