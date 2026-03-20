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
    update_clock_inline, SceneConfig, N_CLOCK_TEXT, N_CONTENT, N_CURSOR, N_DEMO_BALL,
    N_DEMO_EASE_0, N_DEMO_EASE_1, N_DEMO_EASE_2, N_DEMO_EASE_3, N_DEMO_EASE_4, N_DOC_TEXT, N_POINTER,
    N_ROOT, N_SHADOW, N_TITLE_BAR, N_TITLE_TEXT, WELL_KNOWN_COUNT,
};
use crate::test_gen::{generate_test_image, generate_test_rounded_rect, generate_test_star};

// ── Demo animation constants ─────────────────────────────────────────
//
// All positions are content-area-relative (i.e., relative to N_CONTENT's
// top-left corner, which sits just below the title bar and shadow).

/// Width of the demo panel in points.
pub const DEMO_W: u32 = 160;
/// Height of each easing-sampler bar in points.
pub const DEMO_BAR_H: i32 = 8;
/// Vertical gap between easing-sampler bars in points.
pub const DEMO_BAR_GAP: i32 = 4;
/// Width of each easing-sampler travel range in points.
pub const DEMO_EASE_TRAVEL: i32 = 100;
/// X of the left edge of the demo panel (from content-area left = 0).
/// Set at runtime from fb_width; see `demo_left_x`.
// (No compile-time constant because fb_width is dynamic.)

/// Ball side length in points.
pub const DEMO_BALL_SIZE: u32 = 16;
/// Top Y of the bouncing ball's upper target position (content-area-relative).
pub const DEMO_BALL_Y_TOP: i32 = 130;
/// Bottom Y of the bouncing ball's lower target position (content-area-relative).
pub const DEMO_BALL_Y_BOT: i32 = 220;
/// Y of the first easing-sampler bar (content-area-relative).
pub const DEMO_EASE_Y0: i32 = 260;

