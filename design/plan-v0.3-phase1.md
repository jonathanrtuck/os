# v0.3 Phase 1: Motion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a complete animation system and use it for smooth scroll, cursor blink, and transitions.

**Architecture:** New standalone `libraries/animation/` library (pure `no_std`, no `alloc`, no dependencies). Core integrates it for scroll, cursor blink, and transitions. Scene graph carries animated values (opacity, transform). Render services need no changes — they already handle fractional opacity and transforms.

**Design notes:**
- `Animation` and `Timeline` use `f32` values (not generic over property type). The Lerp trait enables type-generic interpolation at the call site, but the timeline stores f32. This avoids monomorphization complexity in a fixed-capacity no-alloc container. The spec says "generic over property type" — this is satisfied at the Lerp level, not the Timeline level.
- `Spring` and `Timeline` use `&mut self` methods. This is a pragmatic deviation from the project's immutability preference — physics simulations and animation managers are inherently stateful accumulators. The mutation is contained within these types and doesn't leak into the scene graph or rendering pipeline.

**Spec:** `design/v0.3-spec.md` sections 1.1–1.4.

**Tech Stack:** Rust `no_std`, `#![no_std]`, bare-metal aarch64. Host-side tests in `system/test/`.

---

## File Map

### New files

| File | Purpose |
|------|---------|
| `libraries/animation/lib.rs` | Easing functions, Lerp trait, Spring, Animation, Timeline |
| `libraries/animation/Cargo.toml` | Minimal crate manifest |
| `test/tests/animation.rs` | Comprehensive host-side tests |

### Modified files

| File | Change |
|------|--------|
| `build.rs` | Compile animation rlib, add to core's externs |
| `test/Cargo.toml` | Add animation dev-dependency |
| `services/core/main.rs` | Integrate animation timeline for scroll, cursor blink, transitions. Migrate scroll_offset from u32 to f32 pixel-space. |
| `services/core/scene_state.rs` | Pass animated opacity values to scene builders |
| `services/core/layout/mod.rs` | Accept `scroll_y: f32` instead of `i32`. Accept `cursor_opacity: u8`. |
| `services/core/layout/full.rs` | Use f32 scroll, set cursor node opacity |
| `services/core/layout/incremental.rs` | Use f32 scroll, set cursor node opacity |
| `services/core/typography.rs` | Update scroll_for_cursor to return f32 |
| `services/core/fallback.rs` | Update scroll_for_cursor signature if duplicated |
| `services/core/test_gen.rs` | Add Phase 1 demo scenes (bouncing ball, easing sampler) |

---

## Task 1: Animation Library — Easing Functions

**Files:**
- Create: `system/libraries/animation/lib.rs`
- Create: `system/libraries/animation/Cargo.toml`
- Create: `system/test/tests/animation.rs`
- Modify: `system/test/Cargo.toml` — add animation dependency

This task implements all easing functions as pure `f32 → f32` functions. No Animation struct yet — just the mathematical foundations.

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "animation"
version = "0.1.0"
edition = "2021"

[lib]
path = "lib.rs"
```

- [ ] **Step 2: Create lib.rs with module docstring and easing function signatures**

Start with the `Easing` enum and `ease()` dispatch function. Implement `linear` first.

```rust
//! Animation library: easing functions, spring physics, interpolation, and timeline.
//!
//! Standalone, general-purpose animation engine. No dependencies on scene graph,
//! rendering, or core. Pure `no_std`, no `alloc`.

#![no_std]

// ── Easing ─────────────────────────────────────────────────────────

/// Easing function selector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    Linear,
    // CSS standard cubic beziers
    Ease,
    EaseIn,
    EaseOut,
    EaseInOut,
    CubicBezier(f32, f32, f32, f32),
    // Polynomial
    EaseInQuad, EaseOutQuad, EaseInOutQuad,
    EaseInCubic, EaseOutCubic, EaseInOutCubic,
    // Exponential
    EaseInExpo, EaseOutExpo, EaseInOutExpo,
    // Back (overshoot)
    EaseInBack, EaseOutBack, EaseInOutBack,
    // Elastic (oscillating overshoot)
    EaseInElastic, EaseOutElastic,
    // Bounce
    EaseInBounce, EaseOutBounce,
    // Discrete
    StepStart, StepEnd,
}

