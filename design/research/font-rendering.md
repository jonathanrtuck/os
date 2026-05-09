# Font Rendering: Research & Design

## Current State (Updated 2026-03-23)

All phases of the font rendering pipeline are **complete**, including a quality
sprint that achieves macOS Core Text-level rendering for Latin text.

**Fonts:** Three-font stack selected by content type, loaded from host via 9p:
JetBrains Mono (monospace — editor/code), Inter (sans-serif — chrome/UI), Source
Serif 4 (serif — prose/body). All variable fonts with weight axes.

**Font library** (`userspace/libraries/fonts/`): Rasterizer (`src/rasterize/`)
with modular pipeline: outline extraction (read-fonts), gvar delta
interpolation, scanline sweep, analytic area coverage (exact signed-area
trapezoids), outline dilation via symmetric miter-join for stem darkening (macOS
formula, Pathfinder coefficients × 1.3 boost), optical sizing. Device-pixel
rasterization: atlas rendered at `font_size_pt × scale_factor` for crisp output
at native display resolution. Glyph cache (`src/cache.rs`): LRU keyed by
(glyph_id, font_size, axis_hash), bounded memory. On-demand atlas upload in
metal-render (fixes ligature drops from fixed-size atlases).

**Shaping:** Wraps HarfRust (pure Rust port of HarfBuzz, no_std+alloc).
`ShapedGlyph` is 16 bytes with 16.16 fixed-point advances for subpixel glyph
positioning. Eliminates cursor drift from float truncation. Single `char_w_fx`
source of truth across layout and rendering.

**Scene graph** (`userspace/libraries/scene/lib.rs`): TextRun carries
`ShapedGlyph` arrays via DataRef in shared memory. Core writes shaped glyph
data; render services read and rasterize. Cross-process safe with compile-time
size assertion.

**Text layout** (`userspace/libraries/layout/`): Unified `layout_paragraph()`
for both monospace and proportional text, parameterized by `FontMetrics` trait.
`CharBreaker` (character-level wrapping) and `WordBreaker` (word-boundary
wrapping). Standalone `byte_to_line_col()` for cursor positioning.

**Compositing:** `draw_coverage` blends coverage maps with sRGB→linear→sRGB
gamma-correct blending (LUT-based). Stem darkening applied via outline dilation
in the rasterizer (not post-rasterization).

**Font quality sprint (5 changes to match macOS Core Text):**

1. Outline dilation via symmetric miter-join (macOS formula, Pathfinder
   coefficients × 1.3 boost)
2. Analytic area coverage rasterizer (exact signed-area trapezoids, not
   quantized)
3. Device-pixel rasterization (atlas at `font_size_pt × scale_factor`)
4. Subpixel glyph positioning (ShapedGlyph widened 8→16 bytes, 16.16 fixed-point
   advances)
5. Single `char_w_fx` source of truth (eliminates cursor drift from truncation)

### What Remains (Future Work)

1. **Complex scripts** — Arabic, Devanagari, CJK handled by HarfRust but
   untested end-to-end
2. **Hinting** — no hint interpretation or auto-hinting (unnecessary for HiDPI
   target)
3. **GPU-native text rendering** — research frontier, see below
4. **Environment-adaptive rendering** — display-aware, ambient light adaptation

---

## Display Target

**Primary: HiDPI (≥2× device pixel ratio).** Retina and equivalent displays are
the present and future for the target audience (personal workstation). All novel
rendering work (gamma correction, optical sizing, weight correction) benefits
HiDPI most.

**The rendering pipeline should be resolution-aware, not resolution-specific.**
The compositor knows the display's physical DPI (from EDID or configuration).
Rendering decisions key off this:

| DPI Range | Hinting              | Anti-aliasing | Subpixel |
| --------- | -------------------- | ------------- | -------- |
| ≥192 (2×) | None                 | Grayscale     | Off      |
| 144–191   | Slight vertical only | Grayscale     | Optional |
| <144      | Slight vertical only | Grayscale     | On (RGB) |

The existing 6× horizontal oversampling already supports the subpixel path. For
HiDPI, the rasterizer can simplify to vertical oversampling only (4× vertical,
1× horizontal), saving ~6× in coverage buffer size per glyph and simplifying
compositing.

---

## How Modern OSes Render Text

### The Pipeline (Universal)

1. **Font parsing** — read TrueType/OpenType tables
2. **Text shaping** — Unicode codepoints → positioned glyphs (ligatures,
   kerning, complex scripts)
