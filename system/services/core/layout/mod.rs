//! Layout algorithm and scene building.
//!
//! Provides monospace line-breaking, cursor/selection positioning, glyph
//! shaping, scroll filtering, and the scene graph building functions that
//! assemble the text editor UI. All scene mutation flows through free
//! functions that accept a `SceneWriter` — `SceneState` delegates here.

mod full;
mod incremental;

use alloc::vec::Vec;

// Re-export all public items from submodules.
pub use full::{
    build_clock_update, build_cursor_update, build_document_content, build_full_scene,
    build_selection_update,
};
pub use incremental::{delete_line, insert_line, update_single_line};
use scene::{Color, Content, DataRef, NodeFlags, ShapedGlyph, NULL};

// ── Float math helpers (no_std) ─────────────────────────────────────

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Delegates to the canonical implementation in `drawing`.
#[inline]
pub(crate) fn round_f32(x: f32) -> i32 {
    drawing::round_f32(x)
}

// ── Well-known node indices ─────────────────────────────────────────

/// Well-known node indices for direct mutation.
pub const N_ROOT: u16 = 0;
pub const N_TITLE_BAR: u16 = 1;
pub const N_TITLE_TEXT: u16 = 2;
pub const N_CLOCK_TEXT: u16 = 3;
pub const N_SHADOW: u16 = 4;
pub const N_CONTENT: u16 = 5;
pub const N_DOC_TEXT: u16 = 6;
pub const N_CURSOR: u16 = 7;

// ── Pointer cursor node (8) ──────────────────────────────────────────
//
// Top-level node rendered above all content. Position updated each
// frame from MSG_POINTER_ABS. Auto-hides after 3 s of inactivity with
// a 300 ms EaseOut fade.
pub const N_POINTER: u16 = 8;

/// Number of well-known nodes (indices 0..8). Dynamic nodes start at 9.
pub const WELL_KNOWN_COUNT: u16 = 9;

/// Font axis_hash value indicating the sans font (Inter).
/// Render backends use this to select the correct font for rasterization.
/// 0 = mono (JetBrains Mono, default), 1 = sans (Inter).
pub const FONT_SANS: u32 = 1;

// ── Configuration ───────────────────────────────────────────────────

/// Shared configuration for scene building functions. Avoids passing
/// 25+ parameters individually to build_editor_scene, update_document_content,
/// update_selection, and update_clock.
pub struct SceneConfig<'a> {
    pub fb_width: u32,
    pub fb_height: u32,
    pub title_bar_h: u32,
    pub shadow_depth: u32,
    pub text_inset_x: u32,
    pub chrome_bg: drawing::Color,
    pub chrome_border: drawing::Color,
    pub chrome_title_color: drawing::Color,
    pub chrome_clock_color: drawing::Color,
    pub bg_color: drawing::Color,
    pub text_color: drawing::Color,
    pub cursor_color: drawing::Color,
    pub sel_color: drawing::Color,
    pub font_size: u16,
    /// Character advance in 16.16 fixed-point points.
    /// Single source of truth for character width — same precision as
    /// scene graph ShapedGlyph advances.
    pub char_width_fx: i32,
    pub line_height: u32,
    pub font_data: &'a [u8],
    pub upem: u16,
    pub axes: &'a [fonts::rasterize::AxisValue],
    /// Sans font data (Inter) for chrome text (title, clock).
    /// Falls back to font_data (mono) when empty.
    pub sans_font_data: &'a [u8],
    pub sans_upem: u16,
}

// ── Layout types ────────────────────────────────────────────────────

/// Local layout run type — used for line-breaking before writing to
/// the scene graph. Each LayoutRun describes one visual text line.
pub struct LayoutRun {
    /// Placeholder DataRef: offset = byte position in source text,
    /// length = byte count. Replaced with actual data buffer ref
    /// before writing to the scene graph.
    pub glyphs: DataRef,
    /// Number of glyphs (= bytes for monospace ASCII).
    pub glyph_count: u16,
    /// Starting point position relative to the parent node.
    pub y: i32,
    /// Text color.
    pub color: Color,
    /// Font size in points.
    pub font_size: u16,
}

