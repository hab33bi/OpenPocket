//! RTC-anchored wall clock (docs/ROADMAP.md M1).
//!
//! Boot: read the PCF85063; if its VL flag is set or it reads earlier than the
//! firmware's build timestamp, seed it with the build time (host local time
//! + a fixed flash fudge). Runtime: `now()` = RTC base + `Instant` elapsed,
//! re-anchored to the RTC hourly (the RTC crystal is truth; the CPU busy-wait
//! pacing drifts). All local time — no timezone machinery on device.

#[cfg(feature = "esp")]
use esp_hal::i2c::master::I2c;
#[cfg(feature = "esp")]
use esp_hal::time::Instant;
#[cfg(feature = "esp")]
use esp_hal::Blocking;
#[cfg(feature = "esp")]
use esp_println::println;

#[cfg(feature = "esp")]
use crate::drivers::pcf85063::{self, RtcTime};

include!(concat!(env!("OUT_DIR"), "/build_time.rs"));

/// Seconds added to the build timestamp when seeding — covers link + flash +
/// boot latency between `build.rs` running and the firmware's first boot.
const FLASH_FUDGE_SECS: u32 = 40;
/// Re-read the RTC this often to correct busy-wait pacing drift.
const RESYNC_SECS: u64 = 3_600;

/// Broken-down local wall time for rendering.
#[derive(Clone, Copy, PartialEq)]
pub struct WallTime {
    pub year: u16,
    pub month: u8, // 1..=12
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[cfg(feature = "esp")]
pub struct WallClock {
    /// Seconds since 2000-01-01 00:00 local at `anchor`.
    base_secs: u32,
    anchor: Instant,
}

#[cfg(feature = "esp")]
impl WallClock {
    /// Read (and seed if needed) the RTC. Logs the decision for the serial
    /// validation checklist: `RTC: VL=.. read=.. build=.. action=kept|seeded`.
    pub fn init(i2c: &mut I2c<'_, Blocking>) -> Self {
        let (rtc_secs, vl, read_ok) = match pcf85063::read(i2c) {
            Ok((t, vl)) => (rtc_to_secs(&t), vl, true),
            Err(()) => (0, true, false),
        };

        let base_secs = if vl || rtc_secs < BUILD_SECS_2000 {
            let target = BUILD_SECS_2000 + FLASH_FUDGE_SECS;
            let w = secs_to_civil(target);
            let t = RtcTime {
                year: w.year,
                month: w.month,
                day: w.day,
                hour: w.hour,
                minute: w.minute,
                second: w.second,
            };
            let weekday = weekday_from_days(target / 86_400);
            let seeded = pcf85063::set(i2c, &t, weekday).is_ok();
            println!(
                "RTC: VL={} read_ok={} read={} build={} action=seeded ok={}",
                vl as u8, read_ok as u8, rtc_secs, BUILD_SECS_2000, seeded as u8
            );
            target
        } else {
            println!(
                "RTC: VL=0 read_ok=1 read={} build={} action=kept",
                rtc_secs, BUILD_SECS_2000
            );
            rtc_secs
        };

        Self {
            base_secs,
            anchor: Instant::now(),
        }
    }

    pub fn now(&self) -> WallTime {
        let secs = self.base_secs + self.anchor.elapsed().as_secs() as u32;
        secs_to_civil(secs)
    }

    /// Call once per frame; re-anchors to the RTC hourly.
    pub fn maybe_resync(&mut self, i2c: &mut I2c<'_, Blocking>) {
        if self.anchor.elapsed().as_secs() < RESYNC_SECS {
            return;
        }
        if let Ok((t, vl)) = pcf85063::read(i2c) {
            if !vl {
                self.base_secs = rtc_to_secs(&t);
                self.anchor = Instant::now();
            }
        }
    }
}

/// RtcTime → seconds since 2000-01-01 00:00.
#[cfg(feature = "esp")]
fn rtc_to_secs(t: &RtcTime) -> u32 {
    let days = days_from_civil(t.year as i32, t.month as u32, t.day as u32);
    days as u32 * 86_400 + t.hour as u32 * 3_600 + t.minute as u32 * 60 + t.second as u32
}

/// Seconds since 2000-01-01 → broken-down local time.
fn secs_to_civil(secs: u32) -> WallTime {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    WallTime {
        year,
        month,
        day,
        hour: (rem / 3_600) as u8,
        minute: ((rem / 60) % 60) as u8,
        second: (rem % 60) as u8,
    }
}

/// 2000-01-01 was a Saturday. Returns 0=Sunday .. 6=Saturday.
fn weekday_from_days(days: u32) -> u8 {
    ((days + 6) % 7) as u8
}

/// Days since 2000-01-01 for a civil date (Howard Hinnant's algorithm,
/// rebased from the 1970 epoch by 10957 days). Integer-only.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 - 10_957
}

/// Inverse of `days_from_civil` for days since 2000-01-01.
fn civil_from_days(days_2000: u32) -> (u16, u8, u8) {
    let z = days_2000 as i64 + 10_957 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = if m <= 2 { y + 1 } else { y };
    (y as u16, m, d)
}
