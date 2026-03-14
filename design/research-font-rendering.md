# Font Rendering: Research & Design

## Current State

**Fonts:** Source Code Pro (monospace), Nunito Sans (proportional), loaded from host via 9p.

**TrueType parser** (`libraries/drawing/truetype.rs`, ~1400 lines): Zero-copy, no-alloc. Quadratic beziers from `glyf` table. GPOS kerning (pair adjustment). Codepoint→glyph mapping. Metrics from `hhea`/`hmtx`. Does NOT handle CFF/OpenType outlines or variable fonts.

**Rasterizer** (`libraries/drawing/rasterizer.rs`, ~480 lines): Scanline sweep with 4× vertical and 6× horizontal oversampling. 6× horizontal = 3 subpixels × 2× each, producing per-channel (R,G,B) subpixel coverage. Fixed-point math (20.12), no floats. Bezier flattening → line segments → active-edge sweep. Max 2048 segments, 64 active edges per scanline.

**Glyph cache** (`libraries/drawing/lib.rs`): Pre-rasterized ASCII (0x20–0x7E) per font/size pair. ~1.3 MiB per cache (95 glyphs × 13,824 bytes). Stores per-glyph metrics (width, height, bearing, advance).

**Text layout:** Monospace layout (char_width × column) and proportional layout with per-glyph advance. GPOS kerning applied between glyph pairs when font reference is provided.

**Compositing:** `draw_coverage` blends subpixel coverage maps onto framebuffer. No gamma correction — blending is in sRGB space.

### What's Missing (Ordered by Impact)

1. **Text shaping** — no ligatures, no complex scripts, no OpenType feature application
2. **Gamma-correct compositing** — blending in sRGB causes perceptual weight errors
3. **Variable font support** — no `fvar`/`gvar` parsing, no glyph interpolation
4. **Unicode coverage** — glyph cache is ASCII-only (0x20–0x7E)
5. **Font fallback** — no mechanism to substitute glyphs from alternate fonts
6. **Optical sizing** — no adaptation of font rendering to physical size
7. **Hinting** — no hint interpretation or auto-hinting

---

## Display Target

**Primary: HiDPI (≥2× device pixel ratio).** Retina and equivalent displays are the present and future for the target audience (personal workstation). All novel rendering work (gamma correction, optical sizing, weight correction) benefits HiDPI most.

**The rendering pipeline should be resolution-aware, not resolution-specific.** The compositor knows the display's physical DPI (from EDID or configuration). Rendering decisions key off this:

| DPI Range | Hinting              | Anti-aliasing | Subpixel |
| --------- | -------------------- | ------------- | -------- |
| ≥192 (2×) | None                 | Grayscale     | Off      |
| 144–191   | Slight vertical only | Grayscale     | Optional |
| <144      | Slight vertical only | Grayscale     | On (RGB) |

The existing 6× horizontal oversampling already supports the subpixel path. For HiDPI, the rasterizer can simplify to vertical oversampling only (4× vertical, 1× horizontal), saving ~6× in coverage buffer size per glyph and simplifying compositing.

---

## How Modern OSes Render Text

### The Pipeline (Universal)

1. **Font parsing** — read TrueType/OpenType tables
2. **Text shaping** — Unicode codepoints → positioned glyphs (ligatures, kerning, complex scripts)
3. **Rasterization** — Bezier outlines → pixel coverage
4. **Hinting** — adjust outlines to align with pixel grid
5. **Anti-aliasing** — smooth edges via coverage values
6. **Compositing** — blend rendered glyphs onto background

### macOS (Core Text + Core Graphics)

Philosophy: **trust the type designer, don't distort the shapes.**

- No grid-fitting at typical sizes. Ignores TrueType hinting instructions. Glyph stems land wherever the outline says.
- Heavy anti-aliasing to compensate. Historically LCD subpixel smoothing; grayscale-only since Mojave (2018) — subpixel dropped because Retina makes it unnecessary.
- Fractional glyph positioning — sub-pixel x-coordinates preserve designer's spacing exactly.
- Result: Faithfully reproduces typeface design. On Retina, gorgeous. On non-Retina external monitors, appears soft/blurry to some.

### Windows (DirectWrite + ClearType)

