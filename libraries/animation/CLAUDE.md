# animation

General-purpose animation engine. Provides easing curves, spring physics, and a timeline-based animation manager. Pure `no_std`, no dependencies, no `alloc`.

## Key Files

- `lib.rs` -- All types in one file: `Easing` (24 curves including CSS standard + elastic/bounce), `Spring` (physics sim with 4 presets, 4ms fixed substeps), `Timeline` (32-slot animation manager), `Animated<T>` (type-generic wrapper), `Lerp` trait (f32, i32, u8, [u8;4] with gamma-correct sRGB, Transform2D)

## Dependencies

- None

## Conventions

- All math is `f32` with custom `sin`/`exp2`/cubic-bezier approximations (< 0.0002 error)
- Back/Elastic/Bounce easings intentionally overshoot `[0, 1]`
- Spring uses semi-implicit Euler with 4ms fixed substeps to avoid divergence at large dt