3. **Rasterization** — Bezier outlines → pixel coverage
4. **Hinting** — adjust outlines to align with pixel grid
5. **Anti-aliasing** — smooth edges via coverage values
6. **Compositing** — blend rendered glyphs onto background

### macOS (Core Text + Core Graphics)

Philosophy: **trust the type designer, don't distort the shapes.**

- No grid-fitting at typical sizes. Ignores TrueType hinting instructions. Glyph
  stems land wherever the outline says.
- Heavy anti-aliasing to compensate. Historically LCD subpixel smoothing;
  grayscale-only since Mojave (2018) — subpixel dropped because Retina makes it
  unnecessary.
- Fractional glyph positioning — sub-pixel x-coordinates preserve designer's
  spacing exactly.
- Result: Faithfully reproduces typeface design. On Retina, gorgeous. On
  non-Retina external monitors, appears soft/blurry to some.

### Windows (DirectWrite + ClearType)

Philosophy: **make text sharp on the pixel grid, even at the cost of distorting
shapes.**

- Aggressive hinting. ClearType snaps stems to pixel boundaries.
- Subpixel rendering exploits LCD subpixel structure for ~3× horizontal
  resolution.
- Tradeoff: Letter spacing is distorted — stems snapped to pixels means uneven
  rhythm. Designer's metrics are compromised for screen crispness.
- DirectWrite improved with subpixel positioning and grayscale mode, but default
  is still snap-to-grid.
- Result: Crisp at low DPI (where most Windows monitors historically lived). On
  HiDPI, aggressive hinting is unnecessary and slightly harmful.

### Linux (FreeType + HarfBuzz + fontconfig)

Most configurable, and the stack everyone else borrows from:

- **FreeType** — open-source rasterizer, extremely mature. Used by Android,
  Chrome, Firefox.
- **HarfBuzz** — open-source text shaper. The industry standard. Used by Chrome,
  Firefox, Android, GNOME, Qt, LibreOffice, Adobe products.
- **fontconfig** — font matching and configuration.
- Modern default: **"slight" hinting** — vertical stems only, horizontal spacing
  preserved. Good middle ground.
- Result: Modern defaults look quite good. Highly configurable.

### Key Takeaway

macOS's philosophy (trust the outlines, don't distort) is correct for HiDPI and
aligns with our design values. The opportunity is to go further: trust the
outlines AND adapt rendering to perceptual context.

---

## Improvement Plan

### Phase 1: Text Shaping ✅ COMPLETE

**Goal:** Correct glyph positioning, ligatures, kerning, complex script support.

**Implemented:** Integrated HarfRust (pure Rust port of HarfBuzz v13.0.0,
no_std+alloc) instead of HarfBuzz C/C++. No FFI needed — pure Rust dependency
compiled for bare-metal target via Cargo.

**Deviation from plan:** Used HarfRust instead of HarfBuzz. HarfRust is the
official Rust port by the HarfBuzz project, achieving 75-95% of C HarfBuzz
speed. Eliminated the FFI porting layer entirely. The custom TrueType parser
(cmap lookup, GPOS kerning) was removed per "kill the old way." The scanline
rasterizer algorithm was preserved, only its input source changed (read-fonts
outlines instead of custom parser).

**What was built:**

- `userspace/libraries/shaping/` — new Cargo crate with HarfRust dependency
- `userspace/libraries/shaping/src/rasterize.rs` — glyph rasterizer using
  read-fonts for outline extraction
- Scene graph evolved to carry ShapedGlyph arrays
- LRU glyph cache replaced fixed 95-slot ASCII cache
- Core service shapes text, compositor rasterizes from glyph IDs
- End-to-end: typed text → HarfRust shaping → scene graph → rasterized pixels

### Phase 2: Gamma-Correct Compositing ✅ COMPLETE (pre-existing)

**Goal:** Perceptually correct text weight regardless of foreground/background
combination.

**Status:** Already implemented before this mission began. `draw_coverage` and
`blend_over` in `userspace/libraries/drawing/lib.rs` already blend in linear
light space with sRGB↔linear LUTs. The research doc was stale on this point —
compositing was listed as "no gamma correction" but gamma-correct blending was
already in place.

### Phase 3: Unicode Coverage & Font Fallback ✅ COMPLETE

**Goal:** Render any Unicode codepoint, not just ASCII.

**Implemented:**

- LRU glyph cache with configurable capacity, keyed by (glyph_id, font_size,
  axis_hash). Supports arbitrary Unicode codepoints and variable font axis
  differentiation.
