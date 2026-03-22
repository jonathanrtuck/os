//! Full scene graph builds.
//!
//! Contains `build_full_scene` (initial scene from scratch) and
//! `build_document_content` (compaction rebuild of document content
//! within an existing scene).

use alloc::vec::Vec;

use scene::{fnv1a, Border, Color, Content, FillRule, NodeFlags, NULL};

use super::{
    allocate_line_nodes, allocate_selection_rects, byte_to_line_col, chars_per_line, dc, doc_width,
    layout_mono_lines, line_bytes_for_run, round_f32, scroll_runs, shape_text, shape_visible_runs,
    update_clock_inline, SceneConfig, N_CLOCK_TEXT, N_CONTENT, N_CURSOR, N_DOC_TEXT, N_POINTER,
    N_ROOT, N_SHADOW, N_TITLE_BAR, N_TITLE_TEXT, WELL_KNOWN_COUNT,
};
use crate::test_gen::generate_test_image;

// ── Full scene builds (called by SceneState methods) ────────────────

/// Build the full editor scene into a fresh (cleared) SceneWriter.
///
/// When `image_mode` is true, the content area shows a centered test image
/// instead of the document text, cursor, selection, and demo nodes.
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
    scroll_y: f32,
    cursor_opacity: u8,
    mouse_x: u32,
    mouse_y: u32,
    pointer_opacity: u8,
    image_mode: bool,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    // Editor-mode-only layout computation — skipped in image mode.
    let (visible_runs, scroll_pt, cursor_line, cursor_col, has_selection, sel_lo, sel_hi) =
        if !image_mode {
            let all_runs = layout_mono_lines(
                doc_text,
                cpl as usize,
                cfg.line_height as i32,
                scene_text_color,
                cfg.font_size,
            );
            let content_y = cfg.title_bar_h + cfg.shadow_depth;
            let content_h = cfg.fb_height.saturating_sub(content_y) as i32;
            let visible_runs = scroll_runs(all_runs, scroll_y, cfg.line_height, content_h);
            let scroll_pt = round_f32(scroll_y);
            let cursor_byte = cursor_pos as usize;
            let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_byte, cpl as usize);
            let (sel_lo, sel_hi) = if sel_start <= sel_end {
                (sel_start as usize, sel_end as usize)
            } else {
                (sel_end as usize, sel_start as usize)
            };
            let has_selection = sel_lo < sel_hi;
            (
                visible_runs,
                scroll_pt,
                cursor_line,
                cursor_col,
                has_selection,
                sel_lo,
                sel_hi,
            )
        } else {
            (alloc::vec![], 0, 0, 0, false, 0, 0)
        };

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

    // Push visible line glyph data (editor mode only).
    let line_glyph_refs = if !image_mode {
        shape_visible_runs(
            w,
            &visible_runs,
            doc_text,
            cfg.font_data,
            cfg.font_size,
            cfg.upem,
            cfg.axes,
        )
    } else {
        alloc::vec![]
    };

    // Allocate well-known nodes in order (sequential IDs).
    let _root = w.alloc_node().unwrap(); // 0
    let _title_bar = w.alloc_node().unwrap(); // 1
    let _title_text = w.alloc_node().unwrap(); // 2
    let _clock_text = w.alloc_node().unwrap(); // 3
    let _shadow = w.alloc_node().unwrap(); // 4
    let _content = w.alloc_node().unwrap(); // 5
    let _doc_text = w.alloc_node().unwrap(); // 6
    let _cursor_node = w.alloc_node().unwrap(); // 7

    // Pointer cursor node (top-level, highest z-order).
    let _pointer = w.alloc_node().unwrap(); // 8

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
        // Blurred shadow below the title bar (analytical Gaussian in metal-render).
        n.shadow_color = Color::rgba(0, 0, 0, 120);
        n.shadow_offset_x = 0;
        n.shadow_offset_y = 2;
        n.shadow_blur_radius = 12;
        n.shadow_spread = 0;
    }

    let text_y_offset = (cfg.title_bar_h.saturating_sub(cfg.line_height)) / 2;

    {
        let n = w.node_mut(N_TITLE_TEXT);

        n.next_sibling = N_CLOCK_TEXT;
        n.x = 12;
        n.y = text_y_offset as i32;
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

    let clock_x = (cfg.fb_width - 12 - 80) as i32;

    {
        let n = w.node_mut(N_CLOCK_TEXT);

        n.x = clock_x;
        n.y = text_y_offset as i32;
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
        n.y = cfg.title_bar_h as i32;
        n.width = cfg.fb_width as u16;
        n.height = 0;
        n.background = Color::TRANSPARENT;
        n.flags = NodeFlags::VISIBLE;
    }

    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h_u32 = cfg.fb_height.saturating_sub(content_y);

    {
        let n = w.node_mut(N_CONTENT);

        n.first_child = N_DOC_TEXT;
        n.next_sibling = NULL;
        n.y = content_y as i32;
        n.width = cfg.fb_width as u16;
        n.height = content_h_u32 as u16;
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }

    // ── Image mode: centered test image, no document text ─────────────
    if image_mode {
        let test_img = generate_test_image();
        let img_ref = w.push_data(&test_img);

        let img_display_w: u16 = 128;
        let img_display_h: u16 = 128;
        let center_x = ((cfg.fb_width as i32 - img_display_w as i32) / 2).max(0);
        let center_y = ((content_h_u32 as i32 - img_display_h as i32) / 2).max(0);

        {
            let n = w.node_mut(N_DOC_TEXT);
            n.x = center_x;
            n.y = center_y;
            n.width = img_display_w;
            n.height = img_display_h;
            n.content = Content::Image {
                data: img_ref,
                src_width: 32,
                src_height: 32,
            };
            n.content_hash = fnv1a(&test_img);
            n.flags = NodeFlags::VISIBLE;
            n.first_child = NULL;
            n.next_sibling = NULL;
            n.content_transform = scene::AffineTransform::identity();
        }

        // Cursor: hidden in image mode.
        {
            let n = w.node_mut(N_CURSOR);
            n.flags = NodeFlags::empty();
            n.next_sibling = NULL;
        }

        // Link N_CONTENT → N_POINTER so the pointer renders on top.
        w.node_mut(N_CONTENT).next_sibling = N_POINTER;

        // Pointer cursor node.
        {
            let arrow_cmds = crate::test_gen::generate_arrow_cursor();
            let arrow_ref = w.push_path_commands(&arrow_cmds);
            let arrow_hash = scene::fnv1a(&arrow_cmds);
            let n = w.node_mut(N_POINTER);
            n.x = mouse_x as i32;
            n.y = mouse_y as i32;
            n.width = 10;
            n.height = 18;
            n.content = Content::Path {
                color: Color::rgb(255, 255, 255),
                fill_rule: FillRule::Winding,
                contours: arrow_ref,
            };
            n.content_hash = arrow_hash;
            n.opacity = pointer_opacity;
            n.flags = NodeFlags::VISIBLE;
            n.next_sibling = NULL;
        }

        w.set_root(N_ROOT);
        return;
    }
    // ── End image mode ────────────────────────────────────────────────

    {
        let n = w.node_mut(N_DOC_TEXT);

        n.x = cfg.text_inset_x as i32;
        n.y = 8;
        n.width = doc_width as u16;
        n.height = content_h_u32 as u16;
        n.content_transform = scene::AffineTransform::translate(0.0, -scroll_y);
        // N_DOC_TEXT is now a pure container -- per-line Glyphs
        // child nodes hold the actual text content.
        n.content = Content::None;
        n.content_hash = fnv1a(doc_text);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }

    // Allocate per-line Glyphs child nodes under N_DOC_TEXT.
    // Ordering: line nodes first, then N_CURSOR, then selection rects.
    let prev_line_node = allocate_line_nodes(
        w,
        &line_glyph_refs,
        doc_width,
        cfg.line_height,
        scene_text_color,
        cfg.font_size,
    );

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    // Cursor: positioned rectangle child of doc text node.
    // Document-relative: renderer applies content_transform from N_DOC_TEXT.
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i32;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;

    {
        let n = w.node_mut(N_CURSOR);

        n.x = cursor_x;
        n.y = cursor_y;
        n.width = 2;
        n.height = cfg.line_height as u16;
        n.background = dc(cfg.cursor_color);
        n.opacity = cursor_opacity;
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
            cpl as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h_u32,
            scroll_pt,
        );
    }

    // N_DOC_TEXT is the only child of N_CONTENT. No sibling chain.

    // Link pointer cursor as a top-level sibling after N_CONTENT so it
    // renders above all document content (highest z-order in root).
    w.node_mut(N_CONTENT).next_sibling = N_POINTER;

    // Pointer cursor node: arrow shape rendered at mouse position.
    {
        let arrow_cmds = crate::test_gen::generate_arrow_cursor();
        let arrow_ref = w.push_path_commands(&arrow_cmds);
        let arrow_hash = scene::fnv1a(&arrow_cmds);
        let n = w.node_mut(N_POINTER);
        n.x = mouse_x as i32;
        n.y = mouse_y as i32;
        n.width = 10;
        n.height = 18;
        n.content = Content::Path {
            color: Color::rgb(255, 255, 255),
            fill_rule: FillRule::Winding,
            contours: arrow_ref,
        };
        n.content_hash = arrow_hash;
        n.opacity = pointer_opacity;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
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
        w.mark_dirty(N_CLOCK_TEXT);
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
    clock_text: Option<&[u8]>,
    cursor_opacity: u8,
) {
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i32;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;

    let n = w.node_mut(N_CURSOR);
    n.x = cursor_x;
    n.y = cursor_y;
    n.opacity = cursor_opacity;

    w.mark_dirty(N_CURSOR);

    if let Some(ct) = clock_text {
        update_clock_inline(w, ct, cfg.font_data, cfg.font_size, cfg.upem, cfg.axes);
    }
}

