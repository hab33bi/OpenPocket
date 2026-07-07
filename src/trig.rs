//! Q14 fixed-point trigonometry via a 512-entry sine LUT (generated in build.rs).
//!
//! Extracted from the retired Raidal shader module — the only trig the product
//! needs: `sin(phase)` in Q14 with linear interpolation between LUT entries.
//! `cos(x)` = `lut_sin_cos_q14(x + FRAC_PI_2_Q14)`.

include!(concat!(env!("OUT_DIR"), "/sin_lut.rs"));

/// One full turn (2π) in Q14.
pub const TAU_Q14: i32 = 103_246;
/// +90° (π/2) in Q14.
pub const FRAC_PI_2_Q14: i32 = 25_736;

const LUT_SHIFT: i32 = 9;

/// sin(phase) for a Q14 phase (radians × 16384), returned in Q14.
#[inline(always)]
pub fn lut_sin_cos_q14(phase: i32) -> i32 {
    let mut x = phase % TAU_Q14;
    if x < 0 {
        x += TAU_Q14;
    }
    let idx_f = ((x as i64) << LUT_SHIFT) / TAU_Q14 as i64;
    let i0 = (idx_f as usize) & 511;
    let frac = (idx_f - i0 as i64) as i32;
    let i1 = (i0 + 1) & 511;
    let v0 = SIN_LUT_I16[i0] as i32;
    let v1 = SIN_LUT_I16[i1] as i32;
    v0 + ((v1 - v0) * frac >> LUT_SHIFT)
}
