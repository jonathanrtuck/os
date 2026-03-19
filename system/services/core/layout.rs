//! Layout algorithm and scene building.
//!
//! Provides monospace line-breaking, cursor/selection positioning, glyph
//! shaping, scroll filtering, and the scene graph building functions that
//! assemble the text editor UI. All scene mutation flows through free
//! functions that accept a `SceneWriter` — `SceneState` delegates here.

use alloc::vec::Vec;

use scene::{fnv1a, Border, Color, Content, DataRef, FillRule, NodeFlags, ShapedGlyph, NULL};

use super::test_gen::{generate_test_image, generate_test_rounded_rect, generate_test_star};

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

/// Number of well-known nodes (indices 0..7). Dynamic nodes start at 8.
pub const WELL_KNOWN_COUNT: u16 = 8;

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
    pub text_inset_top: u32,
    pub chrome_bg: drawing::Color,
    pub chrome_border: drawing::Color,
    pub chrome_title_color: drawing::Color,
    pub chrome_clock_color: drawing::Color,
    pub bg_color: drawing::Color,
    pub text_color: drawing::Color,
    pub cursor_color: drawing::Color,
    pub sel_color: drawing::Color,
    pub font_size: u16,
    pub char_width: u32,
    pub line_height: u32,
    pub font_data: &'a [u8],
    pub upem: u16,
    pub axes: &'a [fonts::rasterize::AxisValue],
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
    /// Starting pixel position relative to the parent node.
    pub y: i16,
    /// Text color.
    pub color: Color,
    /// Font size in pixels.
    pub font_size: u16,
}

// ── Monospace text layout helpers ───────────────────────────────────

/// Convert a byte offset to (visual_line, column) with monospace wrapping.
/// This is the single source of truth for line-breaking logic — used by
/// both scene building (cursor/selection positioning) and scroll calculation.
pub fn byte_to_line_col(text: &[u8], byte_offset: usize, chars_per_line: usize) -> (usize, usize) {
    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut pos: usize = 0;

    while pos < text.len() && pos < byte_offset {
        if text[pos] == b'\n' {
            line += 1;
            col = 0;
            pos += 1;
        } else {
            col += 1;
            pos += 1;

            if col >= chars_per_line && pos < text.len() && text[pos] != b'\n' {
                line += 1;
                col = 0;
            }
        }
    }

    (line, col)
}

/// Break text into visual lines using monospace line-breaking.
pub fn layout_mono_lines(
    text: &[u8],
    chars_per_line: usize,
    line_height: i16,
    color: Color,
    font_size: u16,
) -> Vec<LayoutRun> {
    let mut runs = Vec::new();
    let mut line_y: i16 = 0;
    let mut pos: usize = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        let line_end = if let Some(nl) = remaining.iter().position(|&b| b == b'\n') {
            if nl <= chars_per_line {
                pos + nl
            } else {
                pos + chars_per_line
            }
        } else if remaining.len() <= chars_per_line {
            text.len()
        } else {
            pos + chars_per_line
        };
        let line_len = line_end - pos;

        runs.push(LayoutRun {
            glyphs: DataRef {
                offset: pos as u32,
                length: line_len as u32,
            },
            glyph_count: line_len as u16,
            y: line_y,
            color,
            font_size,
        });

        line_y = line_y.saturating_add(line_height);
        pos = if line_end < text.len() && text[line_end] == b'\n' {
            line_end + 1
        } else {
            line_end
        };
    }

    // If text ends with '\n', emit an empty run for the blank trailing line
    // so the cursor can be positioned there.
    if !text.is_empty() && text[text.len() - 1] == b'\n' {
        runs.push(LayoutRun {
            glyphs: DataRef {
                offset: text.len() as u32,
                length: 0,
            },
            glyph_count: 0,
            y: line_y,
            color,
            font_size,
        });
    }

    if runs.is_empty() {
        runs.push(LayoutRun {
            glyphs: DataRef {
                offset: 0,
                length: 0,
            },
            glyph_count: 0,
            y: 0,
            color,
            font_size,
        });
    }

    runs
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