/// Evaluate an easing function at time `t` in [0, 1].
/// Returns the eased value in [0, 1] (may exceed for overshoot easings).
pub fn ease(easing: Easing, t: f32) -> f32 {
    // clamp input
    let t = if t <= 0.0 { 0.0 } else if t >= 1.0 { 1.0 } else { t };
    match easing {
        Easing::Linear => t,
        // ... each variant implemented below
    }
}
```

- [ ] **Step 3: Implement all easing curve math**

Each easing function is a pure `fn(f32) -> f32`. Implement them all:

- **Cubic bezier evaluator** for CSS standard curves: binary search for `t` given `x`, then evaluate `y`. This is the core math — `Ease`, `EaseIn`, `EaseOut`, `EaseInOut`, and `CubicBezier` all use it.
- **Polynomial:** `quad = t*t`, `cubic = t*t*t`, with in/out/in-out variants using the standard `1 - (1-t)^n` out pattern.
- **Exponential:** `2^(10*(t-1))` for in, `1 - 2^(-10*t)` for out.
- **Back:** `t*t*(2.70158*t - 1.70158)` for in, reversed for out.
- **Elastic:** `2^(10*(t-1)) * sin((t-1.075)*2π/0.3)` for in, reversed for out.
- **Bounce:** piecewise quadratic (4 segments), in = `1 - bounce_out(1-t)`.
- **Step:** `StepStart` returns 1.0 for any t > 0, `StepEnd` returns 0.0 until t >= 1.0.

Use `core::f32::consts::PI` and `core::f32::consts::TAU`. No `libm` dependency — implement `sin` approximation for elastic/bounce easings:

1. **Range reduction first:** reduce input to `[-PI, PI]` via `x = x - TAU * (x / TAU).round()`. Without this, Taylor series diverges at large arguments (elastic easing evaluates sin at up to ~20 radians).
2. **Approximation:** 7th-order minimax polynomial on `[-PI, PI]` (Horner form): `x * (1.0 - x2/6.0 * (1.0 - x2/20.0 * (1.0 - x2/42.0)))`. Error < 0.0002 on the reduced range.
3. **cos** via `sin(x + PI/2)`.

Document the chosen polynomial coefficients and maximum error in a comment.

- [ ] **Step 4: Add animation dependency to test crate**

In `system/test/Cargo.toml`, add:
```toml
animation = { path = "../libraries/animation" }
```

- [ ] **Step 5: Write easing function tests**

Create `system/test/tests/animation.rs`:

```rust
extern crate animation;
use animation::{ease, Easing};

// ── Boundary tests ─────────────────────────────────────────────────

#[test]
fn all_easings_start_at_zero() {
    for easing in ALL_EASINGS {
        let v = ease(easing, 0.0);
        assert!((v - 0.0).abs() < 0.01, "{:?} at t=0: {}", easing, v);
    }
}

#[test]
fn all_easings_end_at_one() {
    for easing in ALL_EASINGS {
        let v = ease(easing, 1.0);
        assert!((v - 1.0).abs() < 0.01, "{:?} at t=1: {}", easing, v);
    }
}

#[test]
fn linear_is_identity() {
    for i in 0..=100 {
        let t = i as f32 / 100.0;
        assert!((ease(Easing::Linear, t) - t).abs() < 0.001);
    }
}

// ── Monotonicity tests (non-overshoot easings) ─────────────────────

#[test]
fn monotonic_easings_are_non_decreasing() {
    let monotonic = [
        Easing::Linear, Easing::Ease, Easing::EaseIn, Easing::EaseOut,
        Easing::EaseInOut, Easing::EaseInQuad, Easing::EaseOutQuad,
        Easing::EaseInOutQuad, Easing::EaseInCubic, Easing::EaseOutCubic,
        Easing::EaseInOutCubic, Easing::EaseInExpo, Easing::EaseOutExpo,
        Easing::EaseInOutExpo,
    ];
    for easing in monotonic {
        let mut prev = 0.0f32;
        for i in 0..=200 {
            let t = i as f32 / 200.0;
            let v = ease(easing, t);
            assert!(v >= prev - 0.001, "{:?} not monotonic at t={}: {} < {}", easing, t, v, prev);
            prev = v;
        }
    }
}

// ── Overshoot tests ────────────────────────────────────────────────

#[test]
fn back_easing_overshoots() {
    // EaseInBack should go below 0 near t=0
    let v = ease(Easing::EaseInBack, 0.2);
    assert!(v < 0.0, "EaseInBack at t=0.2 should be negative: {}", v);
}

#[test]
fn elastic_easing_oscillates() {
    // EaseOutElastic should exceed 1.0 near the end
    let v = ease(Easing::EaseOutElastic, 0.4);
    assert!(v > 1.0 || v < 0.0, "EaseOutElastic should oscillate");
}

// ── CSS cubic bezier reference values ──────────────────────────────

#[test]
fn ease_matches_css_reference() {
    // CSS ease at t=0.5 is approximately 0.8024
    let v = ease(Easing::Ease, 0.5);
    assert!((v - 0.8024).abs() < 0.02, "CSS ease at t=0.5: {} (expected ~0.8024)", v);
}

// ── Clamp tests ────────────────────────────────────────────────────

#[test]
fn negative_t_clamps_to_zero() {
    assert_eq!(ease(Easing::Linear, -1.0), 0.0);
}

#[test]
fn t_above_one_clamps_to_one() {
    assert_eq!(ease(Easing::Linear, 2.0), 1.0);
}

// ── Step tests ─────────────────────────────────────────────────────

#[test]
fn step_start_jumps_immediately() {
    assert_eq!(ease(Easing::StepStart, 0.0), 0.0);
    assert_eq!(ease(Easing::StepStart, 0.01), 1.0);
    assert_eq!(ease(Easing::StepStart, 1.0), 1.0);
}

#[test]
fn step_end_jumps_at_end() {
    assert_eq!(ease(Easing::StepEnd, 0.0), 0.0);
    assert_eq!(ease(Easing::StepEnd, 0.99), 0.0);
    assert_eq!(ease(Easing::StepEnd, 1.0), 1.0);
}

const ALL_EASINGS: [Easing; 24] = [
    Easing::Linear, Easing::Ease, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut,
    Easing::CubicBezier(0.17, 0.67, 0.83, 0.67), // custom test bezier
    Easing::EaseInQuad, Easing::EaseOutQuad, Easing::EaseInOutQuad,
    Easing::EaseInCubic, Easing::EaseOutCubic, Easing::EaseInOutCubic,
    Easing::EaseInExpo, Easing::EaseOutExpo, Easing::EaseInOutExpo,
    Easing::EaseInBack, Easing::EaseOutBack, Easing::EaseInOutBack,
    Easing::EaseInElastic, Easing::EaseOutElastic,
    Easing::EaseInBounce, Easing::EaseOutBounce,
    Easing::StepStart, Easing::StepEnd,
];