- Font fallback chain (`userspace/libraries/shaping/src/fallback.rs`): ordered
  list of font data references. When primary font produces .notdef, subsequent
  fonts are tried. Content-type-aware: Code → monospace primary + proportional
  fallback; Prose/UI → proportional primary + monospace fallback.
- Latin Extended codepoints (é, ñ, ü) render correctly. Supplementary plane
  codepoints don't crash.
- Cache key includes font identifier hash for fallback chain separation.

### Phase 4: Variable Font Support ✅ COMPLETE

**Goal:** Parse and render variable fonts, enabling optical sizing and weight
correction.

**Implemented:** Used read-fonts (from Google's fontations project, already a
transitive dependency of HarfRust) for fvar/gvar/avar table parsing and glyph
outline interpolation. No custom table parsing needed.

**Deviation from plan:** Did not extend the custom `truetype.rs` parser.
Instead, read-fonts handles all variable font table parsing. The custom parser
was removed in Phase 1 (per "kill the old way"). Variable font axis values flow
from core service → scene graph (TextRun.axis_hash) → compositor
(rasterize_with_axes).

**Fonts:** Variable Nunito Sans (opsz, wght, wdth, YTLC axes, 556 KB) and
Variable Source Code Pro (wght axis, ~300 KB) in assets/.

### Phase 5a: Automatic Optical Sizing ✅ COMPLETE

**Goal:** Text looks correct at every size without user intervention.

**Implemented:** `auto_axis_values_for_opsz(font_data, font_size_px, dpi)`
computes the opsz axis value from rendered pixel size using the traditional
typographic formula (opsz = font_size_px × 72 / dpi), clamped to the font's
declared opsz range. Returns empty for fonts without an opsz axis (no-op).

**Deviation from plan:** Variable Nunito Sans DOES have an opsz axis (range
6–12), contrary to the original research note. This was discovered when variable
font files were integrated. No font substitution was needed.

### Phase 5b: Perceptual Dark Mode Weight Correction ✅ COMPLETE

**Goal:** Same perceived text weight regardless of background luminance.

**Implemented:** `weight_correction_factor(fg_rgb, bg_rgb)` computes a
continuous correction factor from WCAG contrast ratio. Light-on-dark → factor <
1.0 (max 15% reduction at 21:1 contrast). Dark-on-light → factor = 1.0 (no
change). `auto_weight_correction_axes(font_data, fg_rgb, bg_rgb)` applies the
correction to the font's wght axis default, returning an AxisValue clamped to
the font's declared range. Empty for fonts without a wght axis (no-op).

**Implementation matches plan:** sRGB→linear LUT (256 entries, compile-time
computed), WCAG relative luminance, contrast ratio, continuous proportional
correction. No binary light/dark switch.

**No production OS does this today.** This is a novel contribution of this
project.

### Phase 5c: Content-Type-Aware Typography ✅ COMPLETE

**Goal:** Text rendering defaults that match the content's purpose.

**Implemented:** `TypographyConfig` struct in
`userspace/libraries/shaping/src/typography.rs` maps content types to font
family, OpenType features, weight preference, tracking, and optical sizing flag.

| Content Type | Font         | Features     | Weight | Optical Sizing | Notes                                                |
| ------------ | ------------ | ------------ | ------ | -------------- | ---------------------------------------------------- |
| Code         | Monospace    | +calt, +tnum | 400    | No             | Contextual alternates for ligatures, tabular figures |
| Prose        | Proportional | +onum        | 400    | Yes            | Oldstyle figures, auto-opsz                          |
| UI           | Proportional | (none)       | 500    | No             | Medium weight for functional labels                  |
| Unknown      | Proportional | +onum        | 400    | Yes            | Falls back to prose defaults                         |

**ContentType enum** in `fallback.rs` extended with `Ui` and `Unknown` variants.
FallbackChain handles all four content types.
`TypographyConfig::for_content_type()` returns complete configuration for any
content type without panic.

**Pipeline wiring:** Content type influences font selection and OpenType
features. Shaping "1/2 != 0.5" as code (monospace + calt + tnum) vs prose
(proportional + onum) produces different glyph output. Editors can override
these defaults.

