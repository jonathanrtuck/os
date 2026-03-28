# Rich Text Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 5 rich text rendering bugs so styled text (bold, italic, headings, code) renders correctly with distinct fonts, sizes, and weights.

**Architecture:** Sequential style IDs replace the broken axis_hash system. A font style registry in the scene data buffer maps style_id → rasterization parameters. A hash-map glyph atlas replaces the flat-array atlas, keyed by (glyph_id, font_size_px, style_id). Layout fixes add per-segment x-offset and per-line height.

**Tech Stack:** Rust (no_std, aarch64-unknown-none), custom bare-metal OS. Build: `cd system && cargo build --release`. Test: `cd system/test && cargo test -- --test-threads=1`. Visual: `hypervisor <kernel> --drive disk.img --capture N /tmp/out.png --timeout 30`.

**Spec:** `docs/superpowers/specs/2026-03-27-rich-text-rendering-design.md`

**Review corrections (apply during implementation):**
1. **Font file paths:** Fonts are at `system/share/inter.ttf`, `system/share/jetbrains-mono.ttf`, `system/share/source-serif-4.ttf`. Test includes use `include_bytes!("../../share/inter.ttf")` (see `render_shaping.rs` for the pattern).
2. **Test imports:** The test crate has `fonts`, `protocol`, and `scene` as Cargo deps. Use `use fonts::...` / `use protocol::content::...` directly — no `#[path]` includes needed for library code.
3. **Atlas test include:** atlas.rs uses `use crate::{ATLAS_HEIGHT, ATLAS_WIDTH}` from metal-render's main.rs. To test in isolation: move `ATLAS_WIDTH`/`ATLAS_HEIGHT` constants into atlas.rs itself (they're 512×512). Then the test can include atlas.rs via `#[path]`.
4. **Build breakage:** Task 3 (atlas rewrite) breaks the metal-render build until Task 7. Fix: keep old `lookup(glyph_id, font_id)` and `pack(glyph_id, font_id, ...)` as deprecated wrapper methods that delegate to the new API with `font_size_px=0, style_id=font_id`. Remove wrappers in Task 7 when callers are updated.
5. **StyleTable struct:** Replace the unwieldy 8-element tuple with a named struct `StyleEntry { content_id: u32, axes: Vec<AxisValue>, upem: u16, ascent_fu: u16, descent_fu: u16, ascender: i16, descender: i16, line_gap: i16 }`.
6. **RenderContext fields:** Remove `font_ascent: u32`. Add `style_registry: &'a [protocol::content::StyleRegistryEntry]` and `scale_factor: f32`.
7. **StyleAxisValue vs AxisValue:** The protocol library defines `StyleAxisValue` for the registry. The fonts library already has `fonts::rasterize::AxisValue` with identical layout. When converting registry entries to rasterizer calls, map between them. Do NOT add a cross-dependency between protocol and fonts.
8. **Pre-scan axes allocation:** Use `let mut axes_buf = [fonts::rasterize::AxisValue { tag: [0;4], value: 0.0 }; 8]; let axes = &axes_buf[..entry.axis_count as usize];` — stack-allocated, no Vec.
9. **FontInfo construction sites:** The extended FontInfo (adding content_id, ascender, descender, line_gap) breaks all construction sites. Key locations to update: `layout/full.rs` ~lines 753-757 (RichFonts resolution), `main.rs` font loading sections, and anywhere `FontInfo { data, upem }` appears.
10. **SceneWriter data buffer:** Use `w.push_data(&registry_bytes)` to write registry as the first data item. The returned `DataRef` is unused (renderer reads from fixed offset 0).
11. **HVAR complexity:** The `read-fonts` crate (already a dependency) has built-in HVAR support. Check `read-fonts` API for `Hvar` table access before writing a parser from scratch — may significantly reduce the work.

---

## File Structure

### New Files

| File | Responsibility |
|------|---------------|
| `system/libraries/fonts/src/rasterize/hvar.rs` | HVAR table parser — advance width deltas for variable fonts |
| `system/test/tests/render_atlas_hashmap.rs` | Host-side unit tests for the hash-map atlas |
| `system/test/tests/render_style_registry.rs` | Host-side unit tests for style registry serialization |
| `system/test/tests/text_hvar.rs` | Host-side unit tests for HVAR parser |

### Modified Files

| File | Change |
|------|--------|
| `system/libraries/fonts/src/rasterize/mod.rs` | Add `pub mod hvar;` and re-export `glyph_h_advance_with_axes` |
| `system/libraries/fonts/src/rasterize/metrics.rs` | Add `glyph_h_advance_with_axes()` that delegates to HVAR |
| `system/libraries/protocol/content.rs` | Add `StyleRegistryHeader`, `StyleRegistryEntry`, serialization |
| `system/libraries/scene/primitives.rs` | Rename `axis_hash` → `style_id`, remove FONT_MONO/FONT_SANS/FONT_SERIF constants |
| `system/services/core/layout/mod.rs` | Extended `FontInfo` with metrics, `StyleTable`, per-line height, variation-aware advances, remove `rich_axis_hash()` |
| `system/services/core/layout/full.rs` | Per-segment x-offset in `allocate_rich_line_nodes()`, style registry writing, use `style_id` |
| `system/services/core/layout/incremental.rs` | Update `axis_hash:` → `style_id:` references |
| `system/services/core/main.rs` | StyleTable initialization, font metric loading, pass to layout |
| `system/services/core/fallback.rs` | Update axis_hash references to style_id |
| `system/services/drivers/metal-render/atlas.rs` | Complete rewrite: hash-map atlas |
| `system/services/drivers/metal-render/main.rs` | Load 3 fonts, parse registry, per-node font_size, axes to rasterizer, raster buffer 256² |
| `system/services/drivers/metal-render/scene_walk.rs` | Per-node baseline, remove global `font_ascent`, explicit `font_size`/`style_id` destructure |
| `system/libraries/fonts/src/cache.rs` | Rename `axis_hash` parameter to `style_id` in cache API |

---

## Phase 1: Foundation (new code, no existing breakage)

### Task 1: HVAR Parser — Table Parsing

