# Raidal-2: WebGL GLSL ↔ Rust Implementation Mapping

> **Purpose:** Line-by-line fidelity contract between the [21st.dev Raidal-2](https://21st.dev) WebGL shader and `src/raidal.rs`. Any optimization must preserve these relationships unless the user explicitly accepts a visual change.  
> **Doc index:** [`README.md`](README.md) · **Entry prompt:** [`03-FOLLOWUP-PROMPT.md`](03-FOLLOWUP-PROMPT.md)

---

## 1. Original GLSL (canonical)

```glsl
// Fragment shader core (Raidal-2 style, simplified notation)
// FC = gl_FragCoord, r = resolution (width, height)

vec2 p = FC.xy - r * 0.5;

vec4 o = vec4(0.0);

for (float i = 0.0; i < 9.0; i++) {
    float layer = i + 1.0;  // effectively layers 1..9 depending on loop semantics

    float a_field = (layer * layer) / 80.0 - length(p) / r.y;

    float denom = max(a_field, -a_field * 3.0) + 2.0 / r.y;

    float edge0 = cos(layer - t);  // t = time in seconds

    float angle = atan(p.y, p.x) + cos(layer - t) + layer * layer;

    float sm = smoothstep(edge0, 2.0, cos(angle));

    o += (0.03 / denom) * sm * (1.2 + sin(angle + layer + vec4(0.0, 2.0, 4.0, 0.0)));
}

o = tanh(o);
// Output: o.rgb → display
```

**Note on loop variable:** The ported Rust uses explicit layers `1..=9` with `layer` as `1.0` through `9.0`. The WebGL `for (float i; i++ < 9.0)` idiom increments `i` from 0; the original 21st.dev port in this repo uses `for layer in 1..=LAYERS` matching `i+1` as layer index. **Do not change layer indexing without visual A/B test.**

---

## 2. Coordinate system

| WebGL | Rust (`Raidal2::new` init) | Notes |
|-------|---------------------------|-------|
| `FC.xy` | Fragment center per low-res cell | `(lx * step + step/2, ly * step + step/2)` |
| `p = FC - r*0.5` | `px = frag_x - res_x*0.5`, `py = frag_y - res_y*0.5` | Centered origin |
| `length(p) / r.y` | `plen_norm = sqrt(px²+py²) * inv_ry` | `inv_ry = 1/466` |
| `atan(p.y, p.x)` | `atan2f(py, px)` → cached as Q14 | **Static** — no per-frame cost |
| Full-res FC | Upscale pass maps 466×466 → low grid | div=N samples centered per cell |

### RENDER_DIVISOR sampling

For output pixel `o` at position `(ox, oy)` on 466×466 display:

```
s_x = ((ox + 0.5) / div) - 0.5    // clamped to [0, low_w-1]
```

Eval grid uses **cell centers** matching bilinear upscale weights in `build_upmap()`.

| div | low_w × low_h | Eval pixels | Quality |
|-----|---------------|-------------|---------|
| 1 | 466×466 | 217,156 | Perfect (too slow) |
| 2 | 233×233 | 54,289 | Near WebGL on 466px panel |
| 3 | 156×156 | 24,336 | **Current** — slight softness |
| 4 | 117×117 | 13,689 | User rejected — blocky |

---

## 3. Per-layer static terms (cached at init)

### 3.1 `a_field` and `denom`

```rust
// Init only (float):
let l = (layer + 1) as f32;
let a = l * l / 80.0 - plen_norm;
let denom = maxf(a, -a * 3.0) + 2.0 * inv_ry;
inv_denom = 0.03 / denom;  // stored as Q20 in row_packed
```

| GLSL | Rust cache field | Frame cost |
|------|------------------|------------|
| `(i*i)/80 - len(p)/r.y` | folded into `denom` via `a` | **0** (init) |
| `max(a, -3a) + 2/r.y` | `denom` | **0** (init) |
| `0.03/denom` | `row_packed[lw + layer*lw + lx]` Q20 | **0** per frame |

### 3.2 `atan(p)`

| GLSL | Rust | Frame cost |
|------|------|------------|
| `atan(p.y, p.x)` | `row_packed[lx]` Q14 radians | **0** per frame |

---

## 4. Per-frame time-varying terms

Only **9** trig calls per frame (not per pixel):

```rust
// update_frame_cos(time_ms):
let t = (time_ms / 1000.0) * time_scale;
for layer in 0..9 {
    frame_cos_q14[layer] = lut_cos_angle_q14((layer + 1) as f32 - t);
}
```

| GLSL | Rust | Count/frame |
|------|------|-------------|
| `cos(i - t)` for edge0 | `frame_cos_q14[layer]` | **9** |

---

## 5. Per-pixel per-layer dynamic terms (hot loop)

From `eval_pixel_q14()`:

```rust
let edge0 = frame_cos[layer];                    // cos(layer - t)
let a = atan_p + edge0 + LAYER_LL_Q14[layer];    // atan(p) + cos(layer-t) + layer²
let cos_a = lut_sin_cos_q14(a + FRAC_PI_2_Q14);  // cos(angle)
let sm = smoothstep_q14(edge0, Q * 2, cos_a);    // smoothstep(edge0, 2.0, cos(angle))
let factor = (inv_q20 * sm as i64) >> 20;        // (0.03/denom) * sm

let s0 = ONE2_Q14 + lut_sin_cos_q14(a + li);     // 1.2 + sin(angle + layer)
let s1 = ONE2_Q14 + lut_sin_cos_q14(a + li + Q*2);
let s2 = ONE2_Q14 + lut_sin_cos_q14(a + li + Q*4);
o_r += factor * s0;
// ... g, b similarly
```

### Mapping table

| GLSL expression | Rust equivalent | LUT calls / layer / pixel |
|-----------------|-----------------|---------------------------|
| `edge0 = cos(layer-t)` | `frame_cos[layer]` | 0 (precomputed) |
| `angle = atan + cos(layer-t) + layer²` | `a = atan_p + edge0 + LAYER_LL` | 0 |
| `cos(angle)` | `lut_sin_cos_q14(a + PI/2)` | **1** |
| `smoothstep(edge0, 2.0, cos(angle))` | `smoothstep_q14(edge0, 2*Q, cos_a)` | 0 |
| `sin(angle+layer+0)` | `lut_sin_cos_q14(a + li)` | **1** |
| `sin(angle+layer+2)` | `lut_sin_cos_q14(a + li + 2*Q)` | **1** |
| `sin(angle+layer+4)` | `lut_sin_cos_q14(a + li + 4*Q)` | **1** |
| `0.03/denom * sm * (1.2+sin...)` | `factor * (ONE2_Q14 + sin...)` | 0 |

**Total LUT calls: 4 per layer × 9 layers = 36 per pixel per frame.**

---

## 6. Tone mapping and output

| GLSL | Rust | Notes |
|------|------|-------|
| `tanh(o.r)` | `fast_tanh_q14((o_r >> 14) as i32)` | Per channel |
| `tanh(o.g)` | same on `o_g` | |
| `tanh(o.b)` | same on `o_b` | |
| float RGB [0,1] | `q14_rgb_to_rgb565` | 5/6/5 quantization |
| — | `upscale_pass` → BE bytes | Full 466×466 |

**Do not collapse to single-channel tanh** — breaks teal/magenta/gold separation.

---

## 7. `smoothstep` implementation

GLSL:
```glsl
smoothstep(edge0, edge1, x) = clamp((x-edge0)/(edge1-edge0), 0, 1)
hermite: t*t*(3-2*t)
```

Rust Q14 (`smoothstep_q14`):
- `edge0` = `frame_cos` (cos value, not angle)
- `edge1` = `2.0` → `Q * 2` = 32768 in Q14
- `x` = `cos_a` (cos of angle)

**Critical:** `edge0` and `x` are both **cosine values** in approximately [-1, 1], while `angle` mixes radians + dimensionless terms. This matches the original WebGL port's mixed-unit behavior — do not "fix" the units without visual validation.

---

## 8. `fast_tanh` rational approximation

```rust
// Matches float version in earlier raidal.rs:
// x * (27 + x²) / (27 + 9x²), clamped at ±3.5
```

Used in Q14 with `lim = 3.5 * Q`.

---

## 9. Upscale — second pass (not in GLSL)

WebGL renders at full resolution. We render at `low_w × low_h` then bilinear upscale:

```rust
struct UpPixel {
    idx: [u16; 4],  // indices into low_rgb565[]
    w: [u8; 4],     // bilinear weights, sum ≈ 255
}
```

`bilinear_rgb565` interpolates **per channel** in 5/6/5 bit space — standard GPU-style approximation.

---

## 10. Fixed-point format reference

| Quantity | Format | Storage |
|----------|--------|---------|
| Angles in LUT | Q14 | `phase` i32, `SIN_LUT_I16` |
| `atan_p` | Q14 | `row_packed[lx]` |
| `inv_denom` | Q20 | `row_packed[lw + layer*lw + lx]` |
| `frame_cos` | Q14 | `frame_cos_q14[9]` |
| `1.2` | Q14 | `ONE2_Q14 = 19661` |
| `layer²` | Q14 | `LAYER_LL_Q14[layer]` |
| `layer` index | Q14 | `LAYER_IDX_Q14[layer]` |
| Accumulators | i64 | `o_r`, `o_g`, `o_b` during layer sum |

---

## 11. Safe optimization boundaries

### Safe (qual-neutral if done correctly)

- Move buffers to internal SRAM
- Dual-core eval on disjoint pixel ranges
- Replace i64 accumulators with i32 + careful scaling
- Reduce LUT calls via algebraic reuse of `cos_a` / `sin(a+li)` if mathematically identical
- Pipeline eval with flush
- div=2 if eval fast enough

### Unsafe (changes appearance)

- Fewer than 9 layers
- Replace `smoothstep` with step/hard threshold
- Single `tanh` on luminance only
- Remove per-channel +0/+2/+4 phase offsets
- div≥4 without user sign-off
- Replace `atan2` with approximation that shifts band positions

---

## 12. Reference: original float `eval_pixel` (Gen 1–2)

For regression comparison, the float hot loop was:

```rust
let factor = inv_denom[layer] * sm;  // inv = 0.03/denom precomputed
o_r += factor * (1.2 + lut_sin(a + l));
o_g += factor * (1.2 + lut_sin(a + l + 2.0));
o_b += factor * (1.2 + lut_sin(a + l + 4.0));
// then fast_tanh per channel, rgb888_to_rgb565
```

Q14 path must match this within ~1–2 RGB565 levels per channel.

---

*Appendix document — Turn 3 of 3 — see [`README.md`](README.md)*