/// Compute the left X of the demo panel given the framebuffer width.
#[inline]
pub(crate) fn demo_left_x(fb_width: u32) -> i32 {
    (fb_width as i32).saturating_sub(DEMO_W as i32 + 10)
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
    scroll_y: f32,
    cursor_opacity: u8,
    mouse_x: u32,
    mouse_y: u32,
    pointer_opacity: u8,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    // Layout document text into visual lines (monospace line-breaking).
    let all_runs = layout_mono_lines(
        doc_text,
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );
    // Apply scroll: filter to visible viewport.
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y) as i32;
    let visible_runs = scroll_runs(all_runs, scroll_y, cfg.line_height, content_h);
    // Scroll offset in points for cursor/selection positioning.
    let scroll_pt = round_f32(scroll_y);
    // Compute cursor line/col for positioning.
    let cursor_byte = cursor_pos as usize;
    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_byte, cpl as usize);

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
    let line_glyph_refs = shape_visible_runs(
        w,
        &visible_runs,
        doc_text,
        cfg.font_data,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );

    // Allocate well-known nodes in order (sequential IDs).
    let _root = w.alloc_node().unwrap(); // 0
    let _title_bar = w.alloc_node().unwrap(); // 1
    let _title_text = w.alloc_node().unwrap(); // 2
    let _clock_text = w.alloc_node().unwrap(); // 3
    let _shadow = w.alloc_node().unwrap(); // 4
    let _content = w.alloc_node().unwrap(); // 5
    let _doc_text = w.alloc_node().unwrap(); // 6
    let _cursor_node = w.alloc_node().unwrap(); // 7
                                                // Demo nodes 8..13 (scaffolding — see N_DEMO_BALL, N_DEMO_EASE_*).
    let _demo_ball = w.alloc_node().unwrap(); // 8
    let _demo_ease0 = w.alloc_node().unwrap(); // 9
    let _demo_ease1 = w.alloc_node().unwrap(); // 10
    let _demo_ease2 = w.alloc_node().unwrap(); // 11
    let _demo_ease3 = w.alloc_node().unwrap(); // 12
    let _demo_ease4 = w.alloc_node().unwrap(); // 13
                                                // Pointer cursor node (top-level, highest z-order).
    let _pointer = w.alloc_node().unwrap(); // 14

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

    // ── Test content: Image + Path ─────────────────────────────
    // These exercise Content::Image and Content::Path in the GPU
    // driver. Positioned in the bottom-right of the content area.

    // Test image: 32x32 BGRA gradient.
    let test_img = generate_test_image();
    let img_ref = w.push_data(&test_img);
    if let Some(img_id) = w.alloc_node() {
        let n = w.node_mut(img_id);
        n.x = (cfg.fb_width as i32).saturating_sub(160);
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
        n.x = (cfg.fb_width as i32).saturating_sub(90);
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
        n.x = (cfg.fb_width as i32).saturating_sub(160);
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

    // ── Demo nodes: bouncing ball + easing sampler ─────────────────
    //
    // These well-known nodes (N_DEMO_BALL, N_DEMO_EASE_0..4) are
    // initialized here and updated in-place every animation frame by
    // `update_demo_nodes` via `apply_demo`. They are direct children
    // of N_CONTENT and never removed or reallocated.
    let demo_x = demo_left_x(cfg.fb_width);

    // Bouncing ball — cyan square, initial Y at top target.
    {
        let n = w.node_mut(N_DEMO_BALL);
        n.x = demo_x + DEMO_EASE_TRAVEL / 2 - DEMO_BALL_SIZE as i32 / 2;
        n.y = DEMO_BALL_Y_TOP;
        n.width = DEMO_BALL_SIZE as u16;
        n.height = DEMO_BALL_SIZE as u16;
        n.background = Color::rgba(0, 220, 220, 255);
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = N_DEMO_EASE_0;
    }

    // Easing-sampler bars: five colors, stacked vertically.
    // Each bar starts at x = demo_x (travel = 0) and animates to
    // x = demo_x + DEMO_EASE_TRAVEL.
    let bar_colors = [
        Color::rgba(255, 100, 100, 255), // Linear   — red
        Color::rgba(255, 180, 50, 255),  // EaseOut  — orange
        Color::rgba(100, 220, 100, 255), // EaseInOut — green
        Color::rgba(80, 160, 255, 255),  // EaseInBack — blue
        Color::rgba(220, 80, 220, 255),  // EaseOutBounce — magenta
    ];
    let ease_node_ids = [
        N_DEMO_EASE_0,
        N_DEMO_EASE_1,
        N_DEMO_EASE_2,
        N_DEMO_EASE_3,
        N_DEMO_EASE_4,
    ];

    for (i, &nid) in ease_node_ids.iter().enumerate() {
        let bar_y = DEMO_EASE_Y0 + i as i32 * (DEMO_BAR_H + DEMO_BAR_GAP);
        let n = w.node_mut(nid);
        n.x = demo_x;
        n.y = bar_y;
        n.width = DEMO_EASE_TRAVEL as u16;
        n.height = DEMO_BAR_H as u16;
        n.background = bar_colors[i];
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = if i + 1 < ease_node_ids.len() {
            ease_node_ids[i + 1]
        } else {
            NULL
        };
    }

    // Link demo siblings: last dynamic node (rr_id) → N_DEMO_BALL.
    // Walk N_CONTENT children to find the last one.
    {
        let mut last = w.node(N_CONTENT).first_child;
        if last == NULL {
            w.node_mut(N_CONTENT).first_child = N_DEMO_BALL;
        } else {
            while w.node(last).next_sibling != NULL {
                last = w.node(last).next_sibling;
            }
            w.node_mut(last).next_sibling = N_DEMO_BALL;
        }
    }

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

/// Update demo node positions in an already-open back buffer.
///
/// Called every animation frame via `SceneState::apply_demo`. Only updates
/// the Y of the bouncing ball and the X of each easing-sampler bar — no
/// heap allocation, no data-buffer changes.
///
/// - `ball_y`: content-area-relative Y for the ball node centre (rounded to i32)
/// - `ease_x`: per-bar X offset from the panel's left edge
pub fn update_demo_nodes(w: &mut scene::SceneWriter<'_>, ball_y: i32, ease_x: &[i32; 5]) {
    w.node_mut(N_DEMO_BALL).y = ball_y;
    w.mark_dirty(N_DEMO_BALL);

    let demo_base_x = {
        // Reconstruct demo_left_x from the ball's current absolute x
        // (ball.x is demo_x + offset, where offset is fixed).
        // Simpler: store demo_x in the ball's x relative form.
        // We know ball.x = demo_x + DEMO_EASE_TRAVEL/2 - DEMO_BALL_SIZE/2
        // so demo_x = ball.x - DEMO_EASE_TRAVEL/2 + DEMO_BALL_SIZE/2.
        let ball_x = w.node(N_DEMO_BALL).x;
        ball_x - DEMO_EASE_TRAVEL / 2 + DEMO_BALL_SIZE as i32 / 2
    };

    let ease_node_ids = [
        N_DEMO_EASE_0,
        N_DEMO_EASE_1,
        N_DEMO_EASE_2,
        N_DEMO_EASE_3,
        N_DEMO_EASE_4,
    ];

    for (i, &nid) in ease_node_ids.iter().enumerate() {
        w.node_mut(nid).x = demo_base_x + ease_x[i];
        w.mark_dirty(nid);
    }
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

    // Repoint N_DOC_TEXT.next_sibling to N_DEMO_BALL so demo nodes stay
    // in the N_CONTENT child chain after truncation. The initial
    // build_full_scene links dynamic test content (img, star, rrect) as
    // siblings after N_DOC_TEXT; those IDs are now above node_count and
    // the traversal would stop there without this fixup.
    w.node_mut(N_DOC_TEXT).next_sibling = N_DEMO_BALL;

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
    // Reset next_sibling to N_DEMO_BALL so the demo nodes remain
    // in the N_CONTENT child chain after compaction. The initial
    // build_full_scene links test content (Image, Path) as
    // siblings of N_DOC_TEXT under N_CONTENT, but those dynamic
    // nodes (IDs >= WELL_KNOWN_COUNT) are gone after truncation.
    // Pointing next_sibling → N_DEMO_BALL skips the now-dead test
    // content nodes and re-attaches the well-known demo nodes.
    w.node_mut(N_DOC_TEXT).next_sibling = N_DEMO_BALL;
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
