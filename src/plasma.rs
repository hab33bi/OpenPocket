//! Velvet Aurora — slow domain-warped plasma background for premium AMOLED watch faces.
//!
//! Design goals:
//! - Deep blue → rich violet palette (luxury AOD brightness)
//! - Domain warp (option D): base plasma field sampled through a slowly moving
//!   coordinate distortion for organic, non-repeating flow
//! - Burn-in mitigation: incommensurate phase speeds, slow rotation, no static
//!   frames, capped peak luminance, radial vignette on the round panel
//!
//! Future: move `render_frame` output buffer to PSRAM + flush via DMA `SpiDmaBus`.

use libm::{cosf, sinf};

const TAU: f32 = core::f32::consts::TAU;

/// Tunable parameters for the Velvet Aurora effect.
#[derive(Clone, Copy, Debug)]
pub struct VelvetAuroraConfig {
    /// Global multiplier for all phase animation speeds.
    pub phase_speed: f32,
    /// Seconds for one full subtle coordinate rotation (burn-in spread).
    pub rotation_period_s: f32,
    /// Magenta whisper strength on palette crests only (0 = none).
    pub magenta_whisper: f32,
    /// Maximum RGB component after palette (luxury AOD — keep low).
    pub luminance_cap: f32,
    /// Domain-warp displacement scale in normalized coordinates.
    pub warp_strength: f32,
    /// Radial vignette: full brightness inside this radius.
    pub vignette_inner: f32,
    /// Radial vignette: fade to black by this radius.
    pub vignette_outer: f32,
}

impl Default for VelvetAuroraConfig {
    fn default() -> Self {
        Self {
            phase_speed: 1.0,
            rotation_period_s: 1_200.0, // 20 minutes per revolution
            magenta_whisper: 0.08,
            luminance_cap: 0.38,
            warp_strength: 0.11,
            vignette_inner: 0.92,
            vignette_outer: 1.0,
        }
    }
}

/// Plasma renderer holding configuration.
pub struct VelvetAurora {
    config: VelvetAuroraConfig,
}

impl VelvetAurora {
    pub fn new(config: VelvetAuroraConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &VelvetAuroraConfig {
        &self.config
    }

    /// Fill `buf` with RGB565 pixels (row-major, `buf.len() == width * height`).
    ///
    /// `time_ms` must advance every frame so no two frames are identical
    /// (burn-in mitigation).
    pub fn render_frame(&self, buf: &mut [u16], width: u16, height: u16, time_ms: u32) {
        let cfg = self.config;
        let t = (time_ms as f32 / 1000.0) * cfg.phase_speed;
        let cx = (width - 1) as f32 * 0.5;
        let cy = (height - 1) as f32 * 0.5;
        let inv_half = 1.0 / cx.min(cy);

        // Slow global rotation — spreads static stress around the circular panel.
        let theta = TAU * t / cfg.rotation_period_s;
        let (rot_c, rot_s) = (cosf(theta), sinf(theta));

        // Incommensurate phase speeds (seconds per 2π) so the field rarely repeats.
        let p1 = TAU * t / 47.0;
        let p2 = TAU * t / 61.0;
        let p3 = TAU * t / 53.0;
        let pw1 = TAU * t / 71.0;
        let pw2 = TAU * t / 83.0;
        let pw3 = TAU * t / 67.0;
        let pw4 = TAU * t / 91.0;

        for y in 0..height {
            let fy = y as f32 - cy;
            for x in 0..width {
                let fx = x as f32 - cx;

                // Normalized coords in [-1, 1]
                let mut nx = fx * inv_half;
                let mut ny = fy * inv_half;

                // Subtle rotation before warp + plasma eval.
                let rx = nx * rot_c - ny * rot_s;
                let ry = nx * rot_s + ny * rot_c;
                nx = rx;
                ny = ry;

                // --- Domain warp (option D) ---
                // A secondary low-frequency vector field offsets sample coordinates,
                // creating smooth flowing "aurora curtains" without harsh plasma edges.
                let warp = cfg.warp_strength;
                let wx = warp
                    * sinf(2.4 * ny + pw1)
                    * cosf(1.9 * nx + pw2)
                    * sinf(0.7 * (nx + ny) + pw3);
                let wy = warp
                    * cosf(2.1 * nx - pw2)
                    * sinf(1.7 * ny + pw4)
                    * cosf(0.6 * (ny - nx) + pw1);

                let sx = nx + wx;
                let sy = ny + wy;
                let r = libm::sqrtf(sx * sx + sy * sy);

                // --- Three-layer plasma on warped coords ---
                let l1 = sinf(1.05 * sx + 0.38 * sy + p1);
                let l2 = sinf(0.72 * sy - 0.48 * sx + p2);
                let l3 = sinf(0.88 * r + 0.35 * sx * sy + p3);

                // Weighted sum → [0, 1]
                let raw = 0.44 * l1 + 0.34 * l2 + 0.22 * l3;
                let tone = (raw * 0.5 + 0.5).clamp(0.0, 1.0);

                // --- Cosine palette (Inigo Quilez) tuned for deep blue → violet ---
                let (mut r8, mut g8, mut b8) = velvet_palette(tone);

                // Whisper of magenta only on the brightest crests (dark purple overall).
                let accent = smoothstep(0.80, 0.96, tone) * cfg.magenta_whisper;
                r8 += accent * 0.14;
                g8 += accent * 0.02;
                b8 += accent * 0.06;

                // Luxury AOD luminance cap.
                let peak = r8.max(g8).max(b8);
                if peak > cfg.luminance_cap {
                    let scale = cfg.luminance_cap / peak;
                    r8 *= scale;
                    g8 *= scale;
                    b8 *= scale;
                }

                // Radial vignette for the round 466 mm panel (corners fade to black).
                let dist = libm::sqrtf(nx * nx + ny * ny);
                let vignette = 1.0 - smoothstep(cfg.vignette_inner, cfg.vignette_outer, dist);
                r8 *= vignette;
                g8 *= vignette;
                b8 *= vignette;

                buf[y as usize * width as usize + x as usize] = rgb888_to_rgb565(r8, g8, b8);
            }
        }
    }
}

/// Cosine palette: `a + b * cos(2π(c*t + d))` per channel.
/// Coefficients chosen for deep blue floor, violet body, no neon saturation.
fn velvet_palette(t: f32) -> (f32, f32, f32) {
    //                R      G      B
    let a = (0.05_f32, 0.03, 0.12);
    let b = (0.04, 0.025, 0.09);
    let c = (1.0, 0.72, 1.08);
    let d = (0.0, 0.14, 0.36);

    let r = a.0 + b.0 * cosf(TAU * (c.0 * t + d.0));
    let g = a.1 + b.1 * cosf(TAU * (c.1 * t + d.1));
    let b_ch = a.2 + b.2 * cosf(TAU * (c.2 * t + d.2));
    (r, g, b_ch)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 >= edge1 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn rgb888_to_rgb565(r: f32, g: f32, b: f32) -> u16 {
    let r5 = (r.clamp(0.0, 1.0) * 31.0) as u16;
    let g6 = (g.clamp(0.0, 1.0) * 63.0) as u16;
    let b5 = (b.clamp(0.0, 1.0) * 31.0) as u16;
    (r5 << 11) | (g6 << 5) | b5
}