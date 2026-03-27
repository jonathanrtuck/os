# Rich Text Rendering: Atlas Redesign & Style Pipeline

**Date:** 2026-03-27
**Status:** Approved
**Scope:** Glyph atlas, style registry, layout fixes, renderer integration

## Problem

The v0.5 mission wired rich text styling from editor → core → scene graph, but the renderer ignores all style information. Five bugs:

1. **Atlas rasterizes with empty axes** — `rasterize_with_axes` called with `&[]`, so bold/italic glyphs look identical to regular
2. **MAX_FONTS = 2, all styles collapse** — axis_hash is truncated to font_id 0 or 1, losing all style info
3. **Single global font_size_px** — headings at 24pt are rasterized at 14pt body size
4. **No x-offset for multi-segment lines** — styled segments on the same line overlap at x=0
5. **Fixed line height regardless of font size** — 24pt heading gets same vertical space as 14pt body

Root cause: the atlas and renderer were built for a single font at one size. The data pipeline was extended for rich text but the rendering leaf nodes were not.

## Design Principles

- **Never revisit.** Success = fonts and text rendering are architecturally complete. Arbitrary fonts, arbitrary variable axes, arbitrary sizes. No artificial limits.
- **Cache by full identity.** A glyph's visual identity is (glyph_id, font_size_px, style_id). The atlas key must capture all three dimensions.
- **Collision-free by construction.** No hashing for identity. Sequential IDs are unique by assignment, not by probability.
- **Leaf node complexity.** The atlas is a leaf node behind a simple interface. Internal complexity (hash table, eviction) doesn't leak.
- **Scene graph stays compact.** Each glyph node carries style_id (u32) and font_size (u16). No axis values in the scene graph.
- **Horizontal LTR assumed.** Layout uses x for inline advance and y for block advance. This is a known assumption tied to unsettled Decision #15 (Layout Engine). Vertical text and RTL require a layout engine redesign — not a rendering change. This spec does not abstract inline/block direction, but documents where the assumption lives so future work knows what to change.

## Section 1: Font Style Registry

### Problem

The renderer needs axis values (weight, italic, optical size, etc.) and font data to rasterize a glyph. The scene graph only carries a compact style identifier. The renderer needs a lookup table to map this identifier to rasterization parameters.

### Design

**Sequential style IDs replace hashing.** Core maintains a style table. Each unique `(content_id, axes[])` combination gets the next u32 ID (0, 1, 2, ...). The scene graph carries `style_id` — a direct index into the style registry. No hash, no collisions, ever.

The `Content::Glyphs` field is renamed from `axis_hash` to `style_id`. Its semantics change from "opaque hash" to "index into the style registry." The binary layout (u32) is unchanged.

Core writes the **style registry** as the first item in the scene data buffer each frame. It maps style_id to rasterization parameters:

```rust
#[repr(C)]
struct StyleRegistryHeader {
    magic: u32,          // "STYL" = 0x4C595453
    entry_count: u16,
    max_axes: u8,        // axes array size per entry (fixed, 8)
    _pad: u8,
}

#[repr(C)]
struct StyleRegistryEntry {
    style_id: u32,       // sequential, collision-free
    content_id: u32,     // font data location in Content Region
    ascent_fu: u16,      // font ascender in font units
    descent_fu: u16,     // font descender in font units (positive = below baseline)
    upem: u16,           // units per em for this font
    axis_count: u8,      // number of valid axes
    _pad: u8,
    axes: [AxisValue; MAX_STYLE_AXES],
}

#[repr(C)]
struct AxisValue {
    tag: [u8; 4],        // e.g., b"wght", b"ital", b"opsz"
    value: f32,          // e.g., 700.0, 1.0, 24.0
}
```

**MAX_STYLE_AXES = 8.** Covers all practical variable font axes (OpenType fonts rarely exceed 5). Entry size = 4+4+2+2+2+1+1+64 = 80 bytes. For the 7 default palette styles (body, heading1, heading2, bold, italic, bold-italic, code) plus 2 chrome styles (mono for plain text, sans for chrome): 9 entries × 80 bytes + 8 byte header = 728 bytes in the 128 KiB data buffer.

### Style ID assignment

Core maintains a `StyleTable` — a small array of `(content_id, axes[])` tuples. When building a scene graph, core calls:

```rust
/// Get or assign a style_id for this (content_id, axes) combination.
/// Returns the existing ID if already registered, or assigns the next
/// sequential ID. Collision-free by construction.
fn style_id_for(&mut self, content_id: u32, axes: &[AxisValue]) -> u32
```