**Files:**
- Create: `system/libraries/fonts/src/rasterize/hvar.rs`
- Create: `system/test/tests/text_hvar.rs`
- Modify: `system/libraries/fonts/src/rasterize/mod.rs`
- Modify: `system/libraries/fonts/Cargo.toml` (if needed for test dep)

**Context:** The HVAR (Horizontal Metrics Variation) table provides advance width deltas for variable fonts. Bold Inter at w700 has wider glyphs than regular w400. Without HVAR, line breaking measures at default width. The existing gvar.rs parser provides the pattern — gvar varies outlines, HVAR varies metrics. Both use the same ItemVariationStore format.

The HVAR table structure (OpenType spec):
- Header: majorVersion, minorVersion, itemVariationStoreOffset, advanceWidthMappingOffset
- ItemVariationStore: contains variation data indexed by (outer, inner) pairs
- DeltaSetMapping (optional): maps glyph_id → (outer, inner) index

Reference: `system/libraries/fonts/src/rasterize/gvar.rs` for axis normalization + delta computation patterns.

- [ ] **Step 1: Write failing test — Inter bold has different advance width**

```rust
// system/test/tests/text_hvar.rs
//! HVAR table tests — variable font advance width deltas.

// Include the fonts library source for host-side testing.
// The test crate already includes fonts via Cargo dependencies.

use fonts::rasterize::{glyph_id_for_char, glyph_h_metrics, AxisValue};

/// Load Inter font data from the share directory.
fn inter_font_data() -> Vec<u8> {
    std::fs::read("../share/fonts/Inter.ttf").expect("Inter.ttf not found in share/fonts/")
}

#[test]
fn inter_has_hvar_table() {
    let data = inter_font_data();
    assert!(
        fonts::rasterize::hvar::has_hvar(&data),
        "Inter should have an HVAR table"
    );
}

#[test]
fn bold_advance_wider_than_regular() {
    let data = inter_font_data();
    let gid = glyph_id_for_char(&data, 'M').expect("glyph for M");

    let (default_advance, _) = glyph_h_metrics(&data, gid).expect("metrics");

    let bold_axes = [AxisValue { tag: *b"wght", value: 700.0 }];
    let bold_advance = fonts::rasterize::hvar::advance_with_delta(&data, gid, &bold_axes)
        .expect("HVAR advance");

    // Bold M must be wider than regular M.
    assert!(
        bold_advance > default_advance as i32,
        "Bold advance ({bold_advance}) should exceed regular ({default_advance})"
    );
}

#[test]
fn default_weight_has_zero_delta() {
    let data = inter_font_data();
    let gid = glyph_id_for_char(&data, 'A').expect("glyph for A");
    let (default_advance, _) = glyph_h_metrics(&data, gid).expect("metrics");

    // Weight 400 is the default — delta should be 0 or very small.
    let regular_axes = [AxisValue { tag: *b"wght", value: 400.0 }];
    let adjusted = fonts::rasterize::hvar::advance_with_delta(&data, gid, &regular_axes)
        .expect("HVAR advance");

    assert_eq!(adjusted, default_advance as i32, "Default weight should have zero delta");
}

#[test]
fn monospace_font_no_hvar() {
    let data = std::fs::read("../share/fonts/JetBrainsMono.ttf")
        .expect("JetBrainsMono.ttf not found");
    // JetBrains Mono is monospace — HVAR may not exist or deltas should be 0.
    // Either way, fallback should work gracefully.
    let gid = glyph_id_for_char(&data, 'A').unwrap_or(0);
    let result = fonts::rasterize::hvar::advance_with_delta(&data, gid, &[]);
    // Should return None (no HVAR) or the default advance.
    // Not crashing is the main assertion.
    let _ = result;
}
```

Run: `cd system/test && cargo test text_hvar -- --test-threads=1`
Expected: FAIL — `hvar` module doesn't exist

- [ ] **Step 2: Add empty hvar module so it compiles**

```rust
// system/libraries/fonts/src/rasterize/hvar.rs
//! HVAR (Horizontal Metrics Variation) table parser.
//!
//! Computes advance width deltas for variable fonts. Used by layout
//! to get correct character widths at non-default axis values.

use alloc::vec::Vec;

use super::metrics::AxisValue;

/// Check whether the font has an HVAR table.
pub fn has_hvar(_font_data: &[u8]) -> bool {
    false // TODO
}

/// Compute the adjusted advance width for a glyph at the given axis values.
/// Returns default_advance + delta from HVAR, or None if no HVAR table.
pub fn advance_with_delta(
    _font_data: &[u8],
    _glyph_id: u16,
    _axes: &[AxisValue],
) -> Option<i32> {
    None // TODO
}
```

Add `pub mod hvar;` to `system/libraries/fonts/src/rasterize/mod.rs`.

Run: `cd system/test && cargo test text_hvar -- --test-threads=1`
Expected: Compiles but tests FAIL (has_hvar returns false, advance_with_delta returns None)

- [ ] **Step 3: Implement HVAR parser**

Implement `has_hvar()` and `advance_with_delta()` using the read-fonts crate to parse:
1. HVAR table header (find via tag `b"HVAR"`)
2. ItemVariationStore (shared format with gvar — study `gvar.rs` lines 100-300 for the pattern)
3. DeltaSetMapping (optional — if absent, glyph_id maps directly to inner index)
4. Axis normalization (reuse `normalize_axis_value` from gvar.rs or extract shared helper)
5. Delta computation: for each region in the variation data, compute scalar from normalized axis coords, multiply by delta, sum

The key functions from gvar.rs to study/reuse:
- Axis normalization (converting user-space axis values to normalized [-1, 1] coordinates)
- Tuple scalar computation (how much a particular region contributes given the current axis coords)
- Delta accumulation

Reference: OpenType spec "HVAR — Horizontal Metrics Variations Table"

Run: `cd system/test && cargo test text_hvar -- --test-threads=1`
Expected: All 4 tests PASS

- [ ] **Step 4: Add public API wrapper in metrics.rs**

Add to `system/libraries/fonts/src/rasterize/metrics.rs`:

