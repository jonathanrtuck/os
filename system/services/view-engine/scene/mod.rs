//! Scene graph building and view-state utilities.
//!
//! Provides the scene graph construction functions that assemble the text
//! editor UI. Layout computation is handled by the layout engine (B) —
//! this module reads B's pre-computed layout results from shared memory.
//! All scene mutation flows through free functions that accept a
//! `SceneWriter` — `SceneState` delegates here.

mod full;
mod incremental;
mod loading;

use alloc::vec::Vec;

pub(crate) use full::rich_xy_to_byte;
// Re-export all public items from submodules.
pub use full::{
    build_clock_update, build_cursor_update, build_document_content, build_full_scene,
    build_rich_document_content, build_selection_update,
};
use icon_lib as icons;
pub use incremental::{delete_line, insert_line, update_single_line};
pub use loading::{build_loading_scene, update_spinner_angle};
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

// Pointer cursor (formerly node 8) is no longer in the scene graph.
// Cursor shape and opacity flow through the CursorState shared-memory
// register; position flows through PointerState. The render driver
// rasterizes cursor icons via the normal GPU pipeline and manages the
// hardware cursor plane independently.

// ── Title bar icon (8) ──────────────────────────────────────────────
//
// Document type icon in the title bar, baseline-aligned with the title
// text. Content::Path with stroke_width > 0 for outline Tabler icons.
pub const N_TITLE_ICON: u16 = 8;

// ── Document strip (9..11) ──────────────────────────────────────────
//
// Horizontal strip of document spaces. N_STRIP is a child of N_CONTENT
// with width = N × viewport. child_offset_x slides the viewport.
// Each document occupies one viewport-width "space" in the strip.
pub const N_STRIP: u16 = 9;

// White page surface for the text document (space 0). A4 proportions,
// centered horizontally. N_DOC_TEXT is a child of this node.
pub const N_PAGE: u16 = 10;

// Image content in space 1. Centered in the second viewport-width
// region of the strip. The image IS its own surface (no page bg).
pub const N_DOC_IMAGE: u16 = 11;

/// Number of well-known nodes (indices 0..11). Dynamic nodes start at 12.
pub const WELL_KNOWN_COUNT: u16 = 12;

// ── Configuration ───────────────────────────────────────────────────

/// Shared configuration for scene building functions. Avoids passing
/// 25+ parameters individually to build_editor_scene, update_document_content,
/// update_selection, and update_clock.
pub struct SceneConfig<'a> {
    pub fb_width: u32,
    pub fb_height: u32,
    pub title_bar_h: u32,
    pub shadow_depth: u32,
    /// Text inset within the page surface (padding from page edge).
    pub text_inset_x: u32,
    pub chrome_bg: drawing::Color,
    pub chrome_border: drawing::Color,
    pub chrome_title_color: drawing::Color,
    pub chrome_clock_color: drawing::Color,
    pub bg_color: drawing::Color,
    pub text_color: drawing::Color,
    pub cursor_color: drawing::Color,
    pub sel_color: drawing::Color,
    /// Page surface background color (white paper).
    pub page_bg: drawing::Color,
    /// Page width in points (A4 proportions, derived from viewport).
    pub page_width: u32,
    /// Page height in points.
    pub page_height: u32,
    pub font_size: u16,
    /// Character advance in 16.16 fixed-point points.
    /// Single source of truth for character width — same precision as
    /// scene graph ShapedGlyph advances.
    pub char_width_fx: i32,
    pub line_height: u32,
    pub font_data: &'a [u8],
    pub upem: u16,
    pub axes: &'a [fonts::rasterize::AxisValue],
    /// Content Region content_id for the mono font.
    pub mono_content_id: u32,
    /// Mono font typographic ascent (font units, positive).
    pub mono_ascender: i16,
    /// Mono font typographic descent (font units, negative).
    pub mono_descender: i16,
    /// Mono font line gap (font units).
    pub mono_line_gap: i16,
    /// Sans font data (Inter) for chrome text (title, clock).
    /// Falls back to font_data (mono) when empty.
    pub sans_font_data: &'a [u8],
    pub sans_upem: u16,
    /// Content Region content_id for the sans font.
    pub sans_content_id: u32,
    /// Sans font typographic ascent (font units, positive).
    pub sans_ascender: i16,
    /// Sans font typographic descent (font units, negative).
    pub sans_descender: i16,
    /// Sans font line gap (font units).
    pub sans_line_gap: i16,
}