Philosophy: **make text sharp on the pixel grid, even at the cost of distorting shapes.**

- Aggressive hinting. ClearType snaps stems to pixel boundaries.
- Subpixel rendering exploits LCD subpixel structure for ~3× horizontal resolution.
- Tradeoff: Letter spacing is distorted — stems snapped to pixels means uneven rhythm. Designer's metrics are compromised for screen crispness.
- DirectWrite improved with subpixel positioning and grayscale mode, but default is still snap-to-grid.
- Result: Crisp at low DPI (where most Windows monitors historically lived). On HiDPI, aggressive hinting is unnecessary and slightly harmful.

### Linux (FreeType + HarfBuzz + fontconfig)

Most configurable, and the stack everyone else borrows from:

- **FreeType** — open-source rasterizer, extremely mature. Used by Android, Chrome, Firefox.
- **HarfBuzz** — open-source text shaper. The industry standard. Used by Chrome, Firefox, Android, GNOME, Qt, LibreOffice, Adobe products.
- **fontconfig** — font matching and configuration.
- Modern default: **"slight" hinting** — vertical stems only, horizontal spacing preserved. Good middle ground.
- Result: Modern defaults look quite good. Highly configurable.

### Key Takeaway

macOS's philosophy (trust the outlines, don't distort) is correct for HiDPI and aligns with our design values. The opportunity is to go further: trust the outlines AND adapt rendering to perceptual context.

---

## Improvement Plan

### Phase 1: Text Shaping

**Goal:** Correct glyph positioning, ligatures, kerning, complex script support.

**Approach: Integrate HarfBuzz.** Text shaping implements the OpenType spec mechanically — it's enormous but not architecturally interesting. HarfBuzz is what virtually every modern renderer uses. Building a shaper from scratch is multi-year effort for complex scripts alone (Arabic joining forms, Devanagari conjuncts, Thai word boundaries, CJK vertical layout). This is one of those cases where "build on established standard interfaces, not implementations" clearly points toward integration.

**What HarfBuzz provides:**

- Kerning (supersedes our GPOS pair lookup — HarfBuzz handles the full GPOS spec)
- Ligatures (fi, fl, ffi in text fonts; →, !=, >= in code fonts)
- Complex scripts (Arabic RTL + joining, Devanagari, Thai, CJK)
- OpenType features (small caps, oldstyle figures, tabular numbers, stylistic sets)

**Integration path:**

- HarfBuzz is C/C++ with a C API. Call via FFI from Rust.
- For bare-metal: HarfBuzz can be built without stdlib (`hb-subset` and core shaper). Needs a porting layer for memory allocation. Alternatively, run shaping in the OS service (which already handles document semantics) and pass positioned glyph arrays to the compositor.
- HarfBuzz uses our TrueType font data — we provide a `hb_font_t` backed by our parser, or let HarfBuzz do its own parsing.

**Impact:** Dramatic visual improvement. The jump from "evenly spaced glyphs" to "properly shaped text" is the single biggest quality delta.

### Phase 2: Gamma-Correct Compositing

**Goal:** Perceptually correct text weight regardless of foreground/background combination.

**Problem:** Anti-aliased text is blended using alpha compositing in sRGB space: `result = fg * alpha + bg * (1 - alpha)`. sRGB is perceptually non-linear (gamma ~2.2), so mathematical interpolation in sRGB doesn't correspond to perceptual interpolation in light. Effects:

- Dark-on-light text looks slightly too thin
- Light-on-dark text looks slightly too bold (halation)
- Colored text on colored backgrounds gets hue shifts at anti-aliased edges

**Fix:** Blend in linear light space. For each channel: sRGB→linear (table lookup) → blend → linear→sRGB (table lookup). Two 256-entry lookup tables, two table lookups per channel per pixel.

**Implementation:** Modify `draw_coverage` in `libraries/drawing/lib.rs`. The lookup tables are ~512 bytes total (256 × u16 for sRGB→linear, 256 × u8 for linear→sRGB, or use 4096-entry table for 12-bit linear precision).

**Impact:** Text weight becomes perceptually consistent across all color combinations. Most visible improvement for light-on-dark (dark mode) text. No OS gets this fully right — we can from day one.

### Phase 3: Unicode Coverage & Font Fallback