```rust
/// Get horizontal advance width with variable font axis adjustments.
/// Returns `hmtx_advance + hvar_delta` if HVAR exists, else plain hmtx advance.
/// Falls back gracefully for non-variable fonts.
pub fn glyph_h_advance_with_axes(
    font_data: &[u8],
    glyph_id: u16,
    axes: &[AxisValue],
) -> Option<i32> {
    if let Some(adjusted) = super::hvar::advance_with_delta(font_data, glyph_id, axes) {
        Some(adjusted)
    } else {
        // Fallback: default hmtx advance, no delta.
        let (advance, _) = glyph_h_metrics(font_data, glyph_id)?;
        Some(advance as i32)
    }
}
```

Re-export from `mod.rs`: add `glyph_h_advance_with_axes` to the pub use.

Run: `cd system/test && cargo test text_hvar -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add system/libraries/fonts/src/rasterize/hvar.rs system/libraries/fonts/src/rasterize/mod.rs system/libraries/fonts/src/rasterize/metrics.rs system/test/tests/text_hvar.rs
git commit -m "feat(fonts): HVAR parser for variation-aware advance widths"
```

---

### Task 2: Style Registry Types

**Files:**
- Modify: `system/libraries/protocol/content.rs`
- Create: `system/test/tests/render_style_registry.rs`

**Context:** The style registry is a serialized table written by core into the scene data buffer. The renderer reads it to map style_id → (content_id, axes, metrics). It's always at byte offset 0 of the data buffer. Types go in the protocol library because it's the shared boundary between core and renderer.

- [ ] **Step 1: Write failing test — registry round-trip**

```rust
// system/test/tests/render_style_registry.rs
//! Style registry serialization tests.

#[path = "../../libraries/protocol/content.rs"]
#[allow(dead_code)]
mod content;

// If content.rs has module dependencies, include them here.
// The test should exercise StyleRegistryHeader, StyleRegistryEntry, AxisValue
// serialization into a byte buffer and deserialization back.

use content::{
    StyleRegistryHeader, StyleRegistryEntry, StyleAxisValue,
    write_style_registry, read_style_registry, MAX_STYLE_AXES,
    STYLE_REGISTRY_MAGIC,
};

#[test]
fn empty_registry_round_trip() {
    let mut buf = [0u8; 4096];
    let written = write_style_registry(&mut buf, &[]);
    assert!(written > 0);

    let entries = read_style_registry(&buf);
    assert!(entries.is_some());
    assert_eq!(entries.unwrap().len(), 0);
}

#[test]
fn single_entry_round_trip() {
    let mut buf = [0u8; 4096];
    let entry = StyleRegistryEntry {
        style_id: 0,
        content_id: 42,
        ascent_fu: 900,
        descent_fu: 200,
        upem: 1000,
        axis_count: 1,
        _pad: 0,
        axes: {
            let mut a = [StyleAxisValue { tag: [0; 4], value: 0.0 }; MAX_STYLE_AXES];
            a[0] = StyleAxisValue { tag: *b"wght", value: 700.0 };
            a
        },
    };
    let written = write_style_registry(&mut buf, &[entry]);
    assert!(written > 0);

    let entries = read_style_registry(&buf).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].style_id, 0);
    assert_eq!(entries[0].content_id, 42);
    assert_eq!(entries[0].ascent_fu, 900);
    assert_eq!(entries[0].axis_count, 1);
    assert_eq!(entries[0].axes[0].tag, *b"wght");
    assert_eq!(entries[0].axes[0].value, 700.0);
}

#[test]
fn magic_validation() {
    let buf = [0u8; 4096]; // all zeros — wrong magic
    assert!(read_style_registry(&buf).is_none());
}

#[test]
fn nine_entries_round_trip() {
    // 7 palette + 2 chrome = 9 entries (the expected v0.5 count)
    let mut buf = [0u8; 8192];
    let entries: Vec<StyleRegistryEntry> = (0..9)
        .map(|i| StyleRegistryEntry {
            style_id: i,
            content_id: 100 + i,
            ascent_fu: 800 + i as u16,
            descent_fu: 200,
            upem: 1000,
            axis_count: 0,
            _pad: 0,
            axes: [StyleAxisValue { tag: [0; 4], value: 0.0 }; MAX_STYLE_AXES],
        })
        .collect();

    let written = write_style_registry(&mut buf, &entries);
    assert!(written > 0);

    let read_back = read_style_registry(&buf).unwrap();
    assert_eq!(read_back.len(), 9);
    for (i, e) in read_back.iter().enumerate() {
        assert_eq!(e.style_id, i as u32);
        assert_eq!(e.content_id, 100 + i as u32);
    }
}
```

Run: `cd system/test && cargo test render_style_registry -- --test-threads=1`
Expected: FAIL — types don't exist

- [ ] **Step 2: Implement style registry types and serialization**

Add to `system/libraries/protocol/content.rs`:

```rust
// ── Style Registry ──────────────────────────────────────────────────

/// Magic value for StyleRegistryHeader: "STYL" as little-endian u32.
pub const STYLE_REGISTRY_MAGIC: u32 = 0x4C59_5453;

/// Maximum variable font axes per style entry.
pub const MAX_STYLE_AXES: usize = 8;

/// Header for the style registry in the scene data buffer.
/// Always at byte offset 0.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StyleRegistryHeader {
    pub magic: u32,
    pub entry_count: u16,
    pub max_axes: u8,
    pub _pad: u8,
}

/// A variable font axis value (tag + design-space value).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StyleAxisValue {
    pub tag: [u8; 4],
    pub value: f32,
}

/// One entry in the style registry, mapping style_id to rasterization params.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StyleRegistryEntry {
    pub style_id: u32,
    pub content_id: u32,
    pub ascent_fu: u16,
    pub descent_fu: u16,
    pub upem: u16,
    pub axis_count: u8,
    pub _pad: u8,
    pub axes: [StyleAxisValue; MAX_STYLE_AXES],
}

/// Write a style registry into a byte buffer. Returns bytes written.
pub fn write_style_registry(buf: &mut [u8], entries: &[StyleRegistryEntry]) -> usize {
    let header_size = core::mem::size_of::<StyleRegistryHeader>();
    let entry_size = core::mem::size_of::<StyleRegistryEntry>();
    let total = header_size + entries.len() * entry_size;
    if buf.len() < total {
        return 0;
    }
    let header = StyleRegistryHeader {
        magic: STYLE_REGISTRY_MAGIC,
        entry_count: entries.len() as u16,
        max_axes: MAX_STYLE_AXES as u8,
        _pad: 0,
    };
    // SAFETY: repr(C) structs, buffer bounds checked above.
    unsafe {
        core::ptr::copy_nonoverlapping(
            &header as *const _ as *const u8,
            buf.as_mut_ptr(),
            header_size,
        );
        for (i, entry) in entries.iter().enumerate() {
            core::ptr::copy_nonoverlapping(
                entry as *const _ as *const u8,
                buf.as_mut_ptr().add(header_size + i * entry_size),
                entry_size,
            );
        }
    }
    total
}

/// Read a style registry from a byte buffer. Returns None if magic is wrong.
pub fn read_style_registry(buf: &[u8]) -> Option<&[StyleRegistryEntry]> {
    let header_size = core::mem::size_of::<StyleRegistryHeader>();
    if buf.len() < header_size {
        return None;
    }
    // SAFETY: checking alignment and size. repr(C), 4-byte aligned.
    let header = unsafe { &*(buf.as_ptr() as *const StyleRegistryHeader) };
    if header.magic != STYLE_REGISTRY_MAGIC {
        return None;
    }
    let count = header.entry_count as usize;
    let entry_size = core::mem::size_of::<StyleRegistryEntry>();
    let needed = header_size + count * entry_size;
    if buf.len() < needed {
        return None;
    }
    // SAFETY: bounds checked, repr(C) layout.
    let entries = unsafe {
        core::slice::from_raw_parts(
            buf.as_ptr().add(header_size) as *const StyleRegistryEntry,
            count,
        )
    };
    Some(entries)
}
```