/// Filter and reposition runs for a scrolled viewport.
pub fn scroll_runs(
    runs: Vec<LayoutRun>,
    scroll_lines: u32,
    line_height: u32,
    viewport_height_px: i32,
) -> Vec<LayoutRun> {
    let scroll_px = scroll_lines as i32 * line_height as i32;

    runs.into_iter()
        .filter_map(|run| {
            let adjusted_y = run.y as i32 - scroll_px;

            if adjusted_y + line_height as i32 <= 0 {
                return None;
            }
            if adjusted_y >= viewport_height_px {
                return None;
            }

            Some(LayoutRun {
                y: adjusted_y as i16,
                ..run
            })
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
    let ps = point_size as i32;
    let u = upem as i32;
    shaped
        .iter()
        .map(|g| ShapedGlyph {
            glyph_id: g.glyph_id,
            x_advance: ((g.x_advance * ps) / u) as i16,
            x_offset: ((g.x_offset * ps) / u) as i16,
            y_offset: ((g.y_offset * ps) / u) as i16,
        })
        .collect()
}

// ── Scene graph building functions ──────────────────────────────────

/// Update the clock text via re-push within an already-open back buffer.
/// Real shaping may produce different glyph counts, so we re-push data
/// and update the Content::Glyphs reference rather than overwriting in place.
pub fn update_clock_inline(
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
            axis_hash: 0,
        };
        n.content_hash = fnv1a(clock_text);
        w.mark_changed(N_CLOCK_TEXT);
    }
}

/// Allocate selection rectangle nodes as children of N_DOC_TEXT (after
/// the cursor node). Each line of the selection gets one rect node.
#[allow(clippy::too_many_arguments)]
pub fn allocate_selection_rects(
    w: &mut scene::SceneWriter<'_>,
    doc_text: &[u8],
    sel_lo: usize,
    sel_hi: usize,
    chars_per_line: usize,
    char_width: u32,
    line_height: u32,
    sel_color: Color,
    content_h: u32,
    scroll_px: i32,
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

        let sel_y = line as i32 * line_height as i32 - scroll_px;

        if sel_y + line_height as i32 <= 0 || sel_y >= content_h as i32 {
            continue;
        }

        if let Some(sel_id) = w.alloc_node() {
            let n = w.node_mut(sel_id);
            n.x = (col_start as u32 * char_width) as i16;
            n.y = sel_y as i16;
            n.width = ((col_end - col_start) as u32 * char_width) as u16;
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

            w.mark_changed(sel_id);
            prev_sel_node = sel_id;
        }
    }
}

// ── Full scene builds (called by SceneState methods) ────────────────

