//! Baker tool for pre-baking Raidal-2 animation frames (host std tool).
//!
//! IMPORTANT: This is a **host** tool. It must be built for your PC, not the ESP32 target.
//! The rust-toolchain.toml forces the 'esp' toolchain, so use `+stable` to get a host-capable rustc.
//!
//! Recommended build:
//!   cargo +stable build --bin baker --features baker --no-default-features --target x86_64-pc-windows-msvc --config 'build.rustflags=[]'
//!
//! Then run:
//!   target\x86_64-pc-windows-msvc\debug\baker.exe 2.5
//!
//! For the actual device firmware (prebake player):
//!   cargo build --bin pocket-watch-smoke-test --features prebake --release
//!
//! This is the host-side tool to pre-bake the shader mathematics into frames.

use std::env;
use std::fs;
use std::path::Path;

use pocket_watch_smoke_test::raidal::{Raidal2, Raidal2Config, Scratch, LOW_W};

const W: u16 = 466;
const H: u16 = 466;
const FRAME_SIZE: usize = (W as usize) * (H as usize) * 2;
const TARGET_FPS: u32 = 30;

fn main() {
    let args: std::vec::Vec<String> = env::args().collect();
    // Bake long enough to search for best 60-frame seam segment.
    // We force t modulo 2.0s in the sampling so the time-dependent part of the shader repeats every 60 frames exactly.
    // This gives a true seamless loop (wrap transition is just a normal frame step) while keeping original motion speed (good compression).
    let seconds: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let total_frames = (seconds * TARGET_FPS as f32) as usize;

    const TARGET_LOOP_FRAMES: usize = 60;
    const LOOP_PERIOD_S: f32 = TARGET_LOOP_FRAMES as f32 / TARGET_FPS as f32;  // 2.0 s

    println!("Baking real Raidal-2 frames for {} seconds @ {} FPS ({} frames) for a {} frame ({:.1}s) loop", seconds, TARGET_FPS, total_frames, TARGET_LOOP_FRAMES, LOOP_PERIOD_S);
    println!("t is modulo {:.1}s in generation for exact periodicity (seamless by construction).", LOOP_PERIOD_S);

    let mut shader = Raidal2::new(
        Raidal2Config {
            render_divisor: 3,
            time_scale: 1.0,
        },
        W,
        H,
    );

    let mut scratch = Scratch::new(LOW_W);

    let mut frames: std::vec::Vec<std::vec::Vec<u8>> = std::vec::Vec::with_capacity(total_frames);

    for i in 0..total_frames {
        let t_s = (i as f32 / TARGET_FPS as f32);
        let t_mod = t_s % LOOP_PERIOD_S;
        let t_ms = (t_mod * 1000.0) as u32;

        shader.update_time(t_ms);

        let mut fb = vec![0u8; FRAME_SIZE];
        shader.eval_pass(&mut scratch);
        shader.upscale_pass(&mut fb);

        frames.push(fb);

        if i % 10 == 0 {
            println!("  generated frame {}/{}", i, total_frames);
        }
    }

    // For 60-frame target: find the *best seam segment* of exactly TARGET_LOOP_FRAMES.
    // Search all possible offsets in the long capture for the one with lowest diff(last, first).
    // Combined with the time_scale tweak above, this should give near-perfect seamless (normal inter-frame motion at wrap).
    let (best_period, best_offset) = find_best_seam_offset_for_p(&frames, TARGET_LOOP_FRAMES);
    println!(
        "Recommended seamless loop: {:.2}s ({} frames) starting at frame {}",
        best_period as f32 / TARGET_FPS as f32,
        best_period,
        best_offset
    );

    let mut loop_frames = extract_loop(&frames, best_offset, best_period);
    // Use the full seamless loop detected by the baker.
    // With delta + RLE it should compress well enough for the 16MB flash.

    // Report actual seam quality for the extracted loop: diff between last frame and first.
    // This is the jump the player will make on wrap. Low == smoother.
    if !loop_frames.is_empty() {
        let last = &loop_frames[loop_frames.len() - 1];
        let first = &loop_frames[0];
        let seam_d = frame_diff(last, first);
        println!("  extracted loop seam diff (last -> first): {}", seam_d);
    }

    // Encode asset
    let asset = encode_asset(&loop_frames, TARGET_FPS);
    let out_path = Path::new("assets/raidal_loop.bin");
    fs::create_dir_all(out_path.parent().unwrap()).ok();
    fs::write(out_path, &asset).expect("write asset");
    println!(
        "Wrote {} ({} frames, {} bytes on disk, {} bytes if raw)",
        out_path.display(),
        loop_frames.len(),
        asset.len(),
        loop_frames.len() * FRAME_SIZE
    );

}

/// Simple diff for seamless detection (sum of squared diffs).
fn frame_diff(a: &[u8], b: &[u8]) -> u64 {
    use std::iter::Iterator; // bring zip into scope for host build
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = (*x as i32 - *y as i32).abs() as u64;
            d * d
        })
        .sum()
}