Run: `cd system/test && cargo test render_style_registry -- --test-threads=1`
Expected: All 4 tests PASS

- [ ] **Step 3: Commit**

```bash
git add system/libraries/protocol/content.rs system/test/tests/render_style_registry.rs
git commit -m "feat(protocol): style registry types and serialization"
```

---

### Task 3: Hash-Map Glyph Atlas

**Files:**
- Modify: `system/services/drivers/metal-render/atlas.rs` (rewrite)
- Create: `system/test/tests/render_atlas_hashmap.rs`

**Context:** The current atlas is a flat array with 2 font slots. The new atlas is an open-addressed hash table keyed by `(glyph_id, font_size_px, style_id)` packed into a u64. Capacity: 16384 slots. Empty sentinel: u64::MAX. FNV-1a for bucket placement. Full reset on pixel texture exhaustion.

Read the current atlas: `system/services/drivers/metal-render/atlas.rs` (103 lines). The new implementation replaces it entirely. The `AtlasEntry` struct stays the same. The `GlyphAtlas` struct and all methods change.

- [ ] **Step 1: Write failing tests for hash-map atlas**

```rust
// system/test/tests/render_atlas_hashmap.rs
//! Hash-map glyph atlas tests.

// Include the atlas source directly for host-side testing.
// The atlas is self-contained (no kernel dependencies).

#[path = "../../services/drivers/metal-render/atlas.rs"]
mod atlas;

#[test]
fn empty_atlas_lookup_returns_none() {
    let a = atlas::GlyphAtlas::new();
    assert!(a.lookup(65, 28, 0).is_none()); // glyph 'A', 28px, style 0
}

#[test]
fn insert_and_lookup() {
    let mut a = atlas::GlyphAtlas::new();
    assert!(a.insert(65, 28, 0, atlas::AtlasEntry {
        u: 10, v: 20, width: 15, height: 20, bearing_x: 1, bearing_y: 18,
    }));
    let e = a.lookup(65, 28, 0).expect("should find entry");
    assert_eq!(e.u, 10);
    assert_eq!(e.width, 15);
    assert_eq!(e.bearing_y, 18);
}

#[test]
fn different_style_id_different_entry() {
    let mut a = atlas::GlyphAtlas::new();
    // Same glyph, same size, different styles.
    a.insert(65, 28, 0, atlas::AtlasEntry {
        u: 0, v: 0, width: 10, height: 10, bearing_x: 0, bearing_y: 10,
    });
    a.insert(65, 28, 1, atlas::AtlasEntry {
        u: 50, v: 0, width: 12, height: 11, bearing_x: 0, bearing_y: 11,
    });
    let regular = a.lookup(65, 28, 0).unwrap();
    let bold = a.lookup(65, 28, 1).unwrap();
    assert_eq!(regular.width, 10);
    assert_eq!(bold.width, 12);
}

#[test]
fn different_font_size_different_entry() {
    let mut a = atlas::GlyphAtlas::new();
    a.insert(65, 28, 0, atlas::AtlasEntry {
        u: 0, v: 0, width: 10, height: 12, bearing_x: 0, bearing_y: 10,
    });
    a.insert(65, 48, 0, atlas::AtlasEntry {
        u: 50, v: 0, width: 18, height: 22, bearing_x: 0, bearing_y: 18,
    });
    assert_eq!(a.lookup(65, 28, 0).unwrap().width, 10);
    assert_eq!(a.lookup(65, 48, 0).unwrap().width, 18);
}

#[test]
fn reset_clears_all() {
    let mut a = atlas::GlyphAtlas::new();
    a.insert(65, 28, 0, atlas::AtlasEntry {
        u: 0, v: 0, width: 10, height: 10, bearing_x: 0, bearing_y: 10,
    });
    assert!(a.lookup(65, 28, 0).is_some());
    a.reset();
    assert!(a.lookup(65, 28, 0).is_none());
}

#[test]
fn handles_many_inserts() {
    let mut a = atlas::GlyphAtlas::new();
    // Insert 1000 entries (different glyph IDs).
    for gid in 0..1000u16 {
        a.insert(gid, 28, 0, atlas::AtlasEntry {
            u: gid, v: 0, width: 10, height: 10, bearing_x: 0, bearing_y: 10,
        });
    }
    // Verify all are retrievable.
    for gid in 0..1000u16 {
        let e = a.lookup(gid, 28, 0).expect(&format!("glyph {gid}"));
        assert_eq!(e.u, gid);
    }
}

#[test]
fn collision_handling() {
    let mut a = atlas::GlyphAtlas::new();
    // Insert many entries that might hash to same bucket.
    // Use sequential style_ids with same glyph_id + size.
    for sid in 0..100u32 {
        a.insert(65, 28, sid, atlas::AtlasEntry {
            u: sid as u16, v: 0, width: 10, height: 10, bearing_x: 0, bearing_y: 10,
        });
    }
    for sid in 0..100u32 {
        let e = a.lookup(65, 28, sid).expect(&format!("style {sid}"));
        assert_eq!(e.u, sid as u16);
    }
}
```