**Goal:** Render any Unicode codepoint, not just ASCII.

**Glyph cache expansion:**

- Current: fixed 95-slot array (0x20–0x7E), ~1.3 MiB per cache.
- Needed: dynamic cache (LRU or similar) for arbitrary codepoints.
- Design consideration: the glyph cache lives in the compositor/scene renderer, which is memory-constrained (bare metal). A fixed atlas with LRU eviction is appropriate. Size depends on working set — Latin-1 is 191 codepoints, full Latin Extended + common symbols is ~500, CJK is unbounded.

**Font fallback:**

- When the primary font lacks a glyph, substitute from a fallback chain.
- The OS service knows the content type — fallback can be content-type-aware (different chains for code vs prose vs UI).
- Fallback chain: primary font → script-specific font (e.g., Noto CJK) → last-resort (Noto Sans).
- The Noto font family is designed explicitly for this role (covers all Unicode scripts).

### Phase 4: Variable Font Support

**Goal:** Parse and render variable fonts, enabling optical sizing and weight correction.

**Tables to add:**

- `fvar` — variation axes (weight, width, optical size, slant, custom)
- `gvar` — glyph variation data (deltas per axis per point)
- `STAT` — style attributes (axis value names)
- `avar` — axis variation (non-linear axis mapping)

**Implementation:** Extend `truetype.rs` to parse these tables. Glyph outline points are adjusted by interpolating deltas from `gvar` based on current axis positions. This happens before rasterization — the rest of the pipeline is unchanged.

**What this unlocks:**

- Optical sizing (Phase 5a)
- Dark mode weight correction (Phase 5b)
- Responsive typography (weight/width adapts to context)

### Phase 5a: Automatic Optical Sizing

**Goal:** Text looks correct at every size without user intervention.

Traditional metal type used different physical cuts at different sizes — small text had wider proportions, thicker hairlines, and more open counters. This knowledge was lost when digital type scaled one outline to all sizes. Variable fonts with an `opsz` axis restore it.

**How it works:** The compositor knows the font's rendered size in physical pixels (from font size + display DPI). For fonts with an `opsz` axis, automatically set the optical size value to match the rendered size. Small text gets the small-optical-size cut (wider, sturdier), headlines get the display cut (finer details).

**Fonts that support this:** Source Serif, Roboto Flex, Recursive, and an expanding set of quality typefaces. Nunito Sans does not currently have an optical size axis — if we want this, we'd need fonts that support it.

**No OS does this automatically and comprehensively.** macOS only applies it if the app opts in. Most apps don't.

### Phase 5b: Perceptual Dark Mode Weight Correction

**Goal:** Same perceived text weight regardless of background luminance.

**Problem:** Light-on-dark text appears bolder than dark-on-light due to irradiation (bright areas spread into dark in human vision). Even with gamma-correct blending (Phase 2), the perceptual effect persists.

**Fix:** With variable fonts, reduce the weight axis value proportionally to the foreground/background luminance contrast. Not a binary light/dark switch — continuous correction.

**Implementation:** The compositor knows foreground color, background color, and the font's weight axis range. A simple perceptual model:

1. Compute relative luminance of fg and bg.
2. If fg is lighter than bg (light-on-dark), reduce weight axis by a factor proportional to the contrast ratio.
3. This requires re-rasterizing at the adjusted weight, or pre-caching a few weight variants.

**No OS does this today.**

### Phase 5c: Content-Type-Aware Typography

**Goal:** Text rendering defaults that match the content's purpose.

