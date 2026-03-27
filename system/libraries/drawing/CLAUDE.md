# drawing

Pixel buffer drawing primitives. Operates on borrowed `Surface` buffers with BGRA8888 or RGBA8888 pixel formats. All blending is sRGB gamma-correct (linear light) using precomputed lookup tables. Pure `no_std`, no dependencies.

## Key Files

- `lib.rs` -- `Surface`, `Color`, `PixelFormat`, public API re-exports. Includes `gamma_tables.rs`, `palette.rs`, and `neon.rs` via `include!`
- `blend.rs` -- Porter-Duff source-over compositing in linear light, integer math only
- `blit.rs` -- Copy and alpha-blend source buffers onto surfaces
- `blur.rs` -- Gaussian blur with separable kernel (box blur fast path)
- `box_blur.rs` -- 3-pass box blur approximation of Gaussian blur
- `coverage.rs` -- Draws 1bpp grayscale glyph coverage maps with sRGB blending
- `fill.rs` -- Solid and blended rectangle fills, including rounded rectangles with AA
- `gradient.rs` -- Radial gradient rendering with Bayer 4x4 ordered dithering
- `line.rs` -- Line drawing
- `transform.rs` -- Affine-transformed blits with bilinear interpolation
- `gamma_tables.rs` -- `SRGB_TO_LINEAR` and `LINEAR_TO_SRGB` lookup tables (textual include)
- `palette.rs` -- Named color constants (textual include)
- `neon.rs` -- NEON SIMD acceleration for aarch64 (textual include, cfg-gated)

## Dependencies

- None

## Conventions

- All coordinates clip to surface bounds silently (no panics on out-of-range)
- Colors are always in RGBA order externally; converted to target pixel format at write time
- Blending uses integer math with `div255` helper, never floating point
- NEON acceleration is compile-time gated on `target_arch = "aarch64"`