Linear scan of the table for deduplication. For 9 styles this is trivial. For a font browser with hundreds of visible styles, still negligible — the table is small and hot in cache.

IDs are stable within a session but NOT across reboots. They're ephemeral scene-graph identifiers, not persistent data. The piece table stores style information (font_family, weight, italic), not style_ids. Core recomputes IDs at startup.

### FONT_MONO / FONT_SANS / FONT_SERIF

These are **no longer compile-time constants** in the scene library. They become runtime values: the style_ids assigned to the three base fonts at default weight:

```rust
// In core, at startup:
let style_mono = style_table.style_id_for(mono_content_id, &[]);  // → 0
let style_sans = style_table.style_id_for(sans_content_id, &[]);  // → 1
let style_serif = style_table.style_id_for(serif_content_id, &[]); // → 2
```

Since these are the first three registered, they'll be 0, 1, 2 — but code must not depend on this. The scene library's `pub const FONT_MONO/FONT_SANS/FONT_SERIF` are removed. Core stores the assigned IDs and uses them when building chrome text nodes and plain text document nodes.

### Registry placement

The style registry is **always the first item** in the scene data buffer. The renderer reads it at a fixed offset (byte 0 of the data buffer) — no scanning. The magic field exists for validation, not discovery.

### Plain text documents

**All documents write a style registry.** For text/plain, the registry has 2 entries: the document's mono font (for text) and the sans font (for chrome). For text/rich, it has 2 + N palette entries. No special fallback path — the registry is always present.

### Renderer flow

1. At frame start, read `StyleRegistryHeader` from data buffer offset 0
2. Validate magic, build a local lookup array indexed by style_id (direct index, O(1))
3. During pre-scan and scene walk, look up style_id to get font data + axes

Since style_ids are sequential starting from 0, the renderer can use a simple array indexed by style_id instead of a hash map for the registry lookup. This is O(1) with no hashing or probing.

## Section 2: Hash-Map Glyph Atlas

### Problem

The current atlas is a flat array indexed by `font_id * GLYPH_STRIDE + glyph_id`. It has 2 font slots and can't distinguish size or style variants.

### Design

Replace with an open-addressed hash table. Fixed capacity, no heap allocation.

**Key:** `(glyph_id: u16, font_size_px: u16, style_id: u32)` packed into a `u64`.

**Entry:** Same as today — `{ u, v, width, height, bearing_x, bearing_y }` (12 bytes).

**Slot:**

```rust
struct AtlasSlot {
    key: u64,            // u64::MAX = empty sentinel
    entry: AtlasEntry,   // 12 bytes
}
```

**Empty sentinel:** `u64::MAX`. This avoids collision with any valid key — a valid key would require `glyph_id=0xFFFF, font_size_px=0xFFFF, style_id=0xFFFFFFFF`, which is degenerate (65535px font size and 4 billion styles are both impossible).

**Capacity:** 16384 slots. At 20 bytes/slot = 320 KB. Handles 200+ fonts × 80 glyphs comfortably.

**Hash function:** FNV-1a over the 8-byte packed key. Probe linearly on collision.

Note: the hash here is for the atlas's internal bucket placement, NOT for identity. The key is the identity (collision-free because style_id is collision-free). The hash just distributes keys across buckets. A hash collision in the atlas means a linear probe, not a wrong glyph.

**Lookup:** `fn lookup(&self, glyph_id: u16, font_size_px: u16, style_id: u32) -> Option<&AtlasEntry>`

**Insert:** `fn insert(&mut self, glyph_id: u16, font_size_px: u16, style_id: u32, entry: AtlasEntry) -> bool`

**Eviction:** When the pixel texture fills (row packing hits the bottom), full reset — clear all slots (fill with `u64::MAX`), reset row packing. Next frame re-rasterizes visible glyphs on demand. Correct behavior (never stale), simple, and the interface supports upgrading to LRU later without changing callers.

**Eliminated concepts:** `font_id`, `MAX_FONTS`, `GLYPH_STRIDE`, `effective_id()`. Gone entirely.

### font_size_px rounding

Consistent rounding everywhere: `font_size_px = (font_size_pt as f32 * scale_factor + 0.5) as u16`. This is the same formula used in the existing code (`main.rs:306`), now applied per-node. The rounding function is defined once and called by both the pre-scan and scene walk to prevent atlas misses from rounding inconsistency.

### Pixel texture