fn find_best_seam_offset_for_p(frames: &[std::vec::Vec<u8>], p: usize) -> (usize, usize) {
    if p == 0 || frames.len() < p {
        return (p, 0);
    }
    let max_off = frames.len() - p;

    let mut best_off = 0usize;
    let mut best_seam = u64::MAX;

    // Full search over offsets for the best (lowest) wrap seam for this fixed p.
    for off in 0..=max_off {
        let last_idx = off + p - 1;
        let seam = frame_diff(&frames[last_idx], &frames[off]);
        if seam < best_seam {
            best_seam = seam;
            best_off = off;
        }
    }

    // Reference natural step (what periodicity gives us)
    let natural_seam = if best_off + p < frames.len() {
        frame_diff(&frames[best_off + p - 1], &frames[best_off + p])
    } else { 0 };

    println!("  best seam for p={}: off={} seam={} (natural inter-frame ~{})", p, best_off, best_seam, natural_seam);
    println!("  (With the time_scale periodicity tweak, this seam is just a normal motion step -- perfectly seamless loop.)");

    (p, best_off)
}

fn extract_loop(frames: &[std::vec::Vec<u8>], offset: usize, period: usize) -> std::vec::Vec<std::vec::Vec<u8>> {
    let end = offset + period;
    if end <= frames.len() {
        frames[offset..end].to_vec()
    } else {
        frames.to_vec()
    }
}

/// Delta + keyframe asset encoder (format 1).
/// Stores full keyframes periodically + XOR deltas between them.
/// This gives excellent compression on slowly-changing content like aurora.
fn encode_asset(frames: &[std::vec::Vec<u8>], fps: u32) -> std::vec::Vec<u8> {
    let mut payload = std::vec::Vec::new();
    let mut table: Vec<(u32, u32, u8, u8)> = Vec::new(); // (offset, len, is_key, is_rle)

    const KEYFRAME_INTERVAL: usize = 15; // higher interval -> fewer large keyframes, deltas still exact via XOR; good for size on slow-changing content

    let mut prev: Option<Vec<u8>> = None;

    for (i, frame) in frames.iter().enumerate() {
        let is_key = (i % KEYFRAME_INTERVAL) == 0 || prev.is_none();
        let data: Vec<u8> = if is_key {
            frame.clone()
        } else {
            // XOR delta from previous frame - very cheap and effective
            let p = prev.as_ref().unwrap();
            frame.iter().zip(p.iter()).map(|(a, b)| a ^ b).collect()
        };

        // Spatial delta (prev byte) + zero-run RLE.
        // Spatial makes neighboring similar pixels (common in aurora) into small diffs, creating more zero runs.
        // Then zero RLE exploits exact 0s.
        let spatial = spatial_delta(&data);
        let compressed = zero_rle_encode(&spatial);

        let offset = payload.len() as u32;
        let len = compressed.len() as u32;
        payload.extend_from_slice(&compressed);
        // is_rle=1 means zero-run compressed (format 1)
        table.push((offset, len, if is_key { 1 } else { 0 }, 1));

        prev = Some(frame.clone());
    }

    let mut header = std::vec::Vec::new();
    // Fixed 28-byte header
    header.extend_from_slice(b"RAID");
    header.extend_from_slice(&1u16.to_le_bytes()); // version
    header.extend_from_slice(&W.to_le_bytes());
    header.extend_from_slice(&H.to_le_bytes());
    header.extend_from_slice(&(fps as u16).to_le_bytes());
    header.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    header.extend_from_slice(&1u8.to_le_bytes()); // format 1 = delta+key
    header.extend_from_slice(&(KEYFRAME_INTERVAL as u8).to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes()); // flags

    // Placeholder for payload_offset (will patch after table) + crc=0
    let po_pos = header.len(); // 20
    header.extend_from_slice(&0u32.to_le_bytes()); // payload_offset placeholder
    header.extend_from_slice(&0u32.to_le_bytes()); // crc placeholder (0)
    // now header.len() == 28

    // Append table AFTER the fixed header prefix
    for (off, len, key, rle) in &table {
        header.extend_from_slice(&off.to_le_bytes());
        header.extend_from_slice(&len.to_le_bytes());
        header.push(*key);
        header.push(*rle);
    }

    let actual_payload_offset = header.len() as u32; // 28 + table*10
    // patch placeholder
    header[po_pos..po_pos + 4].copy_from_slice(&actual_payload_offset.to_le_bytes());

    header.extend_from_slice(&payload);
    header
}

/// Zero-run RLE encoder for delta/keyframes.
/// Non-zero bytes: stored as-is (1 byte).
/// Zero runs (1 or more): stored as 0u8 + u16_le count (3 bytes).
/// Decoder mirrors exactly. Ideal + fast for this use case (aurora deltas have large static regions).
fn zero_rle_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0 {
            out.push(data[i]);
            i += 1;
            continue;
        }
        // zero run
        let mut count: u16 = 0;
        while i < data.len() && data[i] == 0 && count < u16::MAX {
            count += 1;
            i += 1;
        }
        out.push(0);
        out.extend_from_slice(&count.to_le_bytes());
    }
    out
}

/// Byte-wise previous-pixel delta (like PNG "Sub" filter). Turns smooth gradients into small numbers (more zeros after).
fn spatial_delta(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![];
    }
    let mut out = Vec::with_capacity(data.len());
    out.push(data[0]);
    for i in 1..data.len() {
        out.push(data[i].wrapping_sub(data[i - 1]));
    }
    out
}