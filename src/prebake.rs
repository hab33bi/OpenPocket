//! Pre-bake playback for high FPS seamless animation (no live math).
//!
//! Supports compressed delta+keyframe assets produced by the baker tool.
//! Falls back to small in-memory test frames when no asset is provided.

use alloc::vec;
use alloc::vec::Vec;

pub const FRAME_BYTES: usize = 466 * 466 * 2;
#[allow(dead_code)]
pub const TEST_FPS: u32 = 40; // kept for reference; playback now max-speed (see main.rs)

pub struct PrebakePlayer {
    // Asset mode (on-the-fly decode to save RAM for long seamless loops)
    asset: Option<&'static [u8]>,
    entries: Vec<FrameEntry>,
    payload_start: usize,

    // Test mode (pre-decoded, small)
    test_frames: Option<Vec<Vec<u8>>>,

    current: usize,

    // For delta decoding (last full frame)
    prev: Vec<u8>,

    // Reusable buffer to avoid allocating a full frame on every decode (helps FPS + no_std heap).
    decode_tmp: Vec<u8>,
}

#[derive(Clone, Copy)]
struct FrameEntry {
    offset: usize,
    len: usize,
    is_key: bool,
    is_rle: bool,
}

impl PrebakePlayer {
    /// Create with in-memory test frames (useful for bring-up).
    pub fn new_test() -> Self {
        let mut frames = Vec::new();
        for i in 0..8 {
            let mut fb = vec![0u8; FRAME_BYTES];
            let phase = i as u8;
            for y in 0..466 {
                for x in 0..466 {
                    let idx = (y * 466 + x) * 2;
                    let r = (phase * 3 + (x / 20) as u8) & 0x1F;
                    let g = (phase * 5 + (y / 20) as u8) & 0x3F;
                    let b = (phase * 7 + ((x + y) / 30) as u8) & 0x1F;
                    let px: u16 = ((r as u16) << 11) | ((g as u16) << 5) | b as u16;
                    fb[idx] = (px >> 8) as u8;
                    fb[idx + 1] = px as u8;
                }
            }
            let bar_x = (i * 50) % 466;
            for y in 0..466 {
                let idx = (y * 466 + bar_x) * 2;
                fb[idx] = 0xFF;
                fb[idx + 1] = 0xFF;
            }
            frames.push(fb);
        }
        Self {
            asset: None,
            entries: vec![],
            payload_start: 0,
            test_frames: Some(frames),
            current: 0,
            prev: vec![0u8; FRAME_BYTES],
            decode_tmp: vec![0u8; FRAME_BYTES],
        }
    }

    /// Create from a real pre-baked asset (produced by the baker).
    /// The asset must use the format written by the current baker (delta + keyframes).
    /// Decoding is on-the-fly to keep RAM low even for long seamless loops.
    pub fn from_asset(asset: &'static [u8]) -> Self {
        if asset.len() < 32 {
            return Self::new_test();
        }

        // Parse header (matches baker encode_asset)
        let frame_count = u32::from_le_bytes(asset[12..16].try_into().unwrap_or([0;4])) as usize;
        let format = asset[16];
        // payload_offset value (absolute offset of payload after header+table) is stored at bytes 20..24
        let payload_offset = u32::from_le_bytes(asset[20..24].try_into().unwrap_or([0;4])) as usize;

        let mut entries = Vec::with_capacity(frame_count);

        if format == 1 {
            let table_start = 28;
            let entry_size = 10;

            for i in 0..frame_count {
                let entry_off = table_start + i * entry_size;
                if entry_off + 10 > asset.len() { break; }

                let off = u32::from_le_bytes(asset[entry_off..entry_off+4].try_into().unwrap_or([0;4])) as usize;
                let len = u32::from_le_bytes(asset[entry_off+4..entry_off+8].try_into().unwrap_or([0;4])) as usize;
                let is_key = asset[entry_off+8] != 0;
                let is_rle = asset[entry_off+9] != 0;

                entries.push(FrameEntry { offset: off, len, is_key, is_rle });
            }
        } else {
            // Fallback raw: treat each full frame as a keyframe entry
            let mut off = payload_offset;
            for _ in 0..frame_count {
                if off + FRAME_BYTES > asset.len() { break; }
                entries.push(FrameEntry { offset: off - payload_offset, len: FRAME_BYTES, is_key: true, is_rle: false });
                off += FRAME_BYTES;
            }
        }

        if entries.is_empty() {
            return Self::new_test();
        }

        Self {
            asset: Some(asset),
            entries,
            payload_start: payload_offset,
            test_frames: None,
            current: 0,
            prev: vec![0u8; FRAME_BYTES],
            decode_tmp: vec![0u8; FRAME_BYTES],
        }
    }

    pub fn next_frame(&mut self, out: &mut [u8]) {
        if let Some(test) = &self.test_frames {
            let f = &test[self.current];
            out.copy_from_slice(f);
            self.current = (self.current + 1) % test.len();
            return;
        }

        let asset = self.asset.expect("asset mode");
        let entry = &self.entries[self.current];
        let data_start = self.payload_start + entry.offset;
        let stored = &asset[data_start..data_start + entry.len];

        // Decode rle (or raw copy) into reusable tmp.
        if entry.is_rle {
            zero_rle_decode_into(stored, &mut self.decode_tmp);
        } else {
            self.decode_tmp.clear();
            self.decode_tmp.extend_from_slice(stored);
        }

        let n = core::cmp::min(self.decode_tmp.len(), out.len());

        if entry.is_key {
            // Direct: undelta rle result straight into the output fb. No extra full-frame alloc.
            spatial_undelta_to(&self.decode_tmp[..n], &mut out[..n]);
        } else {
            // For delta: undelta into the *out* buffer temporarily (as expanded), then xor with prev in place.
            spatial_undelta_to(&self.decode_tmp[..n], &mut out[..n]);
            for j in 0..n {
                out[j] = self.prev[j] ^ out[j];
            }
        }

        // Keep last full frame for next delta
        self.prev.copy_from_slice(&out[..FRAME_BYTES]);

        self.current = (self.current + 1) % self.entries.len();
    }

    pub fn frame_count(&self) -> usize {
        if let Some(test) = &self.test_frames {
            test.len()
        } else {
            self.entries.len()
        }
    }
}

/// Decode zero-run RLE produced by baker into a reusable buffer.
/// Matches zero_rle_encode: non-zero literal; [0, u16 count] expands to that many zeros.
/// Clears `out` first.
fn zero_rle_decode_into(data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0 {
            // zero run: need 2 more bytes for count
            if i + 2 >= data.len() {
                break;
            }
            let count = u16::from_le_bytes([data[i + 1], data[i + 2]]);
            // extend with many zeros efficiently
            let start = out.len();
            out.resize(start + count as usize, 0);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
}

/// Reverse of spatial_delta in baker: reconstruct from previous-byte deltas directly into dst.
fn spatial_undelta_to(data: &[u8], dst: &mut [u8]) {
    if data.is_empty() || dst.is_empty() {
        return;
    }
    let n = core::cmp::min(data.len(), dst.len());
    dst[0] = data[0];
    for i in 1..n {
        dst[i] = dst[i - 1].wrapping_add(data[i]);
    }
}