Stays the same format (grayscale alpha, row-packed). Larger glyphs (24pt heading at 2× = 48px) consume more texture space than 14pt body (28px). The existing texture dimensions should suffice for document editing. If a font browser causes frequent resets, the texture can be enlarged — that's a tuning knob, not an architecture change.

### Raster buffer

The current rasterization scratch buffer is 100×100 pixels (`main.rs:571`). At 50pt × 2x scale = 100px, this is exactly at the limit. Increase to 256×256 to provide headroom for larger display sizes (3x scale), higher heading sizes, or future use cases. Cost: 64 KB (was 10 KB) — negligible in a process with megabytes of atlas texture.

## Section 3: Layout Fixes (Core Service)

**Assumption:** All layout in this section assumes horizontal left-to-right text. "x" means inline direction, "y" means block direction. This is documented in the Design Principles section and tied to Decision #15.

### 3a: Per-segment x-offset

**File:** `services/core/layout/full.rs`, `allocate_rich_line_nodes()` (line 772)

Currently each segment's scene node has `n.x` unset (defaults to 0) at line 836. Fix: track a running `pen_x` across segments within each line. After shaping a segment, sum its glyph x_advances to get segment width. Set `n.x = scene::pt(pen_x)` before advancing.

```text
for line in rich_lines:
    pen_x = 0.0
    for seg in line.segments:
        shape segment → glyphs[]
        node.x = scene::pt(pen_x as i32)
        pen_x += sum(glyph.x_advance for glyph in glyphs) / 65536.0
```

The x_advance values are 16.16 fixed-point (same as the existing monospace path). Division by 65536 converts to points.

### 3b: Per-line height

**File:** `services/core/layout/mod.rs`, `layout_rich_lines()` (line 349)

Currently `y += line_height` (line 463) uses a global constant. Fix: compute each line's height from its content.

For each line, walk its segments. For each segment's style, compute line height from font metrics:

```text
segment_line_h = (ascender + abs(descender) + line_gap) * font_size / upem
```

Use the maximum across all segments in the line. Advance `y` by that maximum.

This requires font metrics (ascender, descender, line_gap) per font. These are available from the `hhea` or `OS/2` tables via the fonts library. Core resolves them once per font at startup and caches them in an extended `FontInfo` struct:

```rust
pub struct FontInfo<'a> {
    pub data: &'a [u8],
    pub upem: u16,
    pub ascender: i16,   // font units, positive = above baseline
    pub descender: i16,  // font units, negative = below baseline
    pub line_gap: i16,   // font units, additional inter-line spacing
}
```

### 3c: Variation-aware advance widths

**File:** `libraries/fonts/`, `services/core/layout/mod.rs`

`char_advance_pt()` (line 332) reads the default hmtx advance, ignoring that bold Inter is wider than regular Inter. Line breaks are computed at wrong widths for non-default weights.

Fix: apply HVAR (Horizontal Metrics Variation) deltas. The HVAR table maps (glyph_id, axis_coordinates) → advance width delta. The corrected advance is `hmtx_advance + hvar_delta`.

**New function in fonts library:**

```rust
pub fn glyph_h_advance_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    axes: &[AxisValue],
) -> Option<i16>
```

Falls back to plain `glyph_h_metrics` if no HVAR table exists (graceful degradation for fonts without variation tables).

**Font HVAR availability:**

- **Inter:** Yes — well-designed variable font with HVAR. Primary use case (bold text wider than regular) is covered.
- **JetBrains Mono:** Monospace — advance widths are constant across all weights by definition. HVAR not needed; fallback to hmtx is correct.
- **Source Serif 4:** Should have HVAR for weight axis. Verify during implementation.

`char_advance_pt()` in core's layout gains an `axes` parameter and calls the new function.

**Scope:** HVAR parsing is a real investment (~200-300 lines in the fonts library). It's the same class of work as the existing gvar parser. Done right, it's the last piece needed for fully correct variable font metrics.

## Section 4: Renderer Integration (metal-render)

Summary of all changes:

| File            | Change                                                                                                                                                                                                                                    |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `atlas.rs`      | Rewrite: hash-map atlas replacing flat array                                                                                                                                                                                              |
| `main.rs`       | Load 3 font slices (add serif). Read font_size per node. Parse style registry. Pass axes to rasterizer. Increase raster buffer to 256×256.                                                                                                |
| `scene_walk.rs` | Per-node baseline from registry's ascent_fu + node's font_size. Per-node font_size_px for atlas lookup. Remove global `font_ascent` from `RenderContext`. Explicitly destructure `font_size` from `Content::Glyphs` (was hidden by `..`). |

### Pre-scan loop (main.rs)