// ── Text layout (delegates to layout library) ─────────────────────

/// Monospace font metrics for the layout library.
///
/// Every character has the same advance width. The `char_width` is set
/// to 1.0 so that `max_width = chars_per_line` — the library wraps at
/// the same character boundaries as the old hand-written code.
struct UnitMetrics {
    line_height: f32,
}

impl layout_lib::FontMetrics for UnitMetrics {
    fn char_width(&self, _ch: char) -> f32 {
        1.0
    }
    fn line_height(&self) -> f32 {
        self.line_height
    }
}

/// Convert a byte offset to (visual_line, column) with monospace wrapping.
///
/// Delegates to the layout library. The single source of truth for
/// line-breaking logic — used by both scene building (cursor/selection
/// positioning) and scroll calculation.
pub fn byte_to_line_col(text: &[u8], byte_offset: usize, chars_per_line: usize) -> (usize, usize) {
    let metrics = UnitMetrics { line_height: 1.0 };
    let max_width = chars_per_line as f32;
    layout_lib::byte_to_line_col(
        text,
        byte_offset,
        &metrics,
        max_width,
        &layout_lib::CharBreaker,
    )
}

/// Break text into visual lines using the unified layout library.
///
/// Delegates to `layout_lib::layout_paragraph` with `CharBreaker` (character-
/// level wrapping) and unit-width metrics, then wraps each `LayoutLine`
/// into a `LayoutRun` with color and font_size for scene graph construction.
pub fn layout_mono_lines(
    text: &[u8],
    chars_per_line: usize,
    line_height: i32,
    color: Color,
    font_size: u16,
) -> Vec<LayoutRun> {
    let metrics = UnitMetrics {
        line_height: line_height as f32,
    };
    let max_width = chars_per_line as f32;
    let para = layout_lib::layout_paragraph(
        text,
        &metrics,
        max_width,
        layout_lib::Alignment::Left,
        &layout_lib::CharBreaker,
    );

    para.lines
        .iter()
        .map(|line| LayoutRun {
            glyphs: DataRef {
                offset: line.byte_offset,
                length: line.byte_length,
            },
            glyph_count: line.byte_length as u16,
            y: line.y,
            color,
            font_size,
        })
        .collect()
}

/// Extract source text bytes for a run using its placeholder DataRef.
pub fn line_bytes_for_run<'a>(text: &'a [u8], run: &LayoutRun) -> &'a [u8] {
    let start = run.glyphs.offset as usize;
    let len = run.glyphs.length as usize;

    if start + len <= text.len() {
        &text[start..start + len]
    } else {
        &[]
    }
}

/// Filter runs to those visible in a scrolled viewport.
///
/// `scroll_y` is the scroll offset in pixels (f32). Runs keep their
/// document-relative y positions. The caller sets `content_transform`
/// on the container node so the renderer handles the viewport offset.
pub fn scroll_runs(
    runs: Vec<LayoutRun>,
    scroll_y: f32,
    line_height: u32,
    viewport_height_pt: i32,
) -> Vec<LayoutRun> {
    let scroll_pt = round_f32(scroll_y);

    runs.into_iter()
        .filter(|run| {
            let doc_y = run.y;

            // Above the scroll window?
            if doc_y + line_height as i32 <= scroll_pt {
                return false;
            }
            // Below the scroll window?
            if doc_y >= scroll_pt + viewport_height_pt {
                return false;
            }

            true
        })
        .collect()
}