**No other OS has this capability** — it requires native content-type
understanding (our settled decision #5).

---

## Build vs. Integrate (Outcomes)

| Component                              | Decision                | Outcome                                                                                                                                               |
| -------------------------------------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Text shaping**                       | Integrated (HarfRust)   | Used HarfRust (pure Rust port of HarfBuzz) instead of C HarfBuzz. No FFI needed — pure no_std+alloc Cargo dependency. 75-95% of C speed.              |
| **Rasterization**                      | Kept ours               | Scanline rasterizer algorithm preserved. Input source changed from custom parser to read-fonts. Fixed-point math, subpixel rendering intact.          |
| **Compositing**                        | Kept ours               | Gamma-correct blending was already in place. Compositor reads shaped glyph IDs from scene graph, rasterizes via adapted rasterizer + LRU glyph cache. |
| **Variable fonts**                     | Integrated (read-fonts) | Used read-fonts (transitive dep of HarfRust) for fvar/gvar parsing. Custom truetype.rs parser removed ("kill the old way").                           |
| **Optical sizing / weight correction** | Built (novel)           | Automatic optical sizing and continuous weight correction — our novel contributions. No production OS does these.                                     |
| **Font fallback**                      | Built                   | Content-type-aware fallback chains. Tightly coupled to our content-type model. Simple algorithm, custom policy.                                       |
| **Typography defaults**                | Built (novel)           | Content-type-aware TypographyConfig. Unique to our architecture — requires native content-type understanding.                                         |

---

## Research Frontiers (Longer-Term)

### GPU-Native Text Rendering

Current pipeline: CPU rasterizes glyphs → caches bitmaps in texture atlas → GPU
composites. The atlas has a fixed resolution; zooming requires re-rasterization.

Alternatives under active research:

- **Slug** (Eric Lengyel, SIGGRAPH) — renders Bezier curves directly on GPU.
  Resolution-independent. Proprietary.
- **Pathfinder / Vello** (Rust) — GPU 2D vector rendering, open source. Can
  render font outlines directly.
- **Multi-channel Signed Distance Fields (MSDF)** — store distance-to-edge per
  channel, render on GPU. Scales beautifully. Used extensively in games (Valve
  popularized basic SDF). Not perfect below ~16px but excellent above.

Relevance: the scene graph architecture (compositor renders from typed visual
nodes in shared memory) could naturally express glyphs as outlines or SDF
textures rather than pre-rasterized bitmaps. No production OS does GPU-native
text rendering for its entire UI — this is frontier territory.

### Environment-Adaptive Rendering

No OS adapts font rendering to viewing conditions:

- **Display-aware:** Tune rendering per-display using EDID data (subpixel
  layout, density, panel type). Adapt when windows move between displays.
- **Ambient light:** Increase contrast in bright environments, soften in dim
  (requires ambient light sensor data).
- Speculative, but the sensor data exists on most hardware.

### Neural/Learned Rendering

Research into using neural networks for anti-aliasing and hinting decisions.
Could produce resolution-independent rendering with learned perceptual
optimization. Very early stage but worth tracking.

---

## Implementation Sequence (Completed)

```text
Phase 1: Text shaping (HarfRust)            ✅ — biggest visual delta
Phase 2: Gamma-correct compositing          ✅ — pre-existing, already implemented
Phase 3: Unicode coverage + font fallback   ✅ — completeness
Phase 4: Variable font support              ✅ — enables phases 5a/5b
Phase 5a: Automatic optical sizing          ✅ — novel, quality
Phase 5b: Dark mode weight correction       ✅ — novel, quality
Phase 5c: Content-type-aware defaults       ✅ — novel, unique to our architecture
```

All phases completed across three milestones: text-shaping,
unicode-and-fallback, perceptual-rendering. Total: 1,477 host-side tests
passing.

---

## Resolved Questions

1. **HarfBuzz on bare metal:** ✅ Used HarfRust (pure Rust, no_std+alloc) — no
   porting surface needed. Shaping runs in the core service (OS service);
   positioned glyph arrays are passed to the compositor via the scene graph.
2. **Font selection:** ✅ Variable Nunito Sans has opsz+wght+wdth+YTLC axes.
   Variable Source Code Pro has wght axis. Both integrated into assets/. Nunito
   Sans's opsz axis (range 6–12) enables automatic optical sizing.
3. **Glyph cache architecture:** ✅ LRU eviction with configurable max capacity.
   Key: (glyph_id, font_size, axis_hash) where axis_hash combines font
   identifier and variable font axis values via FNV-1a.
4. **Subpixel rendering sunset:** Deferred — kept existing 6× horizontal
   oversampling. The HiDPI simplification (grayscale-only) is a future
   optimization, not blocking any functionality.