/// Update cursor position and selection rects in an already-open back
/// buffer.
#[allow(clippy::too_many_arguments)]
pub fn build_selection_update(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    doc_text: &[u8],
    content_h: u32,
    scroll_pt: i32,
    cursor_opacity: u8,
) {
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    // Clock text is updated only by update_document_content (timer-driven).
    // Skipping here prevents data buffer leak (~64 bytes per selection update).

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

    // N_DOC_TEXT is the sole child of N_CONTENT — no siblings.
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;

    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i32;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.opacity = cursor_opacity;
        n.next_sibling = NULL;
    }
    w.mark_dirty(N_CURSOR);

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
            cpl as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_pt,
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
    scroll_y: f32,
    mark_clock_changed: bool,
    cursor_opacity: u8,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y);
    let scroll_pt = round_f32(scroll_y);

    // Remove old dynamic nodes (line nodes + selection rects).
    // set_node_count automatically clears dangling first_child pointers
    // on surviving nodes that referenced the now-dead dynamic nodes.
    w.set_node_count(WELL_KNOWN_COUNT);

    // ── Data buffer compaction ──────────────────────────────
    // Note: reset_data invalidates all DataRef values (clip_path, content).
    // Surviving nodes with stale DataRefs will produce empty data lookups
    // (the reader returns &[] for out-of-bounds refs). This is safe but
    // means clip paths on demo nodes won't render after compaction — they
    // are re-pushed in build_full_scene on the next full rebuild.
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
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );
    let viewport_height_pt = content_h as i32;
    let visible_runs = scroll_runs(all_runs, scroll_y, cfg.line_height, viewport_height_pt);

    // Push visible line glyph data.
    let line_glyph_refs = shape_visible_runs(
        w,
        &visible_runs,
        doc_text,
        cfg.font_data,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );

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
        w.mark_dirty(N_CLOCK_TEXT);
    }

    // Re-create per-line Glyphs children under N_DOC_TEXT.
    // N_DOC_TEXT is the sole child of N_CONTENT — no siblings.
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;
    w.node_mut(N_DOC_TEXT).content_transform = scene::AffineTransform::translate(0.0, -scroll_y);
    w.node_mut(N_DOC_TEXT).content = Content::None;
    w.node_mut(N_DOC_TEXT).content_hash = fnv1a(doc_text);

    let prev_line_node = allocate_line_nodes(
        w,
        &line_glyph_refs,
        doc_width,
        cfg.line_height,
        scene_text_color,
        cfg.font_size,
    );

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    w.mark_dirty(N_DOC_TEXT);

    // Update cursor position (document-relative).
    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i32;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.opacity = cursor_opacity;
        n.next_sibling = NULL;
    }
    w.mark_dirty(N_CURSOR);

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
            cpl as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_pt,
        );
    }
}