/// Shape text through HarfBuzz and convert from font units to points.
///
/// Calls `fonts::shape_with_variations()` to produce real glyph IDs and
/// metrics, then converts font-unit values to scene-graph points using
/// `value_pt = value_fu * point_size / upem`.
pub fn shape_text(
    font_data: &[u8],
    text: &[u8],
    point_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> Vec<ShapedGlyph> {
    // Use lossy conversion so invalid UTF-8 bytes render as replacement
    // characters instead of causing the entire line to disappear (review 6.7).
    let s = alloc::string::String::from_utf8_lossy(text);
    if s.is_empty() || font_data.is_empty() || upem == 0 {
        return Vec::new();
    }
    let shaped = fonts::shape_with_variations(font_data, &s, &[], axes);
    let ps = point_size as i64;
    let u = upem as i64;
    // Convert font units to 16.16 fixed-point points:
    //   value_16_16 = (value_fu * point_size * 65536) / upem
    // Using i64 to avoid overflow for large font-unit values.
    shaped
        .iter()
        .map(|g| ShapedGlyph {
            glyph_id: g.glyph_id,
            _pad: 0,
            x_advance: ((g.x_advance as i64 * ps * 65536) / u) as i32,
            x_offset: ((g.x_offset as i64 * ps * 65536) / u) as i32,
            y_offset: ((g.y_offset as i64 * ps * 65536) / u) as i32,
        })
        .collect()
}

/// Count lines in a text buffer (newlines + 1).
pub fn count_lines(text: &[u8]) -> usize {
    let mut count: usize = 1;
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'\n' {
            count += 1;
        }
        i += 1;
    }
    count
}

/// Convert a `drawing::Color` to the scene graph `Color` type.
pub(crate) fn dc(c: drawing::Color) -> Color {
    Color::rgba(c.r, c.g, c.b, c.a)
}

/// Compute `chars_per_line` from config using fractional char width.
pub(crate) fn chars_per_line(cfg: &SceneConfig) -> u32 {
    let doc_width = cfg.fb_width.saturating_sub(2 * cfg.text_inset_x);
    if cfg.char_width_fx > 0 {
        ((doc_width as i64 * 65536) / cfg.char_width_fx as i64).max(1) as u32
    } else {
        80
    }
}

/// Compute the document-area width from config.
pub(crate) fn doc_width(cfg: &SceneConfig) -> u32 {
    cfg.fb_width.saturating_sub(2 * cfg.text_inset_x)
}

/// Allocate per-line Glyphs child nodes under N_DOC_TEXT, linking them
/// as a sibling chain. Returns the last allocated line node ID (or NULL
/// if none were allocated).
///
/// This is the shared line-node construction code used by both
/// `build_full_scene` and `build_document_content`.
pub(crate) fn allocate_line_nodes(
    w: &mut scene::SceneWriter<'_>,
    line_glyph_refs: &[(DataRef, u16, i32)],
    doc_width: u32,
    line_height: u32,
    scene_text_color: Color,
    font_size: u16,
) -> u16 {
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    let mut prev_line_node: u16 = NULL;

    for &(glyph_ref, glyph_count, y) in line_glyph_refs {
        if let Some(line_id) = w.alloc_node() {
            let n = w.node_mut(line_id);
            n.y = y;
            n.width = doc_width as u16;
            n.height = line_height as u16;
            n.content = Content::Glyphs {
                color: scene_text_color,
                glyphs: glyph_ref,
                glyph_count,
                font_size,
                axis_hash: 0,
            };
            n.content_hash = scene::fnv1a(&glyph_ref.offset.to_le_bytes());
            n.flags = NodeFlags::VISIBLE;
            n.next_sibling = NULL;

            if prev_line_node == NULL {
                w.node_mut(N_DOC_TEXT).first_child = line_id;
            } else {
                w.node_mut(prev_line_node).next_sibling = line_id;
            }
            prev_line_node = line_id;
        }
    }

    prev_line_node
}