Run: `cd system/test && cargo test render_atlas_hashmap -- --test-threads=1`
Expected: FAIL — old atlas API doesn't match

- [ ] **Step 2: Rewrite atlas.rs as hash-map**

Replace the entire contents of `system/services/drivers/metal-render/atlas.rs`. The old `MAX_FONTS`, `GLYPH_STRIDE`, `effective_id()` are eliminated. New implementation:

```rust
//! Hash-map glyph atlas with row-based pixel packing.
//!
//! Open-addressed hash table keyed by (glyph_id, font_size_px, style_id).
//! Fixed capacity, no heap allocation. Linear probing on collision.

use crate::{ATLAS_HEIGHT, ATLAS_WIDTH};

/// Atlas entry capacity.
const CAPACITY: usize = 16384;

/// Empty sentinel — no valid key can equal u64::MAX.
const EMPTY: u64 = u64::MAX;

/// Atlas entry for a single rasterized glyph.
#[derive(Clone, Copy)]
pub(crate) struct AtlasEntry {
    pub(crate) u: u16,
    pub(crate) v: u16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) bearing_x: i16,
    pub(crate) bearing_y: i16,
}

/// Hash table slot: key + entry.
#[derive(Clone, Copy)]
struct Slot {
    key: u64,
    entry: AtlasEntry,
}

/// Pack (glyph_id, font_size_px, style_id) into a u64 key.
#[inline]
fn pack_key(glyph_id: u16, font_size_px: u16, style_id: u32) -> u64 {
    (glyph_id as u64) | ((font_size_px as u64) << 16) | ((style_id as u64) << 32)
}

/// FNV-1a hash of a u64 key, reduced to table index.
#[inline]
fn hash_key(key: u64) -> usize {
    let bytes = key.to_le_bytes();
    let mut h: u32 = 2166136261;
    for b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h as usize) & (CAPACITY - 1) // CAPACITY must be power of 2
}

pub(crate) struct GlyphAtlas {
    slots: [Slot; CAPACITY],
    pub(crate) pixels: [u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
    pub(crate) row_y: u16,
    pub(crate) row_x: u16,
    pub(crate) row_h: u16,
}

impl GlyphAtlas {
    pub(crate) fn new() -> Self {
        Self {
            slots: [Slot {
                key: EMPTY,
                entry: AtlasEntry {
                    u: 0, v: 0, width: 0, height: 0, bearing_x: 0, bearing_y: 0,
                },
            }; CAPACITY],
            pixels: [0u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
            row_y: 0,
            row_x: 0,
            row_h: 0,
        }
    }

    pub(crate) fn lookup(
        &self, glyph_id: u16, font_size_px: u16, style_id: u32,
    ) -> Option<&AtlasEntry> {
        let key = pack_key(glyph_id, font_size_px, style_id);
        let mut idx = hash_key(key);
        for _ in 0..CAPACITY {
            let slot = &self.slots[idx];
            if slot.key == key {
                return Some(&slot.entry);
            }
            if slot.key == EMPTY {
                return None;
            }
            idx = (idx + 1) & (CAPACITY - 1);
        }
        None
    }

    pub(crate) fn insert(
        &mut self, glyph_id: u16, font_size_px: u16, style_id: u32,
        entry: AtlasEntry,
    ) -> bool {
        let key = pack_key(glyph_id, font_size_px, style_id);
        let mut idx = hash_key(key);
        for _ in 0..CAPACITY {
            let slot = &self.slots[idx];
            if slot.key == EMPTY || slot.key == key {
                self.slots[idx] = Slot { key, entry };
                return true;
            }
            idx = (idx + 1) & (CAPACITY - 1);
        }
        false // full
    }

    /// Pack a glyph bitmap into the pixel texture. Returns (u, v) or None.
    pub(crate) fn pack(
        &mut self, glyph_id: u16, font_size_px: u16, style_id: u32,
        w: u16, h: u16, bearing_x: i16, bearing_y: i16, data: &[u8],
    ) -> bool {
        if w == 0 || h == 0 {
            return self.insert(glyph_id, font_size_px, style_id, AtlasEntry {
                u: 0, v: 0, width: 0, height: 0, bearing_x, bearing_y,
            });
        }
        // Row packing: if glyph doesn't fit in current row, start new row.
        if self.row_x + w > ATLAS_WIDTH as u16 {
            self.row_y += self.row_h;
            self.row_x = 0;
            self.row_h = 0;
        }
        if self.row_y + h > ATLAS_HEIGHT as u16 {
            return false; // texture full — caller should reset
        }
        let u = self.row_x;
        let v = self.row_y;
        // Copy pixel data into atlas texture.
        for row in 0..h as usize {
            let src_start = row * w as usize;
            let dst_start = (v as usize + row) * ATLAS_WIDTH as usize + u as usize;
            let src_end = src_start + w as usize;
            let dst_end = dst_start + w as usize;
            if src_end <= data.len() && dst_end <= self.pixels.len() {
                self.pixels[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
            }
        }
        self.row_x += w;
        if h > self.row_h {
            self.row_h = h;
        }
        self.insert(glyph_id, font_size_px, style_id, AtlasEntry {
            u, v, width: w, height: h, bearing_x, bearing_y,
        })
    }

    pub(crate) fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.key = EMPTY;
        }
        self.row_x = 0;
        self.row_y = 0;
        self.row_h = 0;
        // Don't zero pixels — they'll be overwritten on re-pack.
    }
}
```

Note: This will temporarily break the metal-render build because callers still use the old API (`lookup(glyph_id, font_id)`, `MAX_FONTS`). That's OK — the tests validate the new data structure in isolation. The callers are updated in Phase 3 (Task 7).

Run: `cd system/test && cargo test render_atlas_hashmap -- --test-threads=1`
Expected: All 7 tests PASS

- [ ] **Step 3: Commit**