// ── Icon rendering ─────────────────────────────────────────────────

/// Scale pre-compiled icon path data from viewbox space to point space
/// and push to the scene writer's data buffer. Returns the DataRef for
/// use in `Content::Path`.
///
/// Concatenates all sub-paths from the icon into a single contour blob.
/// Color is applied uniformly (monochrome). For layered rendering with
/// per-layer opacity, use separate nodes per layer group.
fn scale_icon_paths(
    w: &mut scene::SceneWriter<'_>,
    icon: &icons::Icon,
    size_pt: u32,
) -> (DataRef, u32) {
    let scale = size_pt as f32 / icon.viewbox;
    let mut buf = Vec::new();

    for icon_path in icon.paths {
        let cmds = icon_path.commands;
        let mut pos = 0;
        while pos + 4 <= cmds.len() {
            let tag = u32::from_le_bytes([cmds[pos], cmds[pos + 1], cmds[pos + 2], cmds[pos + 3]]);
            match tag {
                0 => {
                    // MoveTo: tag(4) + x(4) + y(4) = 12
                    if pos + 12 > cmds.len() {
                        break;
                    }
                    let x = f32::from_le_bytes([
                        cmds[pos + 4],
                        cmds[pos + 5],
                        cmds[pos + 6],
                        cmds[pos + 7],
                    ]) * scale;
                    let y = f32::from_le_bytes([
                        cmds[pos + 8],
                        cmds[pos + 9],
                        cmds[pos + 10],
                        cmds[pos + 11],
                    ]) * scale;
                    buf.extend_from_slice(&0u32.to_le_bytes());
                    buf.extend_from_slice(&x.to_le_bytes());
                    buf.extend_from_slice(&y.to_le_bytes());
                    pos += 12;
                }
                1 => {
                    // LineTo: tag(4) + x(4) + y(4) = 12
                    if pos + 12 > cmds.len() {
                        break;
                    }
                    let x = f32::from_le_bytes([
                        cmds[pos + 4],
                        cmds[pos + 5],
                        cmds[pos + 6],
                        cmds[pos + 7],
                    ]) * scale;
                    let y = f32::from_le_bytes([
                        cmds[pos + 8],
                        cmds[pos + 9],
                        cmds[pos + 10],
                        cmds[pos + 11],
                    ]) * scale;
                    buf.extend_from_slice(&1u32.to_le_bytes());
                    buf.extend_from_slice(&x.to_le_bytes());
                    buf.extend_from_slice(&y.to_le_bytes());
                    pos += 12;
                }
                2 => {
                    // CubicTo: tag(4) + c1x(4) + c1y(4) + c2x(4) + c2y(4) + x(4) + y(4) = 28
                    if pos + 28 > cmds.len() {
                        break;
                    }
                    let mut coords = [0f32; 6];
                    for ci in 0..6 {
                        let off = pos + 4 + ci * 4;
                        coords[ci] = f32::from_le_bytes([
                            cmds[off],
                            cmds[off + 1],
                            cmds[off + 2],
                            cmds[off + 3],
                        ]) * scale;
                    }
                    buf.extend_from_slice(&2u32.to_le_bytes());
                    for c in &coords {
                        buf.extend_from_slice(&c.to_le_bytes());
                    }
                    pos += 28;
                }
                3 => {
                    // Close: tag(4) = 4
                    buf.extend_from_slice(&3u32.to_le_bytes());
                    pos += 4;
                }
                _ => break,
            }
        }
    }

    let hash = scene::fnv1a(&buf);
    let data_ref = w.push_data(&buf);
    (data_ref, hash)
}