/// Build the full editor scene into a fresh (cleared) SceneWriter.
#[allow(clippy::too_many_arguments)]
pub fn build_full_scene(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    title_label: &[u8],
    clock_text: &[u8],
    scroll_y: i32,
) {
    let dc = |c: drawing::Color| -> Color { Color::rgba(c.r, c.g, c.b, c.a) };
    let scene_text_color = dc(cfg.text_color);
    // Layout document text into visual lines (monospace line-breaking).
    let doc_width = cfg.fb_width.saturating_sub(2 * cfg.text_inset_x);
    let chars_per_line = if cfg.char_width > 0 {
        (doc_width / cfg.char_width).max(1)
    } else {
        80
    };
    let all_runs = layout_mono_lines(
        doc_text,
        chars_per_line as usize,
        cfg.line_height as i16,
        scene_text_color,
        cfg.font_size,
    );
    // Apply scroll: filter to visible viewport, adjust y positions.
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y) as i32;
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let visible_runs = scroll_runs(all_runs, scroll_lines, cfg.line_height, content_h);
    // Scroll offset in pixels for cursor/selection positioning.
    let scroll_px = scroll_lines as i32 * cfg.line_height as i32;
    // Compute cursor line/col for positioning.
    let cursor_byte = cursor_pos as usize;
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_byte, chars_per_line as usize);

    // Compute selection rectangles.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };
    let has_selection = sel_lo < sel_hi;

    w.clear();

    // Push shaped glyph arrays for title and clock.
    let title_glyphs = shape_text(
        cfg.font_data,
        title_label,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
    let clock_glyphs = shape_text(cfg.font_data, clock_text, cfg.font_size, cfg.upem, cfg.axes);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Push visible line glyph data.
    let mut line_glyph_refs: Vec<(DataRef, u16, i16)> = Vec::with_capacity(visible_runs.len());

    for run in &visible_runs {
        let line_text = line_bytes_for_run(doc_text, run);
        let shaped = shape_text(cfg.font_data, line_text, cfg.font_size, cfg.upem, cfg.axes);
        let glyph_ref = w.push_shaped_glyphs(&shaped);

        line_glyph_refs.push((glyph_ref, shaped.len() as u16, run.y));
    }

    // Allocate well-known nodes in order (sequential IDs).
    let _root = w.alloc_node().unwrap(); // 0
    let _title_bar = w.alloc_node().unwrap(); // 1
    let _title_text = w.alloc_node().unwrap(); // 2
    let _clock_text = w.alloc_node().unwrap(); // 3
    let _shadow = w.alloc_node().unwrap(); // 4
    let _content = w.alloc_node().unwrap(); // 5
    let _doc_text = w.alloc_node().unwrap(); // 6
    let _cursor_node = w.alloc_node().unwrap(); // 7

    {
        let n = w.node_mut(N_ROOT);

        n.first_child = N_TITLE_BAR;
        n.width = cfg.fb_width as u16;
        n.height = cfg.fb_height as u16;
        n.background = dc(cfg.bg_color);
        n.flags = NodeFlags::VISIBLE;
    }
    {
        let n = w.node_mut(N_TITLE_BAR);

        n.first_child = N_TITLE_TEXT;
        n.next_sibling = N_SHADOW;
        n.width = cfg.fb_width as u16;
        n.height = cfg.title_bar_h as u16;
        n.background = dc(cfg.chrome_bg);
        n.border = Border {
            color: dc(cfg.chrome_border),
            width: 1,
            _pad: [0; 3],
        };
        n.flags = NodeFlags::VISIBLE;
        // Real blurred shadow below the title bar.
        n.shadow_color = Color::rgba(0, 0, 0, 60);
        n.shadow_offset_x = 0;
        n.shadow_offset_y = cfg.shadow_depth as i16;
        n.shadow_blur_radius = (cfg.shadow_depth as u8).min(8);
        n.shadow_spread = 0;
    }

    let text_y_offset = (cfg.title_bar_h.saturating_sub(cfg.line_height)) / 2;

    {
        let n = w.node_mut(N_TITLE_TEXT);

        n.next_sibling = N_CLOCK_TEXT;
        n.x = 12;
        n.y = text_y_offset as i16;
        n.width = (cfg.fb_width / 2) as u16;
        n.height = cfg.line_height as u16;
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_title_color),
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(title_label);
        n.flags = NodeFlags::VISIBLE;
    }

    let clock_x = (cfg.fb_width - 12 - 80) as i16;

    {
        let n = w.node_mut(N_CLOCK_TEXT);

        n.x = clock_x;
        n.y = text_y_offset as i16;
        n.width = 80;
        n.height = cfg.line_height as u16;
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_clock_color),
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(clock_text);
        n.flags = NodeFlags::VISIBLE;
    }
    {
        // N_SHADOW is kept as a structural placeholder for
        // well-known node index stability. The real shadow is
        // now rendered by the title bar's shadow fields.
        let n = w.node_mut(N_SHADOW);

        n.next_sibling = N_CONTENT;
        n.y = cfg.title_bar_h as i16;
        n.width = cfg.fb_width as u16;
        n.height = 0;
        n.background = Color::TRANSPARENT;
        n.flags = NodeFlags::VISIBLE;
    }

    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y);

    {
        let n = w.node_mut(N_CONTENT);

        n.first_child = N_DOC_TEXT;
        n.next_sibling = NULL;
        n.y = content_y as i16;
        n.width = cfg.fb_width as u16;
        n.height = content_h as u16;
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    {
        let n = w.node_mut(N_DOC_TEXT);

        n.x = cfg.text_inset_x as i16;
        n.y = 8;
        n.width = doc_width as u16;
        n.height = content_h as u16;
        n.scroll_y = 0;
        // N_DOC_TEXT is now a pure container — per-line Glyphs
        // child nodes hold the actual text content.
        n.content = Content::None;
        n.content_hash = fnv1a(doc_text);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }

    // Allocate per-line Glyphs child nodes under N_DOC_TEXT.
    // Ordering: line nodes first, then N_CURSOR, then selection rects.
    // Detach cursor from doc_text's first_child temporarily.
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    let mut prev_line_node: u16 = NULL;

    for &(glyph_ref, glyph_count, y) in &line_glyph_refs {
        if let Some(line_id) = w.alloc_node() {
            let n = w.node_mut(line_id);
            n.y = y;
            n.width = doc_width as u16;
            n.height = cfg.line_height as u16;
            n.content = Content::Glyphs {
                color: scene_text_color,
                glyphs: glyph_ref,
                glyph_count,
                font_size: cfg.font_size,
                axis_hash: 0,
            };
            n.content_hash = fnv1a(&glyph_ref.offset.to_le_bytes());
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

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    // Cursor: positioned rectangle child of doc text node.
    // Scroll-adjusted: cursor_line is absolute, subtract scroll.
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i16;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32 - scroll_px) as i16;

    {
        let n = w.node_mut(N_CURSOR);

        n.x = cursor_x;
        n.y = cursor_y;
        n.width = 2;
        n.height = cfg.line_height as u16;
        n.background = dc(cfg.cursor_color);
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }

    // Selection highlight rectangles (dynamically allocated, scroll-adjusted).
    if has_selection {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            chars_per_line as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_px,
        );
    }

    // ── Test content: Image + Path ─────────────────────────────
    // These exercise Content::Image and Content::Path in the GPU
    // driver. Positioned in the bottom-right of the content area.

    // Test image: 32x32 BGRA gradient.
    let test_img = generate_test_image();
    let img_ref = w.push_data(&test_img);
    if let Some(img_id) = w.alloc_node() {
        let n = w.node_mut(img_id);
        n.x = (cfg.fb_width as i16).saturating_sub(160);
        n.y = 8;
        n.width = 64; // Display at 2x for visibility.
        n.height = 64;
        n.content = Content::Image {
            data: img_ref,
            src_width: 32,
            src_height: 32,
        };
        n.flags = NodeFlags::VISIBLE;

        // Link as last child of N_CONTENT (after cursor/selection).
        // Walk to find last child.
        let mut last = w.node(N_CONTENT).first_child;
        if last == NULL {
            w.node_mut(N_CONTENT).first_child = img_id;
        } else {
            while w.node(last).next_sibling != NULL {
                last = w.node(last).next_sibling;
            }
            w.node_mut(last).next_sibling = img_id;
        }
    }

    // Test path 1: 5-pointed star (red).
    let star_cmds = generate_test_star(60.0);
    let star_ref = w.push_path_commands(&star_cmds);
    if let Some(star_id) = w.alloc_node() {
        let n = w.node_mut(star_id);
        n.x = (cfg.fb_width as i16).saturating_sub(90);
        n.y = 8;
        n.width = 60;
        n.height = 60;
        n.content = Content::Path {
            color: Color::rgba(255, 80, 80, 255),
            fill_rule: FillRule::Winding,
            contours: star_ref,
        };
        n.flags = NodeFlags::VISIBLE;

        let mut last = w.node(N_CONTENT).first_child;
        if last == NULL {
            w.node_mut(N_CONTENT).first_child = star_id;
        } else {
            while w.node(last).next_sibling != NULL {
                last = w.node(last).next_sibling;
            }
            w.node_mut(last).next_sibling = star_id;
        }
    }

    // Test path 2: Rounded rectangle (blue, tests CubicTo).
    let rrect_cmds = generate_test_rounded_rect(80.0, 40.0, 8.0);
    let rrect_ref = w.push_path_commands(&rrect_cmds);
    if let Some(rr_id) = w.alloc_node() {
        let n = w.node_mut(rr_id);
        n.x = (cfg.fb_width as i16).saturating_sub(160);
        n.y = 78;
        n.width = 80;
        n.height = 40;
        n.content = Content::Path {
            color: Color::rgba(80, 140, 255, 255),
            fill_rule: FillRule::Winding,
            contours: rrect_ref,
        };
        n.flags = NodeFlags::VISIBLE;

        let mut last = w.node(N_CONTENT).first_child;
        if last == NULL {
            w.node_mut(N_CONTENT).first_child = rr_id;
        } else {
            while w.node(last).next_sibling != NULL {
                last = w.node(last).next_sibling;
            }
            w.node_mut(last).next_sibling = rr_id;
        }
    }

    w.set_root(N_ROOT);
}