```bash
git add system/services/drivers/metal-render/atlas.rs system/test/tests/render_atlas_hashmap.rs
git commit -m "feat(metal-render): hash-map glyph atlas replacing flat array"
```

---

## Phase 2: Core Integration

### Task 4: Rename axis_hash → style_id Across Codebase

**Files:** (all mechanical, no behavior change)
- Modify: `system/libraries/scene/primitives.rs` — field rename + remove constants
- Modify: `system/services/core/layout/full.rs` — all `axis_hash:` → `style_id:`
- Modify: `system/services/core/layout/incremental.rs` — all `axis_hash:` → `style_id:`
- Modify: `system/services/core/layout/mod.rs` — remove `FONT_SANS` re-export, rename `rich_axis_hash` → temporary placeholder
- Modify: `system/services/core/fallback.rs` — update axis_hash references
- Modify: `system/services/drivers/metal-render/scene_walk.rs` — `axis_hash` → `style_id`
- Modify: `system/services/drivers/metal-render/main.rs` — `axis_hash` → `style_id`
- Modify: `system/libraries/fonts/src/cache.rs` — parameter rename

**Context:** This is a pure rename. No behavior change. The values are the same — what was `FONT_SANS = 1` now uses the literal `1u32` (temporarily). Task 5 replaces literals with `StyleTable` lookups.

- [ ] **Step 1: Rename field in scene library**

In `system/libraries/scene/primitives.rs`:
- Line 291: `axis_hash: u32,` → `style_id: u32,`
- Lines 217-219: Remove `FONT_MONO`, `FONT_SANS`, `FONT_SERIF` constants
- Update doc comments referring to "axis_hash"

- [ ] **Step 2: Update all call sites**

Use grep results from earlier. Every `axis_hash:` in Content::Glyphs construction becomes `style_id:`. Every `FONT_SANS` becomes `1u32` (temporary literal). Every `axis_hash` variable or parameter name becomes `style_id`.

Key files and approximate change counts:
- `core/layout/full.rs`: ~12 occurrences (lines 13, 15, 209, 227, 436, 652, 665, 825, 844, 941, 952)
- `core/layout/incremental.rs`: ~4 occurrences (lines 201, 488, 529, 695)
- `core/layout/mod.rs`: ~4 occurrences (lines 20-21, 507, 578, 640)
- `core/fallback.rs`: ~4 occurrences (lines 239, 241, 246, 247)
- `metal-render/scene_walk.rs`: ~2 occurrences (lines 493, 518)
- `metal-render/main.rs`: ~1 occurrence (line 559)
- `fonts/cache.rs`: ~6 occurrences (lines 249, 250, 332, 334, 340, 342, 360, 366)

- [ ] **Step 3: Verify build**

Run: `cd system && cargo build --release`
Expected: Compiles cleanly

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All ~2,320 tests pass

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: rename axis_hash to style_id across codebase"
```

---

### Task 5: StyleTable + Extended FontInfo + Registry Writing

**Files:**
- Modify: `system/services/core/layout/mod.rs` — `StyleTable` struct, extended `FontInfo`
- Modify: `system/services/core/layout/full.rs` — write registry to scene data, use `StyleTable`
- Modify: `system/services/core/main.rs` — initialize `StyleTable` and font metrics at startup
- Modify: `system/test/tests/layout_measured.rs` (if FontInfo changes break existing tests)

**Context:** Core assigns sequential style_ids via `StyleTable`. Extended `FontInfo` carries ascender/descender/line_gap from font metrics (already available via `fonts::rasterize::font_metrics()`). The style registry is written as the first item in the scene data buffer before any other data (glyph arrays, icon pixels, etc.).

- [ ] **Step 1: Add StyleTable and extended FontInfo to layout/mod.rs**

```rust
/// Font data + metrics for a style, resolved from Content Region.
pub struct FontInfo<'a> {
    pub data: &'a [u8],
    pub upem: u16,
    pub content_id: u32,   // Content Region entry ID
    pub ascender: i16,     // font units, positive = above baseline
    pub descender: i16,    // font units, negative = below baseline
    pub line_gap: i16,     // font units, additional inter-line spacing
}

/// Sequential style ID assignment. Collision-free by construction.
pub struct StyleTable {
    /// (content_id, sorted axes) → style_id.
    entries: Vec<(u32, Vec<fonts::rasterize::AxisValue>, u16, u16, u16, i16, i16, i16)>,
    // content_id, axes, upem, ascent_fu, descent_fu, ascender, descender, line_gap
}
```

The StyleTable stores enough information per entry to write StyleRegistryEntry structs into the scene data buffer.

Implement `style_id_for(&mut self, fi: &FontInfo, axes: &[AxisValue]) -> u32` — linear scan to deduplicate, assigns next ID on miss.

Remove `rich_axis_hash()` function (replaced by StyleTable).

- [ ] **Step 2: Wire StyleTable into core main.rs**

In `main.rs`, after font loading from Content Region, create extended FontInfo structs:

```rust
let mono_metrics = fonts::rasterize::font_metrics(mono_font_data).unwrap();
let mono_fi = FontInfo {
    data: mono_font_data,
    upem: mono_metrics.units_per_em,
    content_id: mono_content_id,
    ascender: mono_metrics.ascent,
    descender: mono_metrics.descent,
    line_gap: mono_metrics.line_gap,
};
// Same for sans and serif.

let mut style_table = StyleTable::new();
let style_mono = style_table.style_id_for(&mono_fi, &[]);
let style_sans = style_table.style_id_for(&sans_fi, &[]);
```

Replace all `1u32` (temporary FONT_SANS literals) with `style_sans`, etc.

- [ ] **Step 3: Write style registry into scene data buffer**

In `build_full_scene()` and `build_rich_document_content()` (layout/full.rs), before writing any other data:

```rust
// Write style registry as first item in scene data buffer.
let registry_entries = style_table.to_registry_entries();
let registry_bytes = protocol::content::write_style_registry(
    w.data_buffer_mut(), &registry_entries,
);
// Advance data buffer write position past the registry.
w.advance_data_offset(registry_bytes);
```

This ensures the registry is always at offset 0 of the data buffer.

- [ ] **Step 4: Build and test**

Run: `cd system && cargo build --release`
Expected: Compiles (renderer still uses old API — will be updated in Task 7)

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All tests pass

- [ ] **Step 5: Boot and verify no visual regression**

```bash
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/after-styletable.png --timeout 30
python3 test/imgdiff.py /tmp/v05-baseline.png /tmp/after-styletable.png
```

Expected: Pixel diff should be minimal (clock time only). No visual change yet — renderer still uses old code paths.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(core): StyleTable with sequential IDs and font style registry"
```