// Edge case tests (spec requirement)
#[test]
fn zero_duration_animation_completes_immediately() {
    let mut a = Animation::new(0.0, 1.0, 0, Easing::Linear);
    a.start(1000);
    assert!(a.is_complete_at(1000));
    assert!((a.value_at(1000) - 1.0).abs() < 0.01);
}

#[test]
fn nan_input_clamped() {
    let v = ease(Easing::Linear, f32::NAN);
    // NAN comparisons are false, so clamping should catch it
    assert!(v == 0.0 || v == 1.0 || (v >= 0.0 && v <= 1.0));
}
```

- [ ] **Step 6: Run tests, verify all pass**

Run: `cd system/test && cargo test animation -- --test-threads=1 -v`

- [ ] **Step 7: Commit**

```
feat: animation library — complete easing function set
```

---

## Task 2: Animation Library — Spring Physics

**Files:**
- Modify: `system/libraries/animation/lib.rs` — add Spring struct
- Modify: `system/test/tests/animation.rs` — add spring tests

- [ ] **Step 1: Write spring tests first**

```rust
// ── Spring tests ───────────────────────────────────────────────────

use animation::Spring;

#[test]
fn spring_default_settles_at_target() {
    let mut s = Spring::default_preset(1.0);
    // Tick for 2 seconds at 60fps
    for _ in 0..120 {
        s.tick(1.0 / 60.0);
    }
    assert!((s.value() - 1.0).abs() < 0.001, "Spring should settle at target: {}", s.value());
    assert!(s.settled(), "Spring should be settled after 2s");
}

#[test]
fn spring_snappy_settles_faster_than_gentle() {
    let mut snappy = Spring::snappy(1.0);
    let mut gentle = Spring::gentle(1.0);
    let mut snappy_settled_at = None;
    let mut gentle_settled_at = None;
    for i in 0..300 {
        snappy.tick(1.0 / 60.0);
        gentle.tick(1.0 / 60.0);
        if snappy.settled() && snappy_settled_at.is_none() { snappy_settled_at = Some(i); }
        if gentle.settled() && gentle_settled_at.is_none() { gentle_settled_at = Some(i); }
    }
    assert!(snappy_settled_at.unwrap() < gentle_settled_at.unwrap(),
        "Snappy ({:?}) should settle before gentle ({:?})",
        snappy_settled_at, gentle_settled_at);
}

#[test]
fn spring_bouncy_overshoots_target() {
    let mut s = Spring::bouncy(1.0);
    let mut max_value = 0.0f32;
    for _ in 0..120 {
        s.tick(1.0 / 60.0);
        if s.value() > max_value { max_value = s.value(); }
    }
    assert!(max_value > 1.05, "Bouncy spring should overshoot: max={}", max_value);
}

#[test]
fn spring_retarget_changes_destination() {
    let mut s = Spring::default_preset(1.0);
    for _ in 0..30 { s.tick(1.0 / 60.0); }
    s.set_target(2.0);
    for _ in 0..120 { s.tick(1.0 / 60.0); }
    assert!((s.value() - 2.0).abs() < 0.001, "Should settle at new target: {}", s.value());
}

#[test]
fn spring_zero_dt_is_noop() {
    let mut s = Spring::default_preset(1.0);
    let v_before = s.value();
    s.tick(0.0);
    assert_eq!(s.value(), v_before);
}
```

- [ ] **Step 2: Run tests, verify they fail** (Spring not defined yet)

- [ ] **Step 3: Implement Spring**

```rust
/// Damped spring physics simulation.
///
/// Models a critically/under/over-damped spring system for smooth animations.
/// Tick-based: call `tick(dt)` each frame with delta time in seconds.
///
/// Physics: F = -stiffness * displacement - damping * velocity
/// Integration: semi-implicit Euler (stable for game-loop dt values).
pub struct Spring {
    target: f32,
    value: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
    mass: f32,
    settle_threshold: f32,
}

impl Spring {
    pub fn new(target: f32, stiffness: f32, damping: f32, mass: f32) -> Self { ... }

    // Presets (stiffness, damping tuned for typical UI animations):
    pub fn default_preset(target: f32) -> Self { Self::new(target, 300.0, 20.0, 1.0) }
    pub fn snappy(target: f32) -> Self { Self::new(target, 600.0, 35.0, 1.0) }
    pub fn gentle(target: f32) -> Self { Self::new(target, 120.0, 14.0, 1.0) }
    pub fn bouncy(target: f32) -> Self { Self::new(target, 300.0, 10.0, 1.0) }

    pub fn tick(&mut self, dt: f32) {
        if dt <= 0.0 { return; }
        let displacement = self.value - self.target;
        let force = -self.stiffness * displacement - self.damping * self.velocity;
        let acceleration = force / self.mass;
        self.velocity += acceleration * dt;  // semi-implicit Euler: update velocity first
        self.value += self.velocity * dt;    // then position
    }