/// Set up a scene node as an icon using Content::Path.
pub(crate) fn emit_icon(
    w: &mut scene::SceneWriter<'_>,
    node_id: u16,
    icon: &icons::Icon,
    x: i32,
    y: i32,
    size_pt: u32,
    color: Color,
    stroke_color: Color,
) {
    let (data_ref, hash) = scale_icon_paths(w, icon, size_pt);

    // Stroke width: scale from viewbox units to points, encode as 8.8 fixed-point.
    let sw_pt = icon.stroke_width * (size_pt as f32 / icon.viewbox);
    let sw_fixed = (sw_pt * 256.0) as u16;

    let n = w.node_mut(node_id);
    n.x = scene::pt(x);
    n.y = scene::pt(y as i32);
    n.width = scene::upt(size_pt);
    n.height = scene::upt(size_pt);
    n.content = Content::Path {
        color,
        stroke_color,
        fill_rule: scene::FillRule::Winding,
        stroke_width: sw_fixed,
        contours: data_ref,
    };
    n.content_hash = hash;
    n.flags = NodeFlags::VISIBLE;
}

// ── Text layout (delegates to layout library for trivial ops) ──────

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

// ── Style table (sequential ID assignment) ──────────────────────────

/// A registered (content_id, axes) combination with font metrics.
struct StyleEntry {
    content_id: u32,
    axes: Vec<fonts::rasterize::AxisValue>,
    ascent_fu: u16,
    descent_fu: u16,
    upem: u16,
}

/// Sequential style ID assignment. Collision-free by construction.
pub struct StyleTable {
    entries: Vec<StyleEntry>,
}

impl StyleTable {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn style_id_for(
        &mut self,
        content_id: u32,
        axes: &[fonts::rasterize::AxisValue],
        ascent_fu: u16,
        descent_fu: u16,
        upem: u16,
    ) -> u32 {
        for (i, e) in self.entries.iter().enumerate() {
            if e.content_id == content_id && axes_eq(&e.axes, axes) {
                return i as u32;
            }
        }
        let id = self.entries.len() as u32;
        self.entries.push(StyleEntry {
            content_id,
            axes: axes.to_vec(),
            ascent_fu,
            descent_fu,
            upem,
        });
        id
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn to_registry_entries(&self) -> Vec<protocol::content::StyleRegistryEntry> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut axes = [protocol::content::StyleAxisValue {
                    tag: [0; 4],
                    value: 0.0,
                }; protocol::content::MAX_STYLE_AXES];
                let axis_count = e.axes.len().min(protocol::content::MAX_STYLE_AXES);
                for (j, av) in e.axes.iter().take(axis_count).enumerate() {
                    axes[j] = protocol::content::StyleAxisValue {
                        tag: av.tag,
                        value: av.value,
                    };
                }
                protocol::content::StyleRegistryEntry {
                    style_id: i as u32,
                    content_id: e.content_id,
                    ascent_fu: e.ascent_fu,
                    descent_fu: e.descent_fu,
                    upem: e.upem,
                    axis_count: axis_count as u8,
                    _pad: 0,
                    weight: 400,
                    caret_skew: 0,
                    axes,
                }
            })
            .collect()
    }
}

fn axes_eq(a: &[fonts::rasterize::AxisValue], b: &[fonts::rasterize::AxisValue]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (av, bv) in a.iter().zip(b.iter()) {
        if av.tag != bv.tag || av.value != bv.value {
            return false;
        }
    }
    true
}