Since the OS natively understands content types (settled decision #5), typographic defaults can be intelligent:

| Content Type   | Font                 | Features                                          | Notes                                      |
| -------------- | -------------------- | ------------------------------------------------- | ------------------------------------------ |
| Code           | Monospace            | Programming ligatures, tabular figures            | Ligatures for `->`, `!=`, `>=`, `<=`, `=>` |
| Prose          | Proportional         | Optical sizing, real small caps, oldstyle figures | Hanging punctuation if layout supports it  |
| Chat/messaging | Proportional         | Compact line height                               | Optimized for scanning                     |
| Data/tables    | Proportional or mono | Tabular figures, tighter spacing                  | Numbers align in columns                   |
| UI labels      | Proportional         | Medium weight, tight tracking                     | Functional, not decorative                 |

Editors can override, but the OS provides intelligent defaults that no other OS can (because no other OS knows what the text is for).

---

## Build vs. Integrate

| Component                              | Recommendation        | Rationale                                                                                                                                                |
| -------------------------------------- | --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Text shaping**                       | Integrate (HarfBuzz)  | Commodity. Enormous, mechanical, not architecturally interesting. The OpenType spec is ~800 pages.                                                       |
| **Rasterization**                      | Keep building ours    | Architecturally interesting. Current rasterizer is solid. Extend with fractional positioning, gamma tables. GPU rasterization is a research opportunity. |
| **Compositing**                        | Keep building ours    | Lives in our compositor. Get gamma-correct blending right from the start.                                                                                |
| **Variable fonts**                     | Build (extend parser) | Natural extension of existing truetype.rs. The parsing is tractable; the interesting work is what we do with the axes.                                   |
| **Optical sizing / weight correction** | Build (novel)         | These are our contributions. They don't exist elsewhere to integrate.                                                                                    |
| **Font fallback**                      | Build                 | Tightly coupled to our content-type model. Simple algorithm, custom policy.                                                                              |

---

## Research Frontiers (Longer-Term)

### GPU-Native Text Rendering

Current pipeline: CPU rasterizes glyphs → caches bitmaps in texture atlas → GPU composites. The atlas has a fixed resolution; zooming requires re-rasterization.

Alternatives under active research:

- **Slug** (Eric Lengyel, SIGGRAPH) — renders Bezier curves directly on GPU. Resolution-independent. Proprietary.
- **Pathfinder / Vello** (Rust) — GPU 2D vector rendering, open source. Can render font outlines directly.
- **Multi-channel Signed Distance Fields (MSDF)** — store distance-to-edge per channel, render on GPU. Scales beautifully. Used extensively in games (Valve popularized basic SDF). Not perfect below ~16px but excellent above.

Relevance: the scene graph architecture (compositor renders from typed visual nodes in shared memory) could naturally express glyphs as outlines or SDF textures rather than pre-rasterized bitmaps. No production OS does GPU-native text rendering for its entire UI — this is frontier territory.

### Environment-Adaptive Rendering

No OS adapts font rendering to viewing conditions:

- **Display-aware:** Tune rendering per-display using EDID data (subpixel layout, density, panel type). Adapt when windows move between displays.
- **Ambient light:** Increase contrast in bright environments, soften in dim (requires ambient light sensor data).
- Speculative, but the sensor data exists on most hardware.

### Neural/Learned Rendering

Research into using neural networks for anti-aliasing and hinting decisions. Could produce resolution-independent rendering with learned perceptual optimization. Very early stage but worth tracking.

---

## Suggested Implementation Sequence

```
Phase 1: Text shaping (HarfBuzz)           — biggest visual delta
Phase 2: Gamma-correct compositing          — correctness, low effort
Phase 3: Unicode coverage + font fallback   — completeness
Phase 4: Variable font support              — enables phases 5a/5b
Phase 5a: Automatic optical sizing          — novel, quality
Phase 5b: Dark mode weight correction       — novel, quality
Phase 5c: Content-type-aware defaults       — novel, unique to our architecture
```

Phases 1 and 2 can be done in parallel (shaping is in the text layout path, gamma correction is in the compositing path). Phase 3 depends on Phase 1 (shaped text produces arbitrary codepoints). Phases 5a–5c all depend on Phase 4 (variable fonts).

---

## Open Questions

1. **HarfBuzz on bare metal:** What's the porting surface? Can we run it in the OS service and pass positioned glyph arrays to the compositor, or does it need to be in the rendering path?
2. **Font selection:** Source Code Pro and Nunito Sans are good starting fonts but neither has an optical size axis. Do we switch to fonts with variable axes (e.g., Recursive for code, Source Serif for prose) or add variable font support and font selection as separate phases?
3. **Glyph cache architecture:** LRU eviction? Fixed atlas with dynamic sub-allocation? How much memory budget for glyph caches on bare metal?
4. **Subpixel rendering sunset:** Do we keep the existing 6× horizontal oversampling path for low-DPI, or simplify to grayscale-only (matching Apple's direction)?