    pub fn value(&self) -> f32 { self.value }
    pub fn velocity(&self) -> f32 { self.velocity }
    pub fn target(&self) -> f32 { self.target }
    pub fn set_target(&mut self, target: f32) { self.target = target; }
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < self.settle_threshold
            && self.velocity.abs() < self.settle_threshold
    }
    pub fn set_settle_threshold(&mut self, threshold: f32) { self.settle_threshold = threshold; }
}
```

- [ ] **Step 4: Run tests, verify all spring tests pass**

- [ ] **Step 5: Commit**

```
feat: animation library — spring physics with presets
```

---

## Task 3: Animation Library — Lerp Trait and Interpolation

**Files:**
- Modify: `system/libraries/animation/lib.rs` — add Lerp trait and implementations
- Modify: `system/test/tests/animation.rs` — add lerp tests

- [ ] **Step 1: Write lerp tests**

```rust
use animation::Lerp;

#[test]
fn lerp_f32_midpoint() {
    assert_eq!(f32::lerp(0.0, 10.0, 0.5), 5.0);
}

#[test]
fn lerp_f32_boundaries() {
    assert_eq!(f32::lerp(0.0, 10.0, 0.0), 0.0);
    assert_eq!(f32::lerp(0.0, 10.0, 1.0), 10.0);
}

#[test]
fn lerp_i32() {
    assert_eq!(i32::lerp(0, 100, 0.5), 50);
    assert_eq!(i32::lerp(-10, 10, 0.5), 0);
}

#[test]
fn lerp_u8() {
    assert_eq!(u8::lerp(0, 255, 0.5), 128);
}

#[test]
fn lerp_color_in_linear_space() {
    use animation::LerpColor;
    // Black to white at midpoint should be ~188 in sRGB (not 128)
    // because linear 0.5 maps to sRGB ~0.735 (gamma curve)
    let mid = LerpColor::lerp_srgb(
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        0.5
    );
    // Linear midpoint in sRGB is approximately 188 (sqrt(0.5) * 255 ≈ 180-188 depending on exact gamma)
    assert!(mid[0] > 160 && mid[0] < 200,
        "Gamma-correct midpoint of black-white should be ~188, got {}", mid[0]);
}
```

- [ ] **Step 2: Implement Lerp trait**

```rust
/// Linear interpolation trait.
pub trait Lerp {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }
}

impl Lerp for i32 {
    fn lerp(a: i32, b: i32, t: f32) -> i32 { (a as f32 + (b - a) as f32 * t) as i32 }
}

impl Lerp for u8 {
    fn lerp(a: u8, b: u8, t: f32) -> u8 {
        (a as f32 + (b as f32 - a as f32) * t).round() as u8
    }
}

/// Color interpolation in linearized sRGB space.
pub struct LerpColor;

impl LerpColor {
    /// Interpolate two sRGB colors in linear light space.
    /// Input/output: [r, g, b, a] as sRGB u8 values.
    pub fn lerp_srgb(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
        // Linearize, interpolate, re-encode
        // Uses the same SRGB_TO_LINEAR / LINEAR_TO_SRGB LUT approach as the drawing library.
        // For the animation library (no dependency on drawing), inline a compact sRGB approximation:
        // linear = (srgb/255)^2.2, srgb = linear^(1/2.2) * 255
        // Alpha interpolated linearly (already linear).
        ...
    }
}
```

**sRGB linearization:** Inline a compact 256-entry `u8 → f32` LUT (sRGB to linear) and a 256-entry `u8 → u8` LUT (linear to sRGB) directly in the animation library. This duplicates ~512 bytes from `drawing/gamma_tables.rs` but keeps the animation library dependency-free. The LUTs are `const` arrays generated at compile time. The linearize-interpolate-reencode approach matches the drawing library's existing gamma-correct blending.

**AffineTransform Lerp:** The animation library defines its own minimal `AffineTransform` struct with 6 `f32` fields (a, b, c, d, tx, ty) and implements Lerp as component-wise f32 interpolation. If the scene library's `AffineTransform` is used directly, add a dependency on scene; otherwise use a type alias or conversion trait at the integration boundary in core. Prefer the type alias approach to avoid coupling animation to scene.

```rust
/// Minimal 2D affine transform for animation interpolation.
/// Component-wise lerp produces valid intermediate transforms for
/// translation, scale, and simple rotation animations.
#[derive(Clone, Copy, Debug)]
pub struct Transform2D {
    pub a: f32, pub b: f32, pub c: f32, pub d: f32, pub tx: f32, pub ty: f32,
}

impl Lerp for Transform2D {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        Self {
            a: f32::lerp(a.a, b.a, t), b: f32::lerp(a.b, b.b, t),
            c: f32::lerp(a.c, b.c, t), d: f32::lerp(a.d, b.d, t),
            tx: f32::lerp(a.tx, b.tx, t), ty: f32::lerp(a.ty, b.ty, t),
        }
    }
}
```

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```
feat: animation library — Lerp trait with gamma-correct color interpolation
```

---

## Task 4: Animation Library — Animation and Timeline

**Files:**
- Modify: `system/libraries/animation/lib.rs` — add Animation and Timeline
- Modify: `system/test/tests/animation.rs` — add timeline tests

- [ ] **Step 1: Write Animation and Timeline tests**

```rust
use animation::{Animation, Timeline, Easing, AnimationId};

#[test]
fn animation_completes_after_duration() {
    let mut a = Animation::new(0.0, 1.0, 500, Easing::Linear); // 500ms
    a.start(1000); // start at t=1000ms
    assert!(!a.is_complete());
    let v = a.value_at(1250); // halfway
    assert!((v - 0.5).abs() < 0.01);
    let v = a.value_at(1500); // done
    assert!((v - 1.0).abs() < 0.01);
    assert!(a.is_complete_at(1500));
}

