# Water — adversarial review: INTEGRATION & COMPILE lens

Scope: will the diffs in `integration.md` + `water_draft.rs` (→ `src/scenes/water.rs`)
actually compile against the repo at this commit? Borrow checker, the tick
signature change, gravity threading from `app.rs`, the `build.rs` `include!`
path, and whether any `src/` edit breaks another scene.

Verdict: **ship.** The wiring compiles as written. One non-blocking dead-code
warning found (below). Everything else checks out against the real code.

---

## Findings

### F1 (low) — unused const `ACC_LSB_PER_G` → `dead_code` warning
`docs/water/water_draft.rs:118` declares `const ACC_LSB_PER_G: i32 = 8192;`,
which is never referenced anywhere in the module (gravity uses `GRAV_SHR`,
calibration uses `REST_BIAS_CLAMP`; grep confirms line 118 is the only hit).
Once copied to `src/scenes/water.rs` this emits a `dead_code` warning. It does
**not** fail the build: neither `lib.rs` nor `app.rs` sets `#![deny(warnings)]`
or `deny(dead_code)` (the only denies are `clippy::mem_forget` /
`clippy::large_stack_frames`, on the *bin* crate root, which don't touch the lib
and aren't run by `cargo build`). Fix: delete the const (or `let _ =
ACC_LSB_PER_G;` — but deletion is correct; the driver already exports its own
`qmi8658::ACC_LSB_PER_G`).

No other unused item exists — imports (`WatchFb`, `lock`) and every other const
and struct field are referenced.

---

## Verified correct (would-be failure points that actually hold)

**Tick signature change — there is none, correctly.** `apps::tick`'s signature
is untouched; Water stays in its `_ => {}` arm (apps.rs:307). A new
`pub fn water_tick(wfb, imu, elapsed_ms, st)` is added and driven from the
run-loop call site. `apps::tick` is never invoked with `WATER` because the
call site (app.rs:322 `else`) is guarded `if idx == apps::WATER { water_tick }
else { tick }`. No exhaustiveness or arity break.

**Borrow: `self.i2c` vs `self.wfb`.** `let imu = qmi8658::read_accel(&mut
self.i2c).ok();` borrows `self.i2c` only for that statement — `.ok()` yields an
owned `Option<(i16,i16,i16)>` (Copy), releasing the borrow. The following
`apps::water_tick(&mut self.wfb, imu, elapsed, &mut app_state)` borrows a
*different* field. Non-overlapping; compiles.

**`apps::State` mutation.** `app_state` is a run-loop local (`let mut app_state
= apps::State::new();`, app.rs:175), disjoint from `self.wfb`, so `&mut
self.wfb` + `&mut app_state` in one call is legal — identical to the existing
`apps::tick` call it replaces. The new `pub wa: Water` field is additive; `State`
derives nothing, so no derive is invalidated. `Water::new()` is `const fn`, so
`State::new()` stays `const`.

**Gravity threading / `read_accel`.** Real signature is
`pub fn read_accel(i2c: &mut I2c<'_, Blocking>) -> Result<(i16,i16,i16),()>`
(qmi8658.rs:48); `self.i2c: I2c<'d, Blocking>` (app.rs:132). `crate::drivers::
qmi8658` is a plain `pub mod` (drivers.rs:8, *not* `cfg`-gated), so the full
path resolves with no new `use`. `.ok()`'s `Option<(i16,i16,i16)>` matches
`water_tick`'s `imu` param exactly. `elapsed` is `u32` (app.rs:182) → matches
`elapsed_ms: u32`.

**`frame_us` arm.** `_ if scene == Scene::App(apps::WATER) && power ==
Power::Awake => ANIM_FRAME_US` — `Scene` derives `PartialEq` (app.rs:81),
`Scene::App(usize)` exists, `apps::WATER: usize` is pub, `Power` derives
`PartialEq` (app.rs:92) and `Awake` exists, `ANIM_FRAME_US` is in scope (used by
the arm above). Compiles; additive, no other arm affected.

**`open_app` seed.** open_app has `if idx == apps::TIME { st.t_anchor = None; }`
with `st: &mut apps::State` (app.rs:1568-1585); the added
`if idx == apps::WATER { st.wa.open(); }` before `self.app_morph(...)` calls the
pub `Water::open(&mut self)`. Fine.

**Reveal branch is reachable and type-correct.** `app_morph` calls
`apps::draw_reveal(&mut self.wfb, &mut self.wheel_fx, now, batt, idx, q,
elapsed, st)` (app.rs:1695) when `q > 0`; `has_content(WATER)` now returns
`true`, so the content phase (`T_CONTENT`, app.rs:1616) runs and this fires.
Inside the new `else if idx == WATER` arm, `st.wa.reveal(wfb, q_q8,
elapsed_ms)` matches `Water::reveal(&mut self, &mut WatchFb, i32, u32)`; `fx`
is `&mut wheel::WheelFx` so `fx.push(..)` resolves; `CX`/`H` are module consts;
the diff adds `const CLOCK_Y1_APPS: i32 = 70;` (no name clash). Reborrow of
`wfb` into `reveal` ends before `wfb.mark_rect(..)`.

**`build.rs` `include!` path.** `water.rs` has `include!(concat!(env!("OUT_DIR"),
"/water_lut.rs"))`; the added `generate_water_lut()` writes exactly
`$OUT_DIR/water_lut.rs` and is registered in `main()` after
`generate_wheel_assets()` — same OUT_DIR/`std::fs::write` idiom as
`generate_noise_luts`. Cargo runs the build script before the crate, and the
existing `cargo:rerun-if-changed` force-rerun keeps it regenerated. Emitted
`pub static WATER_LUT: [(u8,u8);256]` / `pub static PUSH_LUT: [i16;257]` are the
symbols `water.rs` indexes; all `PUSH_LUT` values are `clamp(_,0,8192)` ≤ i16::MAX,
and `d2` (post-reject 1..=255) never exceeds the 257-len table. Generator body is
host-std (`String`/`format!`/`f64::sqrt`) — compiles.

**No other scene breaks.** `scenes.rs` gains only `pub mod water;` (additive).
`has_content` change is `WATER`-only; every other idx still returns `true` as
before. `NOISE_A`/`NOISE_B` are `pub static [u8;256]` at `lock` module scope
(build.rs:237 emit; lock.rs:23 include), so `lock::NOISE_A`/`NOISE_B` resolve
from `water.rs`; aurora indices are masked `& 255` (0..255, safe even for
negative operands). Duplicated helpers (`max_px`, `isqrt`, `fill_rect_black`,
etc.) live in the `water` module and do not collide with the identically-named
private fns in `apps` (different modules). `water.rs` is `no_std`-clean (fixed
arrays only, no alloc). The file-top inner attributes (`//!` docs + `#![allow(
clippy::...)]`) are legal as module-file inner attributes.
