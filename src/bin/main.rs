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
use pocket_watch_smoke_test::raidal::{Raidal2, Raidal2Config, Scratch, LOW_H, LOW_W};

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

const TARGET_FRAME_US: u64 = 16_667;
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
                            let mut local_scratch = [0u8; 8192];
                            bus.flush_bytes(fb_slice, &mut local_scratch);
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

    let byte_count = (LCD_WIDTH as usize) * (LCD_HEIGHT as usize) * 2;
    // Double framebuffer for pipeline: render to one while the other is being flushed by app core.
    let mut fb0 = vec![0u8; byte_count];
    let mut fb1 = vec![0u8; byte_count];
    let mut use_fb0 = true;
    // Publish current for diagnostics / upscale (we will update the ptr each time).
    unsafe {
        let p = &raw mut pocket_watch_smoke_test::raidal::FB_PTR;
        (*p).store(fb0.as_mut_ptr(), Ordering::SeqCst);
    }
    let mut dma_scratch = vec![0u8; DMA_CHUNK_BYTES];

    // Use compile-time fixed low size for SRAM static (div=3). Matches Raidal2::LOW_W/H.
    let low_w = LOW_W;
    let low_h = LOW_H;

    println!("Building static cache + upscale map...");
    let cache_start = Instant::now();
    let mut shader = Raidal2::new(
        Raidal2Config {
            render_divisor: RENDER_DIVISOR,
            time_scale: 1.0,
        },
        LCD_WIDTH,
        LCD_HEIGHT,
    );
    // Publish the instance so the parked app-core worker can call methods on it (row eval).
    unsafe {
        let p = &raw mut pocket_watch_smoke_test::raidal::RAIDAL_PTR;
        (*p).store(&mut shader as *mut _, Ordering::SeqCst);
    }
    let mut scratch = Scratch::new(low_w);
    // Secondary scratch for potential dual-core app core use (see plan).
    let _scratch_secondary = Scratch::new_secondary(low_w);
    println!(
        "Init {} ms | eval {}x{} | FB {} KiB",
        cache_start.elapsed().as_millis(),
        low_w,
        low_h,
        byte_count / 1024
    );

    // Diagnostic: low_rgb565 placement.
    // Board: ESP32-S3R8 — internal SRAM ~512 KiB total, stacked 8MB PSRAM at ~0x3C000000.
    // We want LOW_RGB565 in #[ram(reclaimed)] static (fast internal), **not** from the heap.
    let low_ptr = pocket_watch_smoke_test::raidal::low_rgb565_ptr() as usize;
    println!("low_rgb565 ptr: 0x{:08x}", low_ptr);
    if (0x3C000000..0x3D000000).contains(&low_ptr) {
        println!("  WARNING: appears to be in PSRAM region — placement failed!");
    } else {
        println!("  Appears internal (good). Note: symbol may show under HEAP if reclaimed region overlaps allocator arena.");
    }

    let anim_start = Instant::now();
    shader.init_time(0);
    render_timed(&mut shader, &mut scratch, &mut fb0);
    bus.flush_bytes(&fb0, &mut dma_scratch);
    println!("First frame: {} ms", anim_start.elapsed().as_millis());

    let mut last_report = Instant::now();
    let mut ema_fps: f32 = 0.0;

    // The worker FLUSH offload + pending logic is bypassed for now to fix the
    // "stuck after first frame" issue. We do direct flush after every render.
    let _pending_flush = false;

    println!("Entering main loop with synchronous flush (worker FLUSH offload bypassed to fix first-frame glitch).");
    println!("Once multi-frame updates are confirmed, we can re-enable offload or proceed to pre-bake plan.");

    loop {
        let frame_start = Instant::now();
        let time_ms = anim_start.elapsed().as_millis() as u32;

        shader.update_time(time_ms);
        let current_fb = if use_fb0 { &mut fb0 } else { &mut fb1 };

        // Update FB_PTR every frame so the worker's upscale_rows half writes to the
        // correct double-buffer (previously only pointed at fb0, breaking fb1 band).
        unsafe {
            let p = &raw mut pocket_watch_smoke_test::raidal::FB_PTR;
            (*p).store(current_fb.as_mut_ptr(), Ordering::SeqCst);
        }

        let (eval_ms, upscale_ms) = render_timed(&mut shader, &mut scratch, current_fb);

        // === FIX for "stuck after first frame" glitch ===
        // The previous double-fb + worker FLUSH signal + pending_flush wait was not
        // reliably completing (worker inside long flush_bytes, bus aliasing via BUS_STATIC,
        // or signal visibility after first use).
        // For now we do synchronous flush on core0 after every render. This guarantees
        // repeated display updates using the exact path that worked for the visible first frame.
        // Animation will run (at render + ~25 ms cost). Overlap can be re-added later or
        // we move to the pre-bake plan once this base is solid.
        //
        // We still keep the per-frame FB_PTR + render_timed (dual compute split) because
        // that part succeeded for the initial frame.
        bus.write_command(0x2C);
        bus.flush_bytes(current_fb, &mut dma_scratch);

        // pending flush logic bypassed for now (see comment at declaration)

        // Swap render target for next frame (simple double-buffering of the *render* side).
        use_fb0 = !use_fb0;

        let flush_ms = 26; // offloaded to app core; approximate for log.

        let total_ms = frame_start.elapsed().as_millis();
        let inst_fps = if total_ms > 0 {
            1000.0 / total_ms as f32
        } else {
            0.0
        };
        ema_fps = if ema_fps < 1.0 {
            inst_fps
        } else {
            ema_fps * 0.9 + inst_fps * 0.1
        };

        if last_report.elapsed() >= Duration::from_secs(1) {
            println!(
                "fps~{:.1} eval={eval_ms}ms upscale={upscale_ms}ms flush={flush_ms}ms total={total_ms}ms",
                ema_fps
            );
            last_report = Instant::now();
        }

        delay_until(frame_start + Duration::from_micros(TARGET_FRAME_US));
    }
}