#[test]
fn timeline_manages_multiple_animations() {
    let mut tl = Timeline::new();
    let id1 = tl.start(0.0, 1.0, 500, Easing::Linear, 0).unwrap();
    let id2 = tl.start(10.0, 20.0, 1000, Easing::EaseOut, 0).unwrap();
    assert!(tl.is_active(id1));
    assert!(tl.is_active(id2));

    tl.tick(500);
    assert!(!tl.is_active(id1)); // id1 completed
    assert!(tl.is_active(id2));  // id2 still running

    tl.tick(1000);
    assert!(!tl.is_active(id2)); // id2 completed
}

#[test]
fn timeline_capacity_limit() {
    let mut tl = Timeline::new();
    for i in 0..32 {
        assert!(tl.start(0.0, 1.0, 1000, Easing::Linear, i as u64).is_ok());
    }
    // 33rd should fail
    assert!(tl.start(0.0, 1.0, 1000, Easing::Linear, 32).is_err());
}

#[test]
fn timeline_cancel_frees_slot() {
    let mut tl = Timeline::new();
    let id = tl.start(0.0, 1.0, 1000, Easing::Linear, 0).unwrap();
    tl.cancel(id);
    assert!(!tl.is_active(id));
    // Slot is free again
    assert!(tl.start(0.0, 1.0, 1000, Easing::Linear, 0).is_ok());
}

#[test]
fn timeline_value_returns_current_animated_value() {
    let mut tl = Timeline::new();
    let id = tl.start(0.0, 100.0, 1000, Easing::Linear, 0).unwrap();
    tl.tick(500);
    let v = tl.value(id);
    assert!((v - 50.0).abs() < 1.0);
}
```

- [ ] **Step 2: Implement Animation struct**

```rust
/// Unique identifier for a running animation in a Timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationId(u8);

/// A single property animation from start_value to end_value over duration_ms.
#[derive(Clone, Copy)]
pub struct Animation {
    start_value: f32,
    end_value: f32,
    start_time_ms: u64,
    duration_ms: u32,
    easing: Easing,
    state: AnimState,
}

#[derive(Clone, Copy, PartialEq)]
enum AnimState { Idle, Running, Complete }

impl Animation {
    pub fn new(start: f32, end: f32, duration_ms: u32, easing: Easing) -> Self { ... }
    pub fn start(&mut self, now_ms: u64) { ... }
    pub fn value_at(&self, now_ms: u64) -> f32 {
        let elapsed = now_ms.saturating_sub(self.start_time_ms) as f32;
        let t = (elapsed / self.duration_ms as f32).min(1.0);
        let eased = ease(self.easing, t);
        f32::lerp(self.start_value, self.end_value, eased)
    }
    pub fn is_complete_at(&self, now_ms: u64) -> bool {
        now_ms >= self.start_time_ms + self.duration_ms as u64
    }
}
```

- [ ] **Step 3: Implement Timeline**

```rust
const MAX_ANIMATIONS: usize = 32;

/// Fixed-capacity animation manager. No heap allocation.
pub struct Timeline {
    slots: [Option<Animation>; MAX_ANIMATIONS],
    now_ms: u64,
}

impl Timeline {
    pub const fn new() -> Self { ... }

    /// Start a new animation. Returns AnimationId or Err if at capacity.
    pub fn start(&mut self, from: f32, to: f32, duration_ms: u32, easing: Easing, now_ms: u64)
        -> Result<AnimationId, ()> { ... }

