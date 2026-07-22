# Water — integration diffs (minimal, exact)

Wires [`water_draft.rs`](water_draft.rs) (→ `src/scenes/water.rs`) into the
scene machine. Six touch points; all line neighbourhoods are against the repo
at this commit. `apps` stays I2C-free — the run loop (which owns `self.i2c`)
reads the accelerometer and passes the raw triple in (the plan's Option 1).

---

## 0. Register the module — `src/scenes.rs`

```diff
 pub mod apps;
 pub mod lock;
+pub mod water;
 pub mod wheel;
```

Copy `docs/water/water_draft.rs` → `src/scenes/water.rs`.

---

## 1. `apps::State` — new field + init  (`src/scenes/apps.rs`)

**Import** (near the top, by `use crate::scenes::lock…` ~line 12):

```diff
 use crate::display::watch_fb::WatchFb;
 use crate::scenes::lock::{self, Glyph};
+use crate::scenes::water::Water;
 use crate::scenes::wheel;
```

**Field** in `pub struct State { … }` (ends ~line 63, after `ph_drift`):

```diff
     /// Phone dial drift offset last drawn (pseudo units).
     pub ph_drift: i32,
+    /// Water liquid simulation (particles/hash/calibration — internal SRAM).
+    pub wa: Water,
 }
```

**Init** in `const fn new()` (ends ~line 81, after `ph_drift: 0,`):

```diff
         mu_theta: -1,
         we_arc: -1,
         ph_drift: 0,
+        wa: Water::new(),
     }
```

> `Water::new()` is `const fn`, so `State::new()` stays `const`. The +8.9 KB
> lives on the `run()` stack frame; if first-flash head-room is tight, promote
> `Water` to a module `static` (methods already take `&mut self`) — no logic
> change. (See IMPL-SPEC §3.)

---

## 2. `has_content` — Water now has content  (`src/scenes/apps.rs` ~line 123)

```diff
 pub fn has_content(idx: usize) -> bool {
-    // Every app has real content behind its splash EXCEPT Water, which
-    // rests on its splash (waves logo) until the liquid sim lands
-    // (docs/WATER-APP-PLAN.md — an ultracode pass).
-    idx != WATER
+    // Every app has real content behind its splash. Water's is the liquid sim.
+    let _ = idx;
+    true
 }
```

---

## 3. `apps.rs` — the `water_tick` entry + reveal branch

**Public entry** (add beside `pub fn tick(…)` ~line 297; keep the generic
`tick`'s `_ => {}` Water arm — Water is driven from the run-loop call site, not
the shared tick, because it needs the IMU):

```rust
/// Water's per-frame entry: the run loop reads the IMU (it owns `self.i2c`)
/// and hands the raw triple in; `imu = None` on a bus fault. Keeps `apps`
/// I2C-free. (docs/water/IMPL-SPEC.md §5–7.)
pub fn water_tick(
    wfb: &mut WatchFb,
    imu: Option<(i16, i16, i16)>,
    elapsed_ms: u32,
    st: &mut State,
) {
    st.wa.tick(wfb, imu, elapsed_ms);
}
```

**Reveal branch** — fill-in on open. In `draw_reveal`, add after the `TIMER`
branch (~line 291, before the trailing `// Any remaining app:` comment):

```rust
    } else if idx == WATER {
        // The pool fades up from black as the morph completes; the first
        // water_tick (run loop) calibrates and takes over the physics.
        st.wa.reveal(wfb, q_q8, elapsed_ms);
        let r = (CX - 226, CLOCK_Y1_APPS, CX + 226, H - 1);
        fx.push(r.0, r.1, r.2, r.3);
        wfb.mark_rect(r.0, r.1, r.2, r.3);
    }
```

Add the clip constant near the other apps.rs consts (e.g. by `TIMER_R` ~line
40): `const CLOCK_Y1_APPS: i32 = 70;` (mirrors `water.rs`'s `CLOCK_Y1`, so the
reveal rect matches the sim's clip line).

> The reveal renders the *seeded* pool at alpha `q`; `open_app` (below) seeds it.

---

## 4. `app.rs` — read the IMU for Water, thread it in, pin the cadence

### 4a. Tick call site (~line 322, the non-Gallery `else` branch)

```diff
                     } else {
-                        // The app's signature animation (partial redraw).
-                        apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
+                        if idx == apps::WATER {
+                            // Run loop owns self.i2c: read gravity for the
+                            // liquid this frame (~0.2 ms; None on a bus fault
+                            // → the sim reuses its last vector).
+                            let imu = crate::drivers::qmi8658::read_accel(&mut self.i2c).ok();
+                            apps::water_tick(&mut self.wfb, imu, elapsed, &mut app_state);
+                        } else {
+                            // The app's signature animation (partial redraw).
+                            apps::tick(&mut self.wfb, idx, &now, elapsed, &mut app_state);
+                        }
                         if apps::shows_status(idx) && status_minute != now.minute {
                             status_minute = now.minute;
                             wheel::tick_status(&mut self.wfb, &now, wheel_batt);
                         }
                     }
```

(`read_accel` is `crate::drivers::qmi8658::read_accel`, already used at boot in
`bin/main.rs`; the full path needs no new `use`. Optionally add
`use crate::drivers::qmi8658;` by the `use crate::drivers::{axp2101, cst9217};`
line ~27 and call `qmi8658::read_accel`.) The status restamp is preserved, so
the clock updates on minute rollover exactly as for other apps.

### 4b. Cadence — pin Water to 40 fps (~line 457)

```diff
             let frame_us = match power {
                 Power::Aod | Power::Sleep => IDLE_FRAME_US,
                 _ if scene == Scene::Locked && self.clock.is_animating() => CLOCK_ANIM_FRAME_US,
                 // Entrance reveal animates at full cadence.
                 _ if scene == Scene::Wheel && self.wheel_fx.intro_active() => ANIM_FRAME_US,
+                // Water is always alive — animate at 40 fps while Awake
+                // (freezes when Dim: water_tick only runs at Power::Awake).
+                _ if scene == Scene::App(apps::WATER) && power == Power::Awake => ANIM_FRAME_US,
                 _ => FRAME_US,
             };
```

### 4c. Seed on open (`open_app`, ~line 1582, beside the TIME reset)

```diff
         if idx == apps::TIME {
             st.t_anchor = None;
         }
+        if idx == apps::WATER {
+            st.wa.open(); // spawn the pool; first live tick calibrates the rest bias
+        }
         self.app_morph(idx, true, s_q8, st, now, batt, anim_start, tp)
```

That is the entire `app.rs` footprint: one tick-site `if/else`, one `frame_us`
arm, one `open_app` line. No renderer rewrite, no new interactive loop,
`flush_dirty` reused unchanged.

---

## 5. `build.rs` — `WATER_LUT` + `PUSH_LUT` generator

Mirrors `generate_noise_luts` / the RGB565-BE emit idiom. **Register** it in
`main()` (~line 14, beside `generate_wheel_assets();`):

```diff
     generate_gallery_assets();
     generate_wheel_assets();
+    generate_water_lut();
     linker_be_nice();
```

**Generator** (add near `generate_noise_luts`, ~line 214). Emits both tables
into one file `water_lut.rs`, which `water.rs` already `include!`s:

```rust
/// Water app tables: the deep-indigo→neon-blue→cyan→white colour gradient
/// (RGB565-BE pairs, stamped raw at blit time — no per-pixel divide) and the
/// PUSH_LUT[d2] repulsion table (bakes STIFF*(H-d)/d + falloff, clamped, so the
/// hot pair loop is sqrt-free AND divide-free). Mirrors generate_noise_luts.
fn generate_water_lut() {
    // ---- WATER_LUT: 256-entry gradient, piecewise across 5 stops ----------
    let stops: [(i32, (i32, i32, i32)); 5] = [
        (0,   (2,   4,  22)),   // deep body (near-black indigo)
        (72,  (0,  46, 130)),   // deep blue
        (150, (0, 150, 235)),   // signature neon blue
        (208, (120, 226, 255)), // bright cyan crest
        (255, (232, 250, 255)), // white spray / foam
    ];
    let lerp = |a: i32, b: i32, t: i32, d: i32| a + (b - a) * t / d;
    let mut body =
        String::from("/// Auto-generated by build.rs (generate_water_lut). Do not edit.\n");
    body.push_str("pub static WATER_LUT: [(u8, u8); 256] = [");
    for i in 0..256i32 {
        let mut s = 0usize;
        while s + 1 < stops.len() && i > stops[s + 1].0 {
            s += 1;
        }
        let (i0, (r0, g0, b0)) = stops[s];
        let (i1, (r1, g1, b1)) = stops[(s + 1).min(stops.len() - 1)];
        let d = (i1 - i0).max(1);
        let t = (i - i0).clamp(0, d);
        let (r, g, b) = (lerp(r0, r1, t, d), lerp(g0, g1, t, d), lerp(b0, b1, t, d));
        let r5 = ((r.clamp(0, 255) as u16) * 31 / 255) & 0x1F;
        let g6 = ((g.clamp(0, 255) as u16) * 63 / 255) & 0x3F;
        let b5 = ((b.clamp(0, 255) as u16) * 31 / 255) & 0x1F;
        let px = (r5 << 11) | (g6 << 5) | b5;
        if i > 0 {
            body.push(',');
        }
        body.push_str(&format!("({},{})", (px >> 8) as u8, px as u8));
    }
    body.push_str("];\n");

    // ---- PUSH_LUT[d2]: index d2 in 0..=256 -> clamp(STIFF*(H-d)/d, 0, 8192) --
    // d = sqrt(d2). Bakes the 1/d normalisation AND the linear falloff, so the
    // relax loop is one load + two muls + two shifts per pair (no sqrt, no /).
    // d->0 is clamped: overlapping particles get a strong but BOUNDED push
    // (the anti-singularity guarantee).
    const HP: f64 = 16.0; // = H_PX
    const STIFF: f64 = 1600.0;
    body.push_str("pub static PUSH_LUT: [i16; 257] = [");
    for d2 in 0..=256usize {
        let d = (d2 as f64).sqrt();
        let v = if d < 1.0 {
            STIFF * (HP - 1.0)
        } else if d >= HP {
            0.0
        } else {
            STIFF * (HP - d) / d
        };
        if d2 > 0 {
            body.push(',');
        }
        body.push_str(&format!("{}", (v as i32).clamp(0, 8192)));
    }
    body.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let path = std::path::Path::new(&out_dir).join("water_lut.rs");
    std::fs::write(path, body).expect("write water_lut.rs");
}
```

The module include is already in `water_draft.rs`:

```rust
include!(concat!(env!("OUT_DIR"), "/water_lut.rs")); // WATER_LUT, PUSH_LUT
```

`NOISE_A/NOISE_B` are reused as-is (`lock::NOISE_A/NOISE_B`, already generated).
No new sprites — the `waves` splash icon is already vendored.

---

## 6. Build-order note

Land steps 1–5 in the order above, then follow the numbered on-device BUILD
ORDER in IMPL-SPEC §10 (IMU axis-map first — the one true unknown; then particle
core → incompressibility → surface/viscosity → slosh/spray → look → flush/polish
→ const tuning). Every step is independently testable at 40 fps with the clock
topmost.