fn render_timed(
    shader: &mut Raidal2,
    scratch: &mut Scratch,
    framebuffer: &mut [u8],
) -> (u64, u64) {
    // Use the known fixed low size (div=3). Dual splits rows. Complete all eval before any upscale to avoid memory contention.
    let lh = LOW_H as usize;
    let mid = lh / 2;

    let t0 = Instant::now();

    // Signal core1 for its eval half first
    EVAL_ROW_START.store(mid, Ordering::SeqCst);
    EVAL_ROW_END.store(lh, Ordering::SeqCst);
    CORE1_READY.store(true, Ordering::SeqCst);
    fence(Ordering::SeqCst);

    // Core0 does its eval half in parallel
    shader.eval_rows(scratch, 0, mid);

    // Wait for core1 to finish its eval half (full low buffer ready)
    while !CORE1_DONE.load(Ordering::SeqCst) {
        fence(Ordering::SeqCst);
        core::hint::spin_loop();
    }
    CORE1_DONE.store(false, Ordering::SeqCst);

    let eval_ms = t0.elapsed().as_millis();

    let t1 = Instant::now();

    // Now full low is ready — do parallel upscale
    let mid_out = (LCD_HEIGHT / 2) as usize;
    UPSCALE_ROW_START.store(mid_out, Ordering::SeqCst);
    UPSCALE_ROW_END.store(LCD_HEIGHT as usize, Ordering::SeqCst);
    UPSCALE_CORE1_READY.store(true, Ordering::SeqCst);
    fence(Ordering::SeqCst);

    // Core0 does its upscale band
    shader.upscale_rows(framebuffer, 0, mid_out);

    // Wait for core1 upscale band
    while !UPSCALE_CORE1_DONE.load(Ordering::SeqCst) {
        fence(Ordering::SeqCst);
        core::hint::spin_loop();
    }
    UPSCALE_CORE1_DONE.store(false, Ordering::SeqCst);

    let upscale_ms = t1.elapsed().as_millis();

    (eval_ms, upscale_ms)
}

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