/// Update only the clock text in an already-open back buffer, then
/// mark N_CLOCK_TEXT changed.
pub fn build_clock_update(w: &mut scene::SceneWriter<'_>, cfg: &SceneConfig, clock_text: &[u8]) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Glyphs { color, .. } = clock_node.content {
        let new_glyphs = shape_text(cfg.font_data, clock_text, cfg.font_size, cfg.upem, cfg.axes);
        let new_ref = w.push_shaped_glyphs(&new_glyphs);
        let new_count = new_glyphs.len() as u16;

        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(clock_text);
        w.mark_changed(N_CLOCK_TEXT);
    }
}

/// Update cursor position in an already-open back buffer. Optionally
/// updates the clock if `clock_text` is provided.
pub fn build_cursor_update(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    cursor_pos: u32,
    doc_text: &[u8],
    chars_per_line: u32,
    scroll_px: i32,
    clock_text: Option<&[u8]>,
) {
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i16;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32 - scroll_px) as i16;

    let n = w.node_mut(N_CURSOR);
    n.x = cursor_x;
    n.y = cursor_y;

    w.mark_changed(N_CURSOR);

    if let Some(ct) = clock_text {
        update_clock_inline(w, ct, cfg.font_data, cfg.font_size, cfg.upem, cfg.axes);
    }
}