Currently scans all visible glyph nodes and rasterizes missing glyphs at a single font_size_px with empty axes. Changes:

1. Read `font_size` from each `Content::Glyphs` node
2. Compute `font_size_px = round_font_size(font_size, scale_factor)` (per node, using the canonical rounding function)
3. Check atlas for `(glyph_id, font_size_px, style_id)`
4. On miss: look up `style_id` in style registry → get content_id + axes
5. Find font data via content_id in Content Region
6. Call `rasterize_with_axes(font_data, glyph_id, font_size_px, axes)`
7. Insert into hash-map atlas

### Scene walk (scene_walk.rs)

Currently uses global `font_ascent` (line 508) and ignores `font_size` (destructured away by `..` at line 494). Changes:

1. Explicitly destructure `font_size` and `style_id` from `Content::Glyphs` node (change `..` pattern to named fields)
2. Look up `style_id` in style registry (array index) → get `ascent_fu`, `upem`
3. Compute `baseline = abs_y + (ascent_fu as f32 * font_size as f32 / upem as f32)` (per node)
4. Compute `font_size_px = round_font_size(font_size, scale_factor)` (same canonical function)
5. Look up glyphs in atlas with `(glyph_id, font_size_px, style_id)` key

The `font_ascent: u32` field is **removed** from `RenderContext`. Baseline is always computed per-node from the style registry. No global, no fallback.

### Other render backends

cpu-render and virgil-render are out of scope for this sprint. They continue to work for text/plain. The atlas design is backend-agnostic — porting to other backends is mechanical work using the same interface.

## Testing Strategy

### Visual TDD (the approach the mission should have taken)

1. **RED:** Create rich text document with heading (24pt bold), body (14pt regular), bold, italic, code (13pt mono). Boot, capture screenshot. Run `imgdiff.py` to measure:
   - Heading glyphs are taller than body glyphs (pixel height comparison)
   - Bold glyphs are heavier than regular (pixel density in sampled region)
   - Different segments on the same line don't overlap (gap between styled runs)
   - Each line has appropriate vertical spacing (no overflow/overlap between lines)

   **All of these assertions FAIL** before the fix.

2. **GREEN:** Implement the fix. Same assertions now PASS.

3. **IMPROVE:** Refactor, optimize, clean up.

### Unit tests

- Hash-map atlas: insert, lookup, collision handling, full-reset eviction, capacity limits, u64::MAX sentinel
- Style registry: serialization/deserialization round-trip, lookup by style_id
- StyleTable: style_id assignment is deterministic, deduplicates identical (content_id, axes) pairs
- HVAR parser: advance deltas match reference values for Inter at weight 700
- Per-line height computation: mixed-size lines get correct height
- Per-segment x-offset: multi-segment lines don't overlap
- font_size_px rounding: consistent results from canonical function

### Integration tests

- All existing ~2,320 tests pass (no regression)
- text/plain documents unchanged (regression guard)
- Rich text round-trip: style → save → reboot → load → render matches

### Visual verification

- `hypervisor --capture` + `imgdiff.py` for numerical pixel verification
- Before/after comparisons for each style (heading, bold, italic, code)
- Multi-line documents with mixed styles
- Cmd+B toggle: before and after screenshots show measurable weight difference

## Execution Order (Foundation Up)

1. **fonts library:** HVAR parser + `glyph_h_advance_with_axes()`
2. **protocol library:** Style registry types (StyleRegistryHeader, StyleRegistryEntry, AxisValue)
3. **scene library:** Rename `axis_hash` → `style_id` in Content::Glyphs. Remove compile-time FONT_MONO/FONT_SANS/FONT_SERIF constants.
4. **core:** StyleTable for sequential ID assignment. Extended FontInfo with metrics. Per-line height, per-segment x-offset, variation-aware advances. Write style registry into scene data buffer.
5. **metal-render atlas:** Hash-map rewrite
6. **metal-render main+scene_walk:** Registry parsing, per-node font_size, per-node baseline, axes to rasterizer, remove global font_ascent, increase raster buffer, load serif font
7. **Visual verification:** TDD loop with hypervisor captures

Each layer is testable independently before the next begins.

## Out of Scope

- cpu-render / virgil-render backend updates (separate task)
- Atlas LRU eviction (upgrade path exists, not needed yet)
- Font discovery / user-installable fonts / font browser (future milestone — basic font loading from disk and Content Region already works)
- OpenType feature support beyond variable axes (ligatures, contextual alternates — already handled by shaper)
- Vertical text / RTL layout (requires settling Decision #15: Layout Engine)
