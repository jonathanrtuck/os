# fonts

Text shaping and glyph rasterization. Wraps HarfRust for OpenType shaping (font data + text + features -> shaped glyph array) and provides a custom analytic-coverage rasterizer for glyph outlines. `no_std` with `alloc`.

## Key Files

- `src/lib.rs` -- `shape()` function (HarfRust wrapper), `ShapedGlyph` type. All positions in font units.
- `src/cache.rs` -- `GlyphCache` (fixed 95-slot ASCII cache, ~238 KiB) and `LruGlyphCache` (BTreeMap-based, bounded capacity) for pre-rasterized coverage maps
- `src/rasterize/mod.rs` -- Public API re-exports for the rasterizer subsystem
- `src/rasterize/scanline.rs` -- Analytic area coverage rasterizer (exact signed-area trapezoids, integer/fixed-point math)
- `src/rasterize/outline.rs` -- `GlyphOutline` extraction from font data via read-fonts
- `src/rasterize/scale.rs` -- Font-unit to pixel scaling helpers
- `src/rasterize/metrics.rs` -- `FontMetrics`, `GlyphMetrics`, `glyph_id_for_char`, axis enumeration
- `src/rasterize/embolden.rs` -- Outline dilation for stem darkening (symmetric miter-join, macOS-style coefficients)
- `src/rasterize/gvar.rs` -- Variable font axis support (`rasterize_with_axes`)
- `src/rasterize/optical.rs` -- Optical size and weight correction for variable fonts

## Dependencies

- `harfrust` (no_std) -- OpenType text shaping
- `read-fonts` (no_std) -- Font table parsing and glyph outline extraction

## Conventions

- All position values from shaping are in font units; callers scale via `pixel = font_unit * desired_px / upem`
- Rasterizer output is 1 byte per pixel (grayscale coverage), no subpixel/LCD rendering
- Rasterizer uses integer/fixed-point math only, no floating point
- Glyph cache keys include `(glyph_id, font_size, axis_hash)` for variable font support