/// Shape visible runs and collect glyph data refs for line-node
/// construction. Used by both full and incremental scene builds.
pub(crate) fn shape_visible_runs(
    w: &mut scene::SceneWriter<'_>,
    visible_runs: &[LayoutRun],
    doc_text: &[u8],
    font_data: &[u8],
    font_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> Vec<(DataRef, u16, i32)> {
    let mut line_glyph_refs: Vec<(DataRef, u16, i32)> = Vec::with_capacity(visible_runs.len());

    for run in visible_runs {
        let line_text = line_bytes_for_run(doc_text, run);
        let shaped = shape_text(font_data, line_text, font_size, upem, axes);
        let glyph_ref = w.push_shaped_glyphs(&shaped);

        line_glyph_refs.push((glyph_ref, shaped.len() as u16, run.y));
    }

    line_glyph_refs
}

/// Update the clock text via re-push within an already-open back buffer.
/// Real shaping may produce different glyph counts, so we re-push data
/// and update the Content::Glyphs reference rather than overwriting in place.
pub(crate) fn update_clock_inline(
    w: &mut scene::SceneWriter<'_>,
    clock_text: &[u8],
    font_data: &[u8],
    font_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Glyphs { color, .. } = clock_node.content {
        let new_glyphs = shape_text(font_data, clock_text, font_size, upem, axes);
        let new_ref = w.push_shaped_glyphs(&new_glyphs);
        let new_count = new_glyphs.len() as u16;

        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size,
            axis_hash: FONT_SANS,
        };
        n.content_hash = scene::fnv1a(clock_text);
        w.mark_dirty(N_CLOCK_TEXT);
    }
}

/// Allocate selection rectangle nodes as children of N_DOC_TEXT (after
/// the cursor node). Each line of the selection gets one rect node.
///
/// Selection rects use document-relative y positions. The renderer
/// applies `content_transform` from the parent container to offset them
/// visually. `scroll_pt` and `content_h` are used only for visibility
/// culling.
#[allow(clippy::too_many_arguments)]
pub(crate) fn allocate_selection_rects(
    w: &mut scene::SceneWriter<'_>,
    doc_text: &[u8],
    sel_lo: usize,
    sel_hi: usize,
    chars_per_line: usize,
    char_width_fx: i32,
    line_height: u32,
    sel_color: Color,
    content_h: u32,
    scroll_pt: i32,
) {
    let (sel_start_line, sel_start_col) = byte_to_line_col(doc_text, sel_lo, chars_per_line);
    let (sel_end_line, sel_end_col) = byte_to_line_col(doc_text, sel_hi, chars_per_line);
    let mut prev_sel_node: u16 = NULL;

    for line in sel_start_line..=sel_end_line {
        let col_start = if line == sel_start_line {
            sel_start_col
        } else {
            0
        };
        let col_end = if line == sel_end_line {
            sel_end_col
        } else {
            chars_per_line
        };

        if col_start >= col_end {
            continue;
        }

        // Document-relative y for this selection line.
        let sel_y = line as i32 * line_height as i32;

        // Visibility culling: skip lines outside the scroll window.
        if sel_y + line_height as i32 <= scroll_pt || sel_y >= scroll_pt + content_h as i32 {
            continue;
        }

        if let Some(sel_id) = w.alloc_node() {
            let n = w.node_mut(sel_id);
            n.x = ((col_start as i64 * char_width_fx as i64) >> 16) as i32;
            n.y = sel_y;
            n.width = (((col_end - col_start) as i64 * char_width_fx as i64) >> 16) as u16;
            n.height = line_height as u16;
            n.background = sel_color;
            n.content = Content::None;
            n.flags = NodeFlags::VISIBLE;
            n.next_sibling = NULL;

            if prev_sel_node == NULL {
                w.node_mut(N_CURSOR).next_sibling = sel_id;
            } else {
                w.node_mut(prev_sel_node).next_sibling = sel_id;
            }

            w.mark_dirty(sel_id);
            prev_sel_node = sel_id;
        }
    }
}