---

### Task 6: Layout Fixes

**Files:**
- Modify: `system/services/core/layout/mod.rs` — per-line height, variation-aware advances
- Modify: `system/services/core/layout/full.rs` — per-segment x-offset

**Context:** Three layout bugs: (1) segments overlap at x=0, (2) all lines use body-text height, (3) advance widths ignore variable font weight. These are all in core's layout code.

- [ ] **Step 1: Fix per-segment x-offset in allocate_rich_line_nodes()**

In `system/services/core/layout/full.rs`, function `allocate_rich_line_nodes()` (line 772):

Add a `pen_x: f32 = 0.0` tracker per line. After shaping each segment, set `n.x = scene::pt(pen_x as i32)` and advance pen_x by the sum of glyph x_advances:

```rust
for line in rich_lines {
    let mut pen_x: f32 = 0.0;
    for seg in &line.segments {
        // ... existing shaping code ...
        let shaped = shape_rich_segment(fi.data, seg_text, font_size, fi.upem, style.weight, italic);
        if shaped.is_empty() { continue; }

        // Set x position BEFORE advancing pen.
        if let Some(node_id) = w.alloc_node() {
            let n = w.node_mut(node_id);
            n.x = scene::pt(pen_x as i32);  // NEW: was unset (defaulting to 0)
            n.y = scene::pt(seg.y);
            // ... rest of node setup ...
        }

        // Advance pen by segment width (sum of glyph advances).
        let seg_width: f32 = shaped.iter().map(|g| g.x_advance as f32 / 65536.0).sum();
        pen_x += seg_width;
    }
}
```

- [ ] **Step 2: Fix per-line height in layout_rich_lines()**

In `system/services/core/layout/mod.rs`, function `layout_rich_lines()` (line 349):

Replace `y += line_height` (line 463) with per-line height computation:

```rust
// After building segments for this line, compute line height from content.
let mut max_line_h: i32 = line_height; // fallback to default
for seg in &segments {
    if let Some(style) = piecetable::style(pt_buf, seg.style_id) {
        let fi = match style.font_family {
            piecetable::FONT_MONO => mono_font,
            piecetable::FONT_SERIF => serif_font,
            _ => sans_font,
        };
        if fi.upem > 0 {
            let ascent = fi.ascender.unsigned_abs() as i32;
            let descent = fi.descender.unsigned_abs() as i32;
            let gap = fi.line_gap.max(0) as i32;
            let seg_h = (ascent + descent + gap) * style.font_size_pt as i32
                / fi.upem as i32;
            if seg_h > max_line_h {
                max_line_h = seg_h;
            }
        }
    }
}
result.push(RichLine { segments, y });
y += max_line_h;
```

This requires `layout_rich_lines` to receive the extended FontInfo (with ascender/descender/line_gap).

- [ ] **Step 3: Add variation-aware advance widths**

In `system/services/core/layout/mod.rs`, function `char_advance_pt()` (line 332):

Add an `axes` parameter and use `glyph_h_advance_with_axes` when axes are non-empty:

```rust
fn char_advance_pt(
    font_data: &[u8], ch: char, font_size: u16, upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> f32 {
    if upem == 0 || font_data.is_empty() {
        return 8.0;
    }
    let gid = fonts::rasterize::glyph_id_for_char(font_data, ch).unwrap_or(0);
    if axes.is_empty() {
        let (advance_fu, _) = fonts::rasterize::glyph_h_metrics(font_data, gid).unwrap_or((0, 0));
        (advance_fu as f32 * font_size as f32) / upem as f32
    } else {
        let advance = fonts::rasterize::glyph_h_advance_with_axes(font_data, gid, axes)
            .unwrap_or(0);
        (advance as f32 * font_size as f32) / upem as f32
    }
}
```

Update all call sites of `char_advance_pt` to pass the current style's axes (from the piece table style → font config).

- [ ] **Step 4: Build and test**