/// Update cursor position and selection rects in an already-open back
/// buffer. Optionally updates the clock if `clock_text` is provided.
#[allow(clippy::too_many_arguments)]
pub fn build_selection_update(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    doc_text: &[u8],
    content_h: u32,
    scroll_px: i32,
    clock_text: Option<&[u8]>,
) {
    let dc = |c: drawing::Color| -> Color { Color::rgba(c.r, c.g, c.b, c.a) };
    let doc_width = cfg.fb_width.saturating_sub(2 * cfg.text_inset_x);
    let chars_per_line = if cfg.char_width > 0 {
        (doc_width / cfg.char_width).max(1)
    } else {
        80
    };

    // TODO: data buffer leaks on selection-only updates (~64 bytes per
    // update from push_shaped_glyphs for clock text). Acceptable because
    // text changes call update_document_content which resets the data
    // buffer via reset_data(). Selection-only updates without intervening
    // text changes are rare enough that the leak is bounded well within
    // the DATA_BUFFER_SIZE budget before the next compaction.

    // Count per-line Glyphs children under N_DOC_TEXT (stop at
    // N_CURSOR). These must be preserved — only selection rects
    // (allocated after cursor) are truncated.
    let mut line_count: u16 = 0;
    let mut child = w.node(N_DOC_TEXT).first_child;
    while child != NULL && child != N_CURSOR {
        line_count += 1;
        child = w.node(child).next_sibling;
    }

    // Truncate selection rects only, keeping well-known + line nodes.
    w.set_node_count(WELL_KNOWN_COUNT + line_count);

    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i16;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32 - scroll_px) as i16;

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.next_sibling = NULL;
    }
    w.mark_changed(N_CURSOR);

    if let Some(ct) = clock_text {
        update_clock_inline(w, ct, cfg.font_data, cfg.font_size, cfg.upem, cfg.axes);
    }

    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };

    if sel_lo < sel_hi {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            chars_per_line as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_px,
        );
    }
}