/// Write the style registry into the scene data buffer as the first item.
pub(crate) fn write_style_registry(
    w: &mut scene::SceneWriter<'_>,
    style_table: &StyleTable,
) -> usize {
    let registry_entries = style_table.to_registry_entries();
    let mut registry_buf = [0u8; 8192];
    let registry_size =
        protocol::content::write_style_registry(&mut registry_buf, &registry_entries);
    if registry_size > 0 {
        let _registry_ref = w.push_data(&registry_buf[..registry_size]);
    }
    registry_size
}

/// Create a StyleTable with the two base styles (mono=0, sans=1).
pub(crate) fn base_style_table(cfg: &SceneConfig) -> (StyleTable, u32, u32) {
    let mut st = StyleTable::new();
    let mono_id = st.style_id_for(
        cfg.mono_content_id,
        cfg.axes,
        cfg.mono_ascender as u16,
        (-cfg.mono_descender) as u16,
        cfg.upem,
    );
    let sans_id = st.style_id_for(
        cfg.sans_content_id,
        &[],
        cfg.sans_ascender as u16,
        (-cfg.sans_descender) as u16,
        cfg.sans_upem,
    );
    (st, mono_id, sans_id)
}

// ── Chrome text shaping ─────────────────────────────────────────────

/// Shape text through HarfBuzz and convert from font units to points.
pub fn shape_text(
    font_data: &[u8],
    text: &[u8],
    point_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> Vec<ShapedGlyph> {
    let s = alloc::string::String::from_utf8_lossy(text);
    if s.is_empty() || font_data.is_empty() || upem == 0 {
        return Vec::new();
    }
    let shaped = fonts::shape_with_variations(font_data, &s, &[], axes);
    let ps = point_size as i64;
    let u = upem as i64;
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

/// Shape text using the chrome font (Inter/sans).
pub(crate) fn shape_chrome_text(cfg: &SceneConfig, text: &[u8]) -> Vec<ShapedGlyph> {
    shape_text(
        cfg.sans_font_data,
        text,
        cfg.font_size,
        cfg.sans_upem,
        cfg.axes,
    )
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

// ── LineInfo-based navigation (replaces RichLine navigation) ────────
//
// These functions use B's pre-computed LineInfo entries (cached in
// CoreState) for cursor navigation in rich text. They replace the old
// RichLine-based functions that required local layout computation.

/// Check if a visible run belongs to a given line by byte range overlap.
/// After baseline alignment, runs on the same line may have different y_pt
/// values, so we can't use y_pt equality — byte ranges are authoritative.
#[inline]
fn run_on_line(run: &protocol::layout::VisibleRun, li: &protocol::layout::LineInfo) -> bool {
    let line_start = li.byte_offset as usize;
    let line_end = line_start + li.byte_length as usize;
    let run_start = run.byte_offset as usize;
    let run_end = run_start + run.byte_length as usize;
    run_start < line_end && run_end > line_start
}

/// Find which line index contains `cursor_pos`. Returns total_lines
/// (one past last) if cursor is past all lines.
pub(crate) fn line_info_byte_to_line(
    lines: &[protocol::layout::LineInfo],
    cursor_pos: usize,
) -> usize {
    for (i, li) in lines.iter().enumerate() {
        let start = li.byte_offset as usize;
        let end = start + li.byte_length as usize;
        if cursor_pos >= start && cursor_pos <= end {
            return i;
        }
    }
    lines.len()
}

/// Byte offset of the start of a line.
pub(crate) fn line_info_start(
    lines: &[protocol::layout::LineInfo],
    line_idx: usize,
) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let idx = line_idx.min(lines.len() - 1);
    lines[idx].byte_offset as usize
}

/// Byte offset of the end of a line.
pub(crate) fn line_info_end(
    lines: &[protocol::layout::LineInfo],
    line_idx: usize,
) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let idx = line_idx.min(lines.len() - 1);
    let li = &lines[idx];
    (li.byte_offset + li.byte_length as u32) as usize
}

/// Count how many lines fit in a viewport of `viewport_height_pt` points.
pub(crate) fn line_info_viewport_lines(
    lines: &[protocol::layout::LineInfo],
    viewport_height_pt: i32,
) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let total_h: i32 = lines.iter().map(|l| l.line_height_pt as i32).sum();
    let avg_h = total_h / lines.len() as i32;
    if avg_h <= 0 {
        return 1;
    }
    (viewport_height_pt / avg_h).max(1) as usize
}

