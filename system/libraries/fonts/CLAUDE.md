# fonts

Text shaping, font metrics, and glyph rasterization. Wraps HarfRust for OpenType shaping (font data + text + features -> shaped glyph array) and provides a custom analytic-coverage rasterizer for glyph outlines. `no_std` with `alloc`.

## Module Structure

Two public modules with distinct coordinate concerns:

- **`metrics`** — Font-unit API. Used by layout, presenter, and any code above the render boundary. No pixel concepts. Types: `FontMetrics`, `FontAxis`, `AxisValue`. Functions: `font_metrics`, `caret_skew`, `glyph_id_for_char`, `glyph_h_metrics`, `glyph_h_advance_with_axes`, `font_axes`, `axis_values_hash`, `compute_optical_size`, `auto_axis_values_for_opsz`, `weight_correction_factor`, `auto_weight_correction_axes`.
- **`rasterize`** — Pixel API. Used only by render services (metal-render). Types: `GlyphMetrics`, `RasterBuffer`, `RasterScratch`. Functions: `rasterize`, `rasterize_with_axes`, `compute_dilation`, `embolden_outline`.

## Key Files

- `src/lib.rs` -- `shape()`, `shape_with_variations()`, `ShapedGlyph`. All positions in font units.
- `src/metrics.rs` -- Font-unit metrics, axis types, optical sizing, weight correction
- `src/cache.rs` -- `GlyphCache` (fixed 95-slot ASCII cache) and `LruGlyphCache` (BTreeMap-based). Legacy: no runtime consumers after cpu-render removal.
- `src/rasterize/mod.rs` -- Re-exports pixel types and rasterization functions
- `src/rasterize/scanline.rs` -- Analytic area coverage rasterizer (exact signed-area trapezoids, integer/fixed-point math)
- `src/rasterize/outline.rs` -- `GlyphOutline` extraction from font data via read-fonts
- `src/rasterize/scale.rs` -- Font-unit to pixel scaling helpers (crate-internal)
- `src/rasterize/metrics.rs` -- Pixel output types: `GlyphMetrics`, `RasterBuffer`
- `src/rasterize/embolden.rs` -- Outline dilation for stem darkening (symmetric miter-join, macOS-style coefficients)
- `src/rasterize/gvar.rs` -- Variable font axis support (`rasterize_with_axes`)
- `src/rasterize/hvar.rs` -- HVAR table: variation-aware advance widths

## Dependencies

- `harfrust` (no_std) -- OpenType text shaping
- `read-fonts` (no_std) -- Font table parsing and glyph outline extraction

## Conventions

- All position values from shaping are in font units; callers scale via `pixel = font_unit * desired_px / upem`
- Layout/presenter import `fonts::metrics` — never `fonts::rasterize`
- Render services import `fonts::rasterize` for rasterization + `fonts::metrics::AxisValue` for axis types
- Rasterizer output is 1 byte per pixel (grayscale coverage), no subpixel/LCD rendering
- Rasterizer uses integer/fixed-point math only, no floating point