/// Update document content (line nodes + cursor + selection) in an
/// already-open back buffer. Compacts the data buffer.
#[allow(clippy::too_many_arguments)]
pub fn build_document_content(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    title_label: &[u8],
    clock_text: &[u8],
    scroll_y: i32,
    mark_clock_changed: bool,
) {
    let dc = |c: drawing::Color| -> Color { Color::rgba(c.r, c.g, c.b, c.a) };
    let scene_text_color = dc(cfg.text_color);

    let doc_width = cfg.fb_width.saturating_sub(2 * cfg.text_inset_x);
    let chars_per_line = if cfg.char_width > 0 {
        (doc_width / cfg.char_width).max(1)
    } else {
        80
    };
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y);
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let scroll_px = scroll_lines as i32 * cfg.line_height as i32;

    // Remove old dynamic nodes (line nodes + selection rects).
    w.set_node_count(WELL_KNOWN_COUNT);

    // ── Data buffer compaction ──────────────────────────────
    w.reset_data();

    // Re-push title glyph data.
    let title_glyphs = shape_text(
        cfg.font_data,
        title_label,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

    // Re-push clock glyph data.
    let clock_glyphs = shape_text(cfg.font_data, clock_text, cfg.font_size, cfg.upem, cfg.axes);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Re-layout visible document text lines.
    let all_runs = layout_mono_lines(
        doc_text,
        chars_per_line as usize,
        cfg.line_height as i16,
        scene_text_color,
        cfg.font_size,
    );
    let viewport_height_px = content_h as i32;
    let visible_runs = scroll_runs(all_runs, scroll_lines, cfg.line_height, viewport_height_px);

    // Push visible line glyph data.
    let mut line_glyph_refs: Vec<(DataRef, u16, i16)> = Vec::with_capacity(visible_runs.len());

    for run in &visible_runs {
        let line_text = line_bytes_for_run(doc_text, run);
        let shaped = shape_text(cfg.font_data, line_text, cfg.font_size, cfg.upem, cfg.axes);
        let glyph_ref = w.push_shaped_glyphs(&shaped);

        line_glyph_refs.push((glyph_ref, shaped.len() as u16, run.y));
    }

    // Update N_TITLE_TEXT content references (data was reset).
    {
        let n = w.node_mut(N_TITLE_TEXT);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_title_color),
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(title_label);
    }

    // Update N_CLOCK_TEXT content references (data was reset).
    {
        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_clock_color),
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(clock_text);
    }
    if mark_clock_changed {
        w.mark_changed(N_CLOCK_TEXT);
    }

    // Re-create per-line Glyphs children under N_DOC_TEXT.
    // Reset both first_child AND next_sibling. The initial
    // build_editor_scene links test content (Image, Path) as
    // siblings of N_DOC_TEXT under N_CONTENT. acquire_copy()
    // preserves that stale next_sibling pointer. After
    // truncation, the same node index gets reused for a line
    // node — the walker would visit it twice (once as child of
    // N_DOC_TEXT, once as sibling) with different parent Y
    // offsets, causing ghost duplicates.
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;
    w.node_mut(N_DOC_TEXT).content = Content::None;
    w.node_mut(N_DOC_TEXT).content_hash = fnv1a(doc_text);
    let mut prev_line_node: u16 = NULL;

    for &(glyph_ref, glyph_count, y) in &line_glyph_refs {
        if let Some(line_id) = w.alloc_node() {
            let n = w.node_mut(line_id);
            n.y = y;
            n.width = doc_width as u16;
            n.height = cfg.line_height as u16;
            n.content = Content::Glyphs {
                color: scene_text_color,
                glyphs: glyph_ref,
                glyph_count,
                font_size: cfg.font_size,
                axis_hash: 0,
            };
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

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    w.mark_changed(N_DOC_TEXT);

    // Update cursor position.
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i16;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32 - scroll_px) as i16;

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.next_sibling = NULL;
    }
    w.mark_changed(N_CURSOR);

    // Build selection rects.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };

    if sel_lo < sel_hi {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            chars_per_line as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_px,
        );
    }
}
