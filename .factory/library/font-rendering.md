# Font Rendering Pipeline

Knowledge base for the font rendering mission. HarfRust API, integration patterns, variable font details.

## HarfRust API Reference

**Crate:** `harfrust` v0.5.2 (pure Rust port of HarfBuzz v13.0.0)
**License:** MIT
**no_std:** `harfrust = { version = "0.5", default-features = false }` — uses `alloc`, NOT `std`
**Dependencies:** read-fonts, bitflags, bytemuck, core_maths, smallvec (all no_std compatible)

### Font Creation

```rust
use harfrust::{FontRef, ShaperData};

// FontRef from raw bytes (zero-copy)
let font = FontRef::from_index(&font_data, 0).unwrap(); // font_data: &[u8]

// ShaperData caches parsed tables — create once per font, reuse
let data = ShaperData::new(&font);
```

### Text Shaping

```rust
use harfrust::UnicodeBuffer;

let mut buffer = UnicodeBuffer::new();
buffer.push_str("Hello World");
buffer.guess_segment_properties(); // auto-detect direction, script, language

// Build shaper (reuse for same font config)
let shaper = data.shaper(&font).build();

// Shape — consumes buffer, returns GlyphBuffer
let glyph_buffer = shaper.shape(buffer, &[]);
```

### Reading Output

```rust
// Parallel arrays: glyph_infos() and glyph_positions()
for (info, pos) in glyph_buffer.glyph_infos().iter()
    .zip(glyph_buffer.glyph_positions().iter())
{
    let glyph_id = info.as_glyph();   // u16 glyph ID
    let cluster = info.cluster;         // original character cluster index
    let x_advance = pos.x_advance;     // i32, in FONT UNITS (not pixels!)
    let y_advance = pos.y_advance;     // i32
    let x_offset = pos.x_offset;       // i32, positioning offset
    let y_offset = pos.y_offset;       // i32
}

// IMPORTANT: All values are in font units (UnitsPerEm).
// Scale to pixels: pixel_value = font_unit_value * desired_px_size / units_per_em
```

### OpenType Feature Control

```rust
use harfrust::Feature;

let features = vec![
    Feature::from_str("+liga").unwrap(),    // Enable ligatures
    Feature::from_str("-kern").unwrap(),    // Disable kerning
    Feature::from_str("+tnum").unwrap(),    // Tabular numbers
    Feature::from_str("+onum").unwrap(),    // Oldstyle figures
    Feature::from_str("+smcp").unwrap(),    // Small caps
];
let glyph_buffer = shaper.shape(buffer, &features);
```

### Variable Font Support

```rust
use harfrust::{Variation, ShaperInstance};

let variations = vec![
    Variation::from_str("wght=600").unwrap(),
    Variation::from_str("opsz=10").unwrap(),
];
let instance = ShaperInstance::from_variations(&font, &variations);
let shaper = data.shaper(&font)
    .instance(Some(&instance))
    .build();
```

### Performance Notes

- **Reuse ShaperData** — expensive to create (parses all shaping tables)
- **Reuse Shaper** — for same font + feature configuration
- **UnicodeBuffer is consumed** by shape() — create new buffer for each shaping call
- 75-100% of C HarfBuzz speed (Latin ~90-95%, complex scripts ~75-85%)

## read-fonts for Glyph Outlines

HarfRust depends on `read-fonts` (from Google's fontations project). This same crate provides glyph outline extraction for the rasterizer.

```rust
use read_fonts::{FontRef, TableProvider};
use read_fonts::tables::glyf::Glyf;
use read_fonts::tables::loca::Loca;

// Access glyph outlines
let font = read_fonts::FontRef::new(&font_data).unwrap();
// Use skrifa (higher-level API from fontations) for outline scaling:
// skrifa::instance::LocationRef, skrifa::outline::DrawSettings, etc.
```

**For variable fonts:** read-fonts + skrifa handle glyph interpolation at arbitrary axis positions. The rasterizer receives already-interpolated outlines.

**NOTE:** read-fonts and skrifa both support no_std + alloc. Check latest docs for exact API — the fontations project evolves actively.

## Font Files

| Font | File | Axes | Size |
|------|------|------|------|
| Variable Nunito Sans | `nunito-sans-variable.ttf` | opsz, wght, wdth, YTLC | 556 KB |
| Variable Nunito Sans Italic | `nunito-sans-variable-italic.ttf` | opsz, wght, wdth, YTLC | 543 KB |
| Variable Source Code Pro | `source-code-pro-variable.ttf` | wght | ~300 KB |
| Source Code Pro (static, legacy) | `source-code-pro.ttf` | none | 9 KB |
| Nunito Sans (static, legacy) | `nunito-sans.ttf` | none | 138 KB |

Variable fonts go in `system/share/`. Legacy static fonts can be removed once variable versions are wired through.

## Architecture: Where Shaping Lives

```
Core Service (document semantics)
  │  uses shaping library
  │  shape(font_data, text, features) → Vec<ShapedGlyph>
  ▼
Scene Graph (interface)
  │  TextRun carries ShapedGlyph arrays
  │  (glyph_id, x_advance, y_advance, x_offset, y_offset)
  ▼
Compositor (pixels)
  │  rasterize(font_data, glyph_id, size) → coverage map
  │  glyph cache: (glyph_id, size) → cached coverage
  │  draw_coverage() → pixels on framebuffer
  ▼
Display
```

**Shaping = document semantics** (which glyphs, in what order, with what positions).
**Rasterization = pixel production** (glyph outline → coverage map).
**Compositing = blending** (coverage map → framebuffer pixels, already gamma-correct).

## Perceptual Rendering Formulas

### Optical Size

```
opsz_value = font_size_px  // for screen-optimized fonts
// or: opsz_value = font_size_px * 72.0 / dpi  // for traditional point-size mapping
```

Clamp to font's opsz axis range (e.g., Nunito Sans opsz 6–12).

### Weight Correction for Dark Mode

```
// Relative luminance (sRGB)
fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);
    0.2126 * rl + 0.7152 * gl + 0.0722 * bl
}

// Contrast ratio
let fg_lum = relative_luminance(fg);
let bg_lum = relative_luminance(bg);
let contrast = (max(fg_lum, bg_lum) + 0.05) / (min(fg_lum, bg_lum) + 0.05);

// Weight correction (only when fg is lighter than bg)
if fg_lum > bg_lum {
    let reduction = (contrast - 1.0) / 20.0; // 0.0 to ~1.0
    let correction = 1.0 - reduction.clamp(0.0, 0.15); // max 15% reduction
    adjusted_weight = base_weight * correction;
}
```

Clamp adjusted_weight to font's wght axis range.

## Build System Integration

The bare-metal build (`system/build.rs`) compiles libraries via direct `rustc` invocations. For libraries with Cargo dependency trees (harfrust), the build needs to use `cargo build --target aarch64-unknown-none` to resolve dependencies.

**Approach:** Create the shaping library as a standard Cargo package with its own Cargo.toml. In build.rs, compile it via cargo for the bare-metal target, then link the resulting rlib alongside the existing manually-compiled libraries.

**Key files:**
- `system/build.rs` — the build orchestrator
- `system/libraries/shaping/Cargo.toml` — new, with harfrust dependency
- `system/libraries/shaping/src/lib.rs` — new, shaping API
- `system/test/Cargo.toml` — add shaping as dev-dependency for host tests