Run: `cd system && cargo build --release`
Expected: Compiles

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): per-segment x-offset, per-line height, variation-aware advances"
```

---

## Phase 3: Renderer Integration

### Task 7: Metal-Render — Full Integration

**Files:**
- Modify: `system/services/drivers/metal-render/main.rs` — registry parsing, per-node font_size, 3 font slices, raster buffer 256², axes to rasterizer
- Modify: `system/services/drivers/metal-render/scene_walk.rs` — per-node baseline, remove global font_ascent, style_id destructure, new atlas API

**Context:** This is where all the foundation work connects. The renderer reads the style registry from the scene data buffer, uses style_id to look up font data + axes, rasterizes at the correct font_size_px with correct axes, and computes per-node baselines. This task changes behavior — styled text will finally render differently.

- [ ] **Step 1: Add canonical font_size_px rounding function**

In `main.rs`, add a helper used by both pre-scan and scene walk:

```rust
/// Canonical font_size_px computation. One function, used everywhere.
#[inline]
fn round_font_size(font_size_pt: u16, scale_factor: f32) -> u16 {
    let px = font_size_pt as f32 * scale_factor;
    if px >= 0.0 { (px + 0.5) as u16 } else { 1 }
}
```

- [ ] **Step 2: Load serif font (3 font slices)**

In `main.rs`, where `font_slices` is defined (line 287), add the serif font:

```rust
// Find serif font in Content Region (same pattern as mono and sans).
let serif_font_slice: &[u8] = /* look up CONTENT_ID_FONT_SERIF */;
```

Store all three font slices accessible by content_id for registry lookup.

- [ ] **Step 3: Increase raster buffer to 256×256**

Line 543 (approximately): Change `[0u8; 100 * 100]` to `[0u8; 256 * 256]`. Update the `RasterBuffer` width/height caps to 256.

- [ ] **Step 4: Parse style registry at frame start**

In the render loop, before the glyph pre-scan:

```rust
// Read style registry from scene data buffer offset 0.
let data_buf = reader.front_data();
let registry = protocol::content::read_style_registry(data_buf);
```

Build a local lookup: since style_ids are sequential from 0, an array indexed by style_id works.

- [ ] **Step 5: Update pre-scan to use per-node font_size + registry + new atlas API**

In the pre-scan loop (line ~547):

```rust
if let scene::Content::Glyphs {
    glyphs, glyph_count, font_size, style_id, ..
} = node.content {
    let font_size_px = round_font_size(font_size, scale_factor);

    // Look up style in registry.
    let style_entry = match registry_lookup(style_id) {
        Some(e) => e,
        None => continue,
    };

    // Find font data by content_id.
    let raster_font = find_font_by_content_id(style_entry.content_id);

    // Build axis values for rasterization.
    let axes: Vec<fonts::rasterize::AxisValue> = style_entry.axes[..style_entry.axis_count as usize]
        .iter()
        .map(|a| fonts::rasterize::AxisValue { tag: a.tag, value: a.value })
        .collect();

    let shaped = reader.front_shaped_glyphs(glyphs, glyph_count);
    for sg in shaped {
        if glyph_atlas.lookup(sg.glyph_id, font_size_px, style_id).is_some() {
            continue;
        }
        // Rasterize with actual axes and per-node font size.
        if let Some(m) = fonts::rasterize::rasterize_with_axes(
            raster_font, sg.glyph_id, font_size_px,
            &mut rb, raster_scratch, &axes, scale_factor_int,
        ) {
            glyph_atlas.pack(
                sg.glyph_id, font_size_px, style_id,
                m.width as u16, m.height as u16,
                m.bearing_x as i16, m.bearing_y as i16,
                &raster_buf[..m.width as usize * m.height as usize],
            );
        }
    }
}
```

- [ ] **Step 6: Update scene_walk.rs — per-node baseline and atlas lookup**

In `scene_walk.rs`, the `Content::Glyphs` match (line ~489):

```rust
Content::Glyphs {
    color, glyphs, glyph_count, font_size, style_id,
} => {
    // Per-node baseline from registry.
    let baseline_y = if let Some(entry) = ctx.registry_lookup(style_id) {
        abs_y + (entry.ascent_fu as f32 * font_size as f32 / entry.upem as f32)
    } else {
        abs_y + font_size as f32  // fallback
    };

    let font_size_px = round_font_size(font_size, ctx.scale_factor);

    // Walk glyphs with pen cursor.
    let mut pen_x = abs_x;
    for sg in shaped {
        if let Some(entry) = ctx.atlas.lookup(sg.glyph_id, font_size_px, style_id) {
            // ... existing glyph quad emission, using baseline_y ...
        }
        pen_x += sg.x_advance as f32 / fp16;
    }
}
```

Remove `font_ascent: u32` from `RenderContext`. Remove the global `font_ascent` computation from `main.rs`.

- [ ] **Step 7: Build**

Run: `cd system && cargo build --release`
Expected: Compiles cleanly

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(metal-render): per-node style rendering with hash-map atlas"
```

---

### Task 8: Visual Verification — TDD Loop

**Files:**
- Modify: `tools/mkdisk/main.rs` (if sample document needs richer test content)

**Context:** This is the acceptance test. We capture screenshots and use `imgdiff.py` to numerically verify that styled text renders correctly. Every assertion must be backed by a number, not eyeballing.

- [ ] **Step 1: Capture baseline (before fix should show garbled text)**

We already have `/tmp/v05-baseline.png` from earlier in this session. If needed, recapture:

```bash
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/v05-baseline.png --timeout 30
python3 test/imgdiff.py /tmp/v05-baseline.png
```

- [ ] **Step 2: Build with all fixes and capture**

```bash
cd system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/v05-styled.png --timeout 30
python3 test/imgdiff.py /tmp/v05-styled.png
```

Verify: heading text is larger than body text (different vertical extent in imgdiff output), no overlapping text at the top of the page.

- [ ] **Step 3: Compare before/after**

```bash
python3 test/imgdiff.py /tmp/v05-baseline.png /tmp/v05-styled.png
```

Expected: significant pixel diff (styled text renders differently). The heading should occupy more vertical space, text should not overlap.

- [ ] **Step 4: Test Cmd+B bold toggle**

Create event script:

```bash
cat > /tmp/bold-test.events << 'SCRIPT'
wait 30
type Hello World
key cmd+a
wait 5
capture /tmp/before-bold.png
key cmd+b
wait 5
capture /tmp/after-bold.png
SCRIPT

hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/bold-test.events --timeout 60
python3 test/imgdiff.py /tmp/before-bold.png /tmp/after-bold.png
```

Expected: pixel diff > 0. Bold text should be measurably heavier (more dark pixels in the glyph regions).

- [ ] **Step 5: Test no regression on text/plain**

```bash
cat > /tmp/plain-test.events << 'SCRIPT'
wait 30
key ctrl+tab
wait 10
capture /tmp/plain-text.png
SCRIPT

hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/plain-test.events --timeout 60
python3 test/imgdiff.py /tmp/plain-text.png
```

Expected: text/plain document (second tab) renders normally, no crash.

- [ ] **Step 6: Run full test suite**

```bash
cd system/test && cargo test -- --test-threads=1
```

Expected: All ~2,320+ tests pass (including new tests from Tasks 1-3).

- [ ] **Step 7: Final commit**

```bash
git add -A
git commit -m "test: visual verification of rich text rendering"
```

---

## Summary

| Task | Phase | Description | New Tests |
|------|-------|-------------|-----------|
| 1 | Foundation | HVAR parser for variation-aware advances | 4 |
| 2 | Foundation | Style registry types + serialization | 4 |
| 3 | Foundation | Hash-map glyph atlas | 7 |
| 4 | Core | Rename axis_hash → style_id (mechanical) | 0 |
| 5 | Core | StyleTable + FontInfo + registry writing | 0 (integration) |
| 6 | Core | Layout fixes (x-offset, line height, HVAR) | 0 (visual) |
| 7 | Renderer | Metal-render full integration | 0 (integration) |
| 8 | Verify | Visual TDD with hypervisor captures | 5 visual |

**Total estimated new tests:** 15 unit + 5 visual assertions