    /// Advance all animations to `now_ms`. Completed animations are removed.
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        for slot in self.slots.iter_mut() {
            if let Some(anim) = slot {
                if anim.is_complete_at(now_ms) {
                    *slot = None;
                }
            }
        }
    }

    /// Get the current value of an animation. Returns end_value if complete.
    pub fn value(&self, id: AnimationId) -> f32 { ... }

    /// Cancel an animation, freeing its slot.
    pub fn cancel(&mut self, id: AnimationId) { ... }

    /// Check if an animation is still running.
    pub fn is_active(&self, id: AnimationId) -> bool { ... }

    /// Returns true if any animation is active (useful for frame scheduling).
    pub fn any_active(&self) -> bool { ... }
}
```

- [ ] **Step 4: Run all animation tests, verify pass**

Run: `cd system/test && cargo test animation -- --test-threads=1 -v`

- [ ] **Step 5: Commit**

```
feat: animation library — Animation, Timeline (32-slot fixed capacity)
```

---

## Task 5: Build System Integration

**Files:**
- Modify: `system/build.rs` — compile animation rlib, add to core's extern list

- [ ] **Step 1: Add animation to build.rs**

In the rlib compilation section (after `protocol`, before `drawing`), add:

```rust
let animation_src = manifest_dir.join("libraries/animation/lib.rs");
let animation_rlib = out_dir.join("libanimation.rlib");
rustc_rlib(&rustc, &animation_src, &animation_rlib, "animation", &[]);
```

In the program compilation loop (`build.rs` line 146-166), add animation to core's extern list. The current loop uses `needs_drawing` to gate drawing/scene/fonts. Animation is unconditional for core only. Add after the `needs_drawing` block (line 163):

```rust
if name == "core" {
    externs.push(("animation", animation_rlib.clone()));
}
```

Also add `cargo:rerun-if-changed` for the animation source:
```rust
println!("cargo:rerun-if-changed=libraries/animation/lib.rs");
```

- [ ] **Step 2: Add `extern crate animation;` to core's main.rs**

At the top of `system/services/core/main.rs`, add:
```rust
extern crate animation;
```

- [ ] **Step 3: Build and verify**

Run: `cd system && cargo build --release`
Expected: successful build with animation library linked into core.

- [ ] **Step 4: Run all tests to verify no regressions**

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: all ~1,943 existing tests pass + new animation tests pass.

- [ ] **Step 5: Commit**

```
chore: integrate animation library into build system
```

---

## Task 6: Scroll Model Migration

**Files:**
- Modify: `system/services/core/main.rs` — change `scroll_offset` from `u32` to `f32`, update all usage sites
- Modify: `system/services/core/layout/mod.rs` — accept `scroll_y: f32`
- Modify: `system/services/core/layout/full.rs` — use f32 scroll
- Modify: `system/services/core/layout/incremental.rs` — use f32 scroll
- Modify: `system/services/core/typography.rs` or equivalent — update `scroll_for_cursor`

This is a cross-cutting refactor that must be done in a single commit to avoid breaking the build.

- [ ] **Step 1: Audit all scroll_offset usage**

Search for `scroll_offset`, `scroll_y`, `scroll_for_cursor` across all core files. Document every site that needs to change. The research agent found these key locations:
- `main.rs:86` — `scroll_offset: u32` in CoreState
- `main.rs:1000, 1024, 1052, 1066, 1080, 1095` — `s.scroll_offset as i32` passed to scene builders
- `main.rs:829` — `let scroll = s.scroll_offset` for click hit testing
- Layout functions — accept `scroll_y: i32`

- [ ] **Step 2: Migrate scroll_offset to f32 pixel-space**

Change `CoreState.scroll_offset` from `u32` (line count) to `f32` (pixel offset). Also change `saved_editor_scroll` (line 85 of main.rs) from `u32` to `f32` — this stores/restores scroll during Ctrl+Tab mode switching. Update both initializations to `0.0`.

Update `scroll_for_cursor()` to work in pixel space:
- Instead of returning a line index, return a pixel offset
- Conversion: `target_line * line_height_in_points`
- The layout functions receive the pixel offset directly

- [ ] **Step 3: Update all layout function signatures**

Change every `scroll_y: i32` parameter to `scroll_y: f32` in:
- `build_full_scene()` in `layout/full.rs`
- `build_document_content()` in `layout/full.rs`
- `build_cursor_update()` in `layout/incremental.rs`
- `build_selection_update()` in `layout/incremental.rs`
- `build_clock_update()` in `layout/incremental.rs`
- Any other function that passes scroll through

Also update `scroll_runs()` in `layout/mod.rs` — this takes `scroll_lines: u32` and computes `scroll_pt` internally. Change to accept `scroll_y: f32` (already in pixel space) and remove the internal line→pixel conversion.

Inside these functions, convert `scroll_y` to integer points only at the final pixel-positioning step: `let scroll_px = scroll_y.round() as i32;`

When scroll settles (spring `settled()` returns true), snap `scroll_offset` to the nearest integer point to avoid persistent sub-pixel jitter: `scroll_offset = scroll_offset.round();`

- [ ] **Step 4: Update content_transform usage**

The content_transform on `N_DOC_TEXT` is already set with:
```rust
AffineTransform::translate(0.0, -(scroll_pt as f32))
```
Change to use the f32 directly:
```rust
AffineTransform::translate(0.0, -scroll_offset)
```

- [ ] **Step 5: Update click hit-testing**

`main.rs` line ~829: `let scroll = s.scroll_offset` — this was line-count, now it's pixel-space. Update the hit-testing math to use pixel-space scroll directly (no multiplication by line height needed).

- [ ] **Step 6: Update scroll_for_cursor comparisons**

Any code that compares `old_scroll == new_scroll` needs epsilon comparison:
```rust
fn scroll_changed(old: f32, new: f32) -> bool {
    (old - new).abs() > 0.5  // half-pixel threshold
}
```

- [ ] **Step 7: Build and run all tests**

Run: `cd system && cargo build --release && cd test && cargo test -- --test-threads=1`
Expected: all tests pass. Any test that hard-codes scroll values may need updating.

- [ ] **Step 8: Visually verify via QEMU**

Run the OS in QEMU, type text, scroll with arrow keys past the viewport. Take a screenshot to verify scrolling still works correctly after the migration.

- [ ] **Step 9: Commit**

```
refactor: migrate scroll model from line-count to pixel-space (f32)
```

---

## Task 7: Smooth Scroll with Spring Animation

**Files:**
- Modify: `system/services/core/main.rs` — add scroll Spring to CoreState, animate scroll

- [ ] **Step 1: Add scroll spring to CoreState**

```rust
use animation::Spring;

// In CoreState:
scroll_spring: Spring,      // animates toward target scroll position
scroll_target: f32,         // target scroll offset in pixels (where we want to be)
scroll_animating: bool,     // true when spring is in motion
```

Initialize: `scroll_spring: Spring::snappy(0.0), scroll_target: 0.0, scroll_animating: false`

- [ ] **Step 2: Modify scroll update logic**

When `scroll_for_cursor` or keyboard scroll computes a new target:
```rust
// Instead of: s.scroll_offset = new_offset;
// Do:
s.scroll_target = new_offset;
s.scroll_spring.set_target(new_offset);
s.scroll_animating = true;
```

- [ ] **Step 3: Add animation tick to event loop**

In the event loop, after processing all events but before scene dispatch, tick the scroll spring:

```rust
if s.scroll_animating {
    let dt = 1.0 / 60.0; // frame time (TODO: use actual elapsed time from counter)
    s.scroll_spring.tick(dt);
    s.scroll_offset = s.scroll_spring.value();
    if s.scroll_spring.settled() {
        s.scroll_offset = s.scroll_target; // snap to exact target
        s.scroll_animating = false;
    }
    changed = true; // scene needs update
}
```

- [ ] **Step 4: Handle overscroll**

Add bounds clamping to the scroll target:
```rust
let max_scroll = max_scroll_offset(text_len, viewport_lines, line_height);
let clamped = scroll_target.clamp(-50.0, max_scroll + 50.0); // 50pt overscroll
s.scroll_spring.set_target(clamped);