/// Compute cursor x position (millipoints) at `cursor_pos` using
/// B's pre-shaped VisibleRun glyph data from shared memory.
pub(crate) fn line_info_cursor_x_mpt(
    lines: &[protocol::layout::LineInfo],
    cursor_pos: usize,
) -> i32 {
    let line_idx = line_info_byte_to_line(lines, cursor_pos);
    if line_idx >= lines.len() {
        return 0;
    }
    let li = &lines[line_idx];
    let line_start = li.byte_offset as usize;

    // Read VisibleRuns from B for this line.
    let header = match crate::read_layout_header() {
        Some(h) => h,
        None => return 0,
    };

    let mut x_mpt: i32 = 0;
    for ri in 0..header.visible_run_count as usize {
        let run = crate::read_visible_run(&header, ri);
        if !run_on_line(&run, li) {
            continue;
        }
        let run_start = run.byte_offset as usize;
        let run_end = run_start + run.byte_length as usize;

        if cursor_pos <= run_start {
            // Cursor is before this run — use accumulated x.
            return run.x_mpt;
        }

        if cursor_pos >= run_end {
            // Past this run — accumulate full run width.
            let glyphs = crate::read_glyph_data(&header, run.glyph_data_offset, run.glyph_count);
            x_mpt = run.x_mpt;
            for g in glyphs {
                x_mpt += g.x_advance >> 6; // 16.16 → millipoints
            }
            continue;
        }

        // Cursor is within this run. Walk text and glyphs together.
        let doc = crate::doc_text_for_range(run_start, run_end);
        let glyphs = crate::read_glyph_data(&header, run.glyph_data_offset, run.glyph_count);
        x_mpt = run.x_mpt;
        let mut byte_pos = run_start;
        let mut gi = 0usize;
        let s = core::str::from_utf8(doc).unwrap_or("");
        for ch in s.chars() {
            if byte_pos >= cursor_pos {
                break;
            }
            if gi < glyphs.len() {
                x_mpt += glyphs[gi].x_advance >> 6;
                gi += 1;
            }
            byte_pos += ch.len_utf8();
        }
        return x_mpt;
    }

    x_mpt
}

/// Given a line index and target x (millipoints), find the closest byte offset.
pub(crate) fn line_info_x_to_byte(
    lines: &[protocol::layout::LineInfo],
    line_idx: usize,
    target_x_mpt: i32,
) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let idx = line_idx.min(lines.len() - 1);
    let li = &lines[idx];

    let header = match crate::read_layout_header() {
        Some(h) => h,
        None => return li.byte_offset as usize,
    };

    let mut best_pos = li.byte_offset as usize;

    for ri in 0..header.visible_run_count as usize {
        let run = crate::read_visible_run(&header, ri);
        if !run_on_line(&run, li) {
            continue;
        }
        let run_start = run.byte_offset as usize;
        let run_end = run_start + run.byte_length as usize;
        let doc = crate::doc_text_for_range(run_start, run_end);
        let glyphs = crate::read_glyph_data(&header, run.glyph_data_offset, run.glyph_count);

        let mut x_mpt = run.x_mpt;
        let mut byte_pos = run_start;
        let s = core::str::from_utf8(doc).unwrap_or("");
        for (gi, ch) in s.chars().enumerate() {
            let advance = if gi < glyphs.len() {
                glyphs[gi].x_advance >> 6
            } else {
                0
            };
            if x_mpt + advance / 2 >= target_x_mpt {
                return byte_pos;
            }
            x_mpt += advance;
            byte_pos += ch.len_utf8();
        }
        best_pos = run_end;
    }

    best_pos
}