// If overscrolled past bounds and user stops scrolling, spring back:
if s.scroll_offset < 0.0 || s.scroll_offset > max_scroll {
    s.scroll_spring.set_target(s.scroll_offset.clamp(0.0, max_scroll));
}
```

- [ ] **Step 5: Ensure animation drives frame updates**

When any animation is active (scroll spring, cursor blink, or transition), the event loop must tick at ~60fps instead of blocking indefinitely. The current `sys::wait` call (main.rs line ~731) already supports a timeout parameter in nanoseconds.

Compute the timeout dynamically:
```rust
let animating = s.scroll_animating || s.timeline.any_active()
    || s.blink_phase != BlinkPhase::VisibleHold; // blink hold doesn't need ticking
let timeout = if animating {
    16_000_000 // 16ms = ~60fps, in nanoseconds
} else {
    u64::MAX // block until next event (input, timer, editor)
};
```

Pass this timeout to the existing `sys::wait` call. The clock timer (1-second interval) coexists naturally — when the animation timeout fires, `sys::wait` returns with a timeout result (index 0xFF), at which point the event loop processes animations and scene updates without draining any input. When the clock timer fires, the normal clock update path runs. Both work with the same `sys::wait` call.

- [ ] **Step 6: Visually verify smooth scroll**

Launch QEMU, enter enough text to require scrolling, use arrow keys to scroll past the viewport. Verify the scroll animates smoothly (not instant jump). Take timed screenshots.

- [ ] **Step 7: Commit**

```
feat: smooth scroll with spring physics animation
```

---

## Task 8: Animated Cursor Blink

**Files:**
- Modify: `system/services/core/main.rs` — add cursor blink using Timeline
- Modify: `system/services/core/layout/mod.rs` — accept cursor_opacity parameter
- Modify: `system/services/core/layout/full.rs` — apply cursor_opacity to cursor node
- Modify: `system/services/core/layout/incremental.rs` — apply cursor_opacity

- [ ] **Step 1: Add Timeline and blink state to CoreState**

```rust
use animation::{Timeline, AnimationId, Easing};

// In CoreState:
timeline: Timeline,
cursor_blink_id: Option<AnimationId>,
cursor_opacity: u8,  // 0-255, passed to scene builder
```

- [ ] **Step 2: Implement cursor blink as a 4-phase state machine**

```rust
#[derive(Clone, Copy, PartialEq)]
enum BlinkPhase {
    VisibleHold,  // 500ms at opacity 255
    FadeOut,      // 150ms from 255→0
    HiddenHold,   // 300ms at opacity 0
    FadeIn,       // 150ms from 0→255
}
```

In CoreState, add:
```rust
blink_phase: BlinkPhase,
blink_phase_start_ms: u64,
cursor_blink_id: Option<AnimationId>,
cursor_opacity: u8,
```

Each tick of the event loop checks the blink state:
```rust
fn advance_blink(state: &mut CoreState, timeline: &mut Timeline, now_ms: u64) {
    let elapsed = now_ms - state.blink_phase_start_ms;
    match state.blink_phase {
        BlinkPhase::VisibleHold => {
            state.cursor_opacity = 255;
            if elapsed >= 500 {
                // Start fade-out animation
                state.cursor_blink_id = timeline.start(255.0, 0.0, 150, Easing::EaseInOut, now_ms).ok();
                state.blink_phase = BlinkPhase::FadeOut;
                state.blink_phase_start_ms = now_ms;
            }
        }
        BlinkPhase::FadeOut => {
            if let Some(id) = state.cursor_blink_id {
                state.cursor_opacity = if timeline.is_active(id) { timeline.value(id) as u8 } else { 0 };
            }
            if elapsed >= 150 {
                state.blink_phase = BlinkPhase::HiddenHold;
                state.blink_phase_start_ms = now_ms;
                state.cursor_opacity = 0;
            }
        }
        BlinkPhase::HiddenHold => {
            state.cursor_opacity = 0;
            if elapsed >= 300 {
                state.cursor_blink_id = timeline.start(0.0, 255.0, 150, Easing::EaseInOut, now_ms).ok();
                state.blink_phase = BlinkPhase::FadeIn;
                state.blink_phase_start_ms = now_ms;
            }
        }
        BlinkPhase::FadeIn => {
            if let Some(id) = state.cursor_blink_id {
                state.cursor_opacity = if timeline.is_active(id) { timeline.value(id) as u8 } else { 255 };
            }
            if elapsed >= 150 {
                state.blink_phase = BlinkPhase::VisibleHold;
                state.blink_phase_start_ms = now_ms;
                state.cursor_opacity = 255;
            }
        }
    }
}
```

Reset function (called on any user input):
```rust
fn reset_blink(state: &mut CoreState, timeline: &mut Timeline, now_ms: u64) {
    if let Some(id) = state.cursor_blink_id { timeline.cancel(id); }
    state.blink_phase = BlinkPhase::VisibleHold;
    state.blink_phase_start_ms = now_ms;
    state.cursor_opacity = 255;
}
```

- [ ] **Step 3: Integrate blink into event loop**

After the animation tick section:
```rust
// Tick the timeline
let now_ms = sys::counter() * 1000 / sys::counter_freq();
s.timeline.tick(now_ms);

// Update cursor opacity from blink animation
if let Some(blink_id) = s.cursor_blink_id {
    if s.timeline.is_active(blink_id) {
        s.cursor_opacity = s.timeline.value(blink_id) as u8;
        changed = true;
    } else {
        // Animation complete — start next phase
        // If was fading out (→0), hold dark then fade in
        // If was fading in (→255), hold visible then fade out
        // Implement as state machine or alternating animations
        s.cursor_blink_id = start_next_blink_phase(&mut s.timeline, now_ms, s.cursor_opacity);
    }
}
```

- [ ] **Step 4: Reset blink on user input**

Any keystroke or click resets the cursor to fully visible:
```rust
// In key event handler and click handler:
if let Some(id) = s.cursor_blink_id {
    s.timeline.cancel(id);
}
s.cursor_opacity = 255;
s.cursor_blink_id = start_cursor_blink(&mut s.timeline, now_ms);
```

- [ ] **Step 5: Pass cursor_opacity through to layout**

Add `cursor_opacity: u8` parameter to `build_editor_scene`, `build_full_scene`, `build_cursor_update`, etc. Apply it to the cursor node:
```rust
w.node_mut(N_CURSOR).opacity = cursor_opacity;
```

- [ ] **Step 6: Visually verify cursor blink**

Launch QEMU, wait for cursor to start blinking. Verify smooth fade (not abrupt toggle). Type a character — cursor should reset to fully visible. Take a sequence of screenshots at ~200ms intervals to verify opacity changes.

- [ ] **Step 7: Commit**

```
feat: animated cursor blink with smooth fade
```

---

## Task 9: Animated Transitions

**Files:**
- Modify: `system/services/core/main.rs` — fade transitions for document switching, spring scroll

Spring scroll is already done (Task 7). This task adds:
- Selection highlight fade-in
- Document switch fade (Ctrl+Tab)

- [ ] **Step 1: Selection highlight fade-in**

When selection changes, animate selection node opacity from 0→255 over 100ms:
```rust
// In the selection change handler:
s.selection_anim_id = s.timeline.start(0.0, 255.0, 100, Easing::EaseOut, now_ms).ok();
```

Apply selection opacity in layout:
```rust
// Selection nodes get opacity from animation
let sel_opacity = if let Some(id) = s.selection_anim_id {
    if s.timeline.is_active(id) { s.timeline.value(id) as u8 } else { 255 }
} else { 255 };
```

- [ ] **Step 2: Document switch fade**

On Ctrl+Tab, fade root opacity 255→0, switch mode, fade 0→255:
```rust
// When context_switched:
s.fade_out_id = s.timeline.start(255.0, 0.0, 120, Easing::EaseOut, now_ms).ok();
// After fade-out completes, rebuild scene in new mode, then fade in:
s.fade_in_id = s.timeline.start(0.0, 255.0, 120, Easing::EaseIn, now_ms + 120).ok();
```

Apply to root node opacity in the scene build.

- [ ] **Step 3: Visually verify transitions**

Test Ctrl+Tab fade. Test selection animation. Take screenshots.

- [ ] **Step 4: Commit**

```
feat: animated transitions — selection fade-in, document switch fade
```

---

## Task 10: Phase 1 Demo Scenes

**Files:**
- Modify: `system/services/core/test_gen.rs` — add animation demo content

- [ ] **Step 1: Add bouncing ball demo**

In the test content generator (the area that renders demo content in the top-right), add a small animated circle that bounces using the spring physics:

```rust
// A small colored square that animates position using spring physics
// Spring target alternates between two positions every 2 seconds
```

This demonstrates the animation system is working and visible.

- [ ] **Step 2: Add easing curve sampler**

Add a row of small rectangles, each animating with a different easing function. They all start at the same time and move the same distance, showing the different motion curves side by side.

- [ ] **Step 3: Visually verify demos**

Launch QEMU, verify the animation demos are visible in the demo area. Take screenshots at different times to show the animations in progress.

- [ ] **Step 4: Commit**

```
feat: Phase 1 demo scenes — bouncing ball, easing sampler
```

---

## Task 11: Phase 1 Final Verification

- [ ] **Step 1: Run full test suite**

```
cd system/test && cargo test -- --test-threads=1
```

All existing tests + new animation tests must pass.

- [ ] **Step 2: Visual verification session**

Launch QEMU, verify all Phase 1 features:
1. Smooth scroll (arrow keys past viewport) — spring animation, not instant jump
2. Cursor blink — smooth fade in/out
3. Selection fade-in (Shift+arrow to select text)
4. Demo scenes animating
5. Overall 60fps feel (no visible stuttering)

Take screenshots for each, verify in the Read tool.

- [ ] **Step 3: Update CLAUDE.md "Where We Left Off"**

Document Phase 1 completion, test counts, what's ready for Phase 2.

- [ ] **Step 4: Commit**

```
chore: Phase 1 (Motion) complete — animation library, smooth scroll, cursor blink, transitions
```