// ── Utility functions ───────────────────────────────────────────────

/// Convert a `drawing::Color` to the scene graph `Color` type.
pub(crate) fn dc(c: drawing::Color) -> Color {
    Color::rgba(c.r, c.g, c.b, c.a)
}

/// Compute `chars_per_line` from config using page width and text inset.
pub(crate) fn chars_per_line(cfg: &SceneConfig) -> u32 {
    let dw = doc_width(cfg);
    if cfg.char_width_fx > 0 {
        ((dw as i64 * 65536) / cfg.char_width_fx as i64).max(1) as u32
    } else {
        80
    }
}

/// Compute the text-area width within the page surface.
pub(crate) fn doc_width(cfg: &SceneConfig) -> u32 {
    cfg.page_width.saturating_sub(2 * cfg.text_inset_x)
}

/// Allocate per-line Glyphs child nodes under N_DOC_TEXT.
pub(crate) fn allocate_line_nodes(
    w: &mut scene::SceneWriter<'_>,
    line_glyph_refs: &[(DataRef, u16, i32)],
    doc_width: u32,
    line_height: u32,
    scene_text_color: Color,
    font_size: u16,
    style_id: u32,
) -> u16 {
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    let mut prev_line_node: u16 = NULL;

    for &(glyph_ref, glyph_count, y) in line_glyph_refs {
        if let Some(line_id) = w.alloc_node() {
            let n = w.node_mut(line_id);
            n.y = scene::pt(y);
            n.width = scene::upt(doc_width);
            n.height = scene::upt(line_height);
            n.content = Content::Glyphs {
                color: scene_text_color,
                glyphs: glyph_ref,
                glyph_count,
                font_size,
                style_id,
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

/// Read pre-shaped glyph data from layout engine (B) results and push
/// to the scene writer. Returns glyph refs for line-node construction.
pub(crate) fn push_layout_results_to_scene(
    w: &mut scene::SceneWriter<'_>,
    header: &protocol::layout::LayoutResultsHeader,
) -> Vec<(DataRef, u16, i32)> {
    let mut line_glyph_refs: Vec<(DataRef, u16, i32)> =
        Vec::with_capacity(header.visible_run_count as usize);

    for i in 0..header.visible_run_count as usize {
        let run = crate::read_visible_run(header, i);
        let glyphs = crate::read_glyph_data(header, run.glyph_data_offset, run.glyph_count);
        let glyph_ref = w.push_shaped_glyphs(glyphs);
        line_glyph_refs.push((glyph_ref, run.glyph_count, run.y_mpt));
    }

    line_glyph_refs
}

/// Update the clock text via re-push within an already-open back buffer.
pub(crate) fn update_clock_inline(
    w: &mut scene::SceneWriter<'_>,
    clock_text: &[u8],
    cfg: &SceneConfig,
) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Glyphs {
        color, style_id, ..
    } = clock_node.content
    {
        let new_glyphs = shape_chrome_text(cfg, clock_text);
        let new_ref = w.push_shaped_glyphs(&new_glyphs);
        let new_count = new_glyphs.len() as u16;

        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size: cfg.font_size,
            style_id,
        };
        n.content_hash = scene::fnv1a(clock_text);
        w.mark_dirty(N_CLOCK_TEXT);
    }
}

/// Allocate selection rectangle nodes as children of N_DOC_TEXT.
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

        let sel_y = line as i32 * line_height as i32;

        if sel_y + line_height as i32 <= scroll_pt || sel_y >= scroll_pt + content_h as i32 {
            continue;
        }

        if let Some(sel_id) = w.alloc_node() {
            let n = w.node_mut(sel_id);
            n.x = ((col_start as i64 * char_width_fx as i64) >> 6) as scene::Mpt;
            n.y = scene::pt(sel_y);
            n.width = (((col_end - col_start) as u64 * char_width_fx as u64) >> 6) as scene::Umpt;
            n.height = scene::upt(line_height);
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
