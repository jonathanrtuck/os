//! Mutable scene graph state backed by a double-buffered shared memory layout.
//!
//! Wraps a `DoubleWriter` operating on shared memory. The core process
//! builds each frame into the back buffer, then `swap()` publishes it.
//! The compositor reads the front buffer from the same shared memory region.

use alloc::vec::Vec;

use scene::{
    fnv1a, Border, Color, Content, DataRef, DoubleWriter, NodeFlags, ShapedGlyph,
    DATA_BUFFER_SIZE, DOUBLE_SCENE_SIZE, NULL,
};

/// Local layout run type — used for line-breaking before writing to
/// the scene graph. Each LayoutRun describes one visual text line.
struct LayoutRun {
    /// Placeholder DataRef: offset = byte position in source text,
    /// length = byte count. Replaced with actual data buffer ref
    /// before writing to the scene graph.
    glyphs: DataRef,
    /// Number of glyphs (= bytes for monospace ASCII).
    glyph_count: u16,
    /// Starting pixel position relative to the parent node.
    y: i16,
    /// Text color.
    color: Color,
    /// Font size in pixels.
    font_size: u16,
}

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

pub struct SceneState {
    buf: &'static mut [u8],
}

impl SceneState {
    /// Create from an externally-provided buffer (shared memory).
    pub fn from_buf(buf: &'static mut [u8]) -> Self {
        assert!(buf.len() >= DOUBLE_SCENE_SIZE);

        let _ = DoubleWriter::new(buf);

        Self { buf }
    }

    fn double(&mut self) -> DoubleWriter<'_> {
        DoubleWriter::from_existing(self.buf)
    }

    /// Build the full scene tree for the text editor screen layout.
    /// Writes to the back buffer and swaps to publish as the new front.
    ///
    /// Text layout (line breaking, cursor/selection positioning) happens
    /// here. Each visible text line becomes a Content::Glyphs child node
    /// under N_DOC_TEXT. Title and clock are single Content::Glyphs nodes.
    #[allow(clippy::too_many_arguments)]
    pub fn build_editor_scene(
        &mut self,
        fb_width: u32,
        fb_height: u32,
        title_bar_h: u32,
        shadow_depth: u32,
        text_inset_x: u32,
        _text_inset_top: u32,
        chrome_bg: drawing::Color,
        chrome_border: drawing::Color,
        chrome_title_color: drawing::Color,
        chrome_clock_color: drawing::Color,
        bg_color: drawing::Color,
        text_color: drawing::Color,
        cursor_color: drawing::Color,
        sel_color: drawing::Color,
        font_size: u16,
        char_width: u32,
        line_height: u32,
        doc_text: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        title_label: &[u8],
        clock_text: &[u8],
        scroll_y: i32,
    ) {
        let dc = |c: drawing::Color| -> Color { Color::rgba(c.r, c.g, c.b, c.a) };
        let scene_text_color = dc(text_color);
        // Layout document text into visual lines (monospace line-breaking).
        let doc_width = fb_width.saturating_sub(2 * text_inset_x);
        let chars_per_line = if char_width > 0 {
            (doc_width / char_width).max(1)
        } else {
            80
        };
        let all_runs = layout_mono_lines(
            doc_text,
            chars_per_line as usize,
            line_height as i16,
            scene_text_color,
            font_size,
        );
        // Apply scroll: filter to visible viewport, adjust y positions.
        let content_y = title_bar_h + shadow_depth;
        let content_h = fb_height.saturating_sub(content_y) as i32;
        let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
        let visible_runs = scroll_runs(all_runs, scroll_lines, line_height, content_h);
        // Scroll offset in pixels for cursor/selection positioning.
        let scroll_px = scroll_lines as i32 * line_height as i32;
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
        let mut dw = self.double();

        {
            let mut w = dw.back();

            w.clear();

            // Push shaped glyph arrays for title and clock.
            let title_glyphs = bytes_to_shaped_glyphs(title_label, char_width as u16);
            let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
            let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width as u16);
            let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

            // Push visible line glyph data.
            let mut line_glyph_refs: Vec<(DataRef, u16, i16)> =
                Vec::with_capacity(visible_runs.len());

            for run in &visible_runs {
                let line_text = line_bytes_for_run(doc_text, run);
                let shaped = bytes_to_shaped_glyphs(line_text, char_width as u16);
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
                n.width = fb_width as u16;
                n.height = fb_height as u16;
                n.background = dc(bg_color);
                n.flags = NodeFlags::VISIBLE;
            }
            {
                let n = w.node_mut(N_TITLE_BAR);

                n.first_child = N_TITLE_TEXT;
                n.next_sibling = N_SHADOW;
                n.width = fb_width as u16;
                n.height = title_bar_h as u16;
                n.background = dc(chrome_bg);
                n.border = Border {
                    color: dc(chrome_border),
                    width: 1,
                    _pad: [0; 3],
                };
                n.flags = NodeFlags::VISIBLE;
                // Real blurred shadow below the title bar.
                n.shadow_color = Color::rgba(0, 0, 0, 60);
                n.shadow_offset_x = 0;
                n.shadow_offset_y = shadow_depth as i16;
                n.shadow_blur_radius = (shadow_depth as u8).min(8);
                n.shadow_spread = 0;
            }

            let text_y_offset = (title_bar_h.saturating_sub(line_height)) / 2;

            {
                let n = w.node_mut(N_TITLE_TEXT);

                n.next_sibling = N_CLOCK_TEXT;
                n.x = 12;
                n.y = text_y_offset as i16;
                n.width = (fb_width / 2) as u16;
                n.height = line_height as u16;
                n.content = Content::Glyphs {
                    color: dc(chrome_title_color),
                    glyphs: title_glyph_ref,
                    glyph_count: title_glyphs.len() as u16,
                    font_size,
                    axis_hash: 0,
                };
                n.content_hash = fnv1a(title_label);
                n.flags = NodeFlags::VISIBLE;
            }

            let clock_x = (fb_width - 12 - 80) as i16;

            {
                let n = w.node_mut(N_CLOCK_TEXT);

                n.x = clock_x;
                n.y = text_y_offset as i16;
                n.width = 80;
                n.height = line_height as u16;
                n.content = Content::Glyphs {
                    color: dc(chrome_clock_color),
                    glyphs: clock_glyph_ref,
                    glyph_count: clock_glyphs.len() as u16,
                    font_size,
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
                n.y = title_bar_h as i16;
                n.width = fb_width as u16;
                n.height = 0;
                n.background = Color::TRANSPARENT;
                n.flags = NodeFlags::VISIBLE;
            }

            let content_y = title_bar_h + shadow_depth;
            let content_h = fb_height.saturating_sub(content_y);

            {
                let n = w.node_mut(N_CONTENT);

                n.first_child = N_DOC_TEXT;
                n.next_sibling = NULL;
                n.y = content_y as i16;
                n.width = fb_width as u16;
                n.height = content_h as u16;
                n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
            }
            {
                let n = w.node_mut(N_DOC_TEXT);

                n.x = text_inset_x as i16;
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
                    n.height = line_height as u16;
                    n.content = Content::Glyphs {
                        color: scene_text_color,
                        glyphs: glyph_ref,
                        glyph_count,
                        font_size,
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
            let cursor_x = (cursor_col as u32 * char_width) as i16;
            let cursor_y = (cursor_line as i32 * line_height as i32 - scroll_px) as i16;

            {
                let n = w.node_mut(N_CURSOR);

                n.x = cursor_x;
                n.y = cursor_y;
                n.width = 2;
                n.height = line_height as u16;
                n.content = Content::FillRect {
                    color: dc(cursor_color),
                };
                n.flags = NodeFlags::VISIBLE;
                n.next_sibling = NULL;
            }

            // Selection highlight rectangles (dynamically allocated, scroll-adjusted).
            if has_selection {
                allocate_selection_rects(
                    &mut w,
                    doc_text,
                    sel_lo,
                    sel_hi,
                    chars_per_line as usize,
                    char_width,
                    line_height,
                    dc(sel_color),
                    content_h,
                    scroll_px,
                );
            }

            w.set_root(N_ROOT);
        }

        dw.swap();
    }

    // ── Targeted incremental update methods ─────────────────────────

    /// Update only the clock text glyphs in-place. Zero heap allocations
    /// for the glyph data: the old ShapedGlyph array is overwritten via
    /// `update_data` (same length). Only N_CLOCK_TEXT is marked changed.
    pub fn update_clock(&mut self, clock_text: &[u8]) {
        let mut dw = self.double();
        if !dw.copy_front_to_back() {
            return; // Reader still on back buffer — skip update.
        }

        let mut updated_ok = false;

        {
            let mut w = dw.back();

            // Read the clock node's Content::Glyphs to find the glyph DataRef.
            let clock_node = w.node(N_CLOCK_TEXT);
            if let Content::Glyphs { glyphs, .. } = clock_node.content {
                // Build new glyphs from clock_text using same advance.
                let advance = 8u16; // monospace char width
                let new_glyphs = bytes_to_shaped_glyphs(clock_text, advance);

                // SAFETY: ShapedGlyph is repr(C) with no padding.
                let new_bytes = unsafe {
                    core::slice::from_raw_parts(
                        new_glyphs.as_ptr() as *const u8,
                        new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                    )
                };

                // In-place overwrite (same length: 8 glyphs = 64 bytes).
                if w.update_data(glyphs, new_bytes) {
                    w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(clock_text);
                    w.mark_changed(N_CLOCK_TEXT);
                    updated_ok = true;
                }
            }
        }

        if updated_ok {
            dw.swap();
        }
    }

    /// Update only the cursor position. Zero heap allocations.
    /// Only N_CURSOR is marked changed.
    pub fn update_cursor(
        &mut self,
        cursor_pos: u32,
        doc_text: &[u8],
        chars_per_line: u32,
        char_width: u32,
        line_height: u32,
        scroll_px: i32,
        clock_text: Option<&[u8]>,
    ) {
        let mut dw = self.double();
        if !dw.copy_front_to_back() {
            return;
        }

        {
            let mut w = dw.back();

            let (cursor_line, cursor_col) =
                byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line as usize);
            let cursor_x = (cursor_col as u32 * char_width) as i16;
            let cursor_y = (cursor_line as i32 * line_height as i32 - scroll_px) as i16;

            let n = w.node_mut(N_CURSOR);
            n.x = cursor_x;
            n.y = cursor_y;

            w.mark_changed(N_CURSOR);

            if let Some(ct) = clock_text {
                update_clock_inline(&mut w, ct);
            }
        }

        dw.swap();
    }

    /// Update cursor position and selection rects.
    #[allow(clippy::too_many_arguments)]
    pub fn update_selection(
        &mut self,
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        doc_text: &[u8],
        chars_per_line: u32,
        char_width: u32,
        line_height: u32,
        sel_color: Color,
        content_h: u32,
        scroll_px: i32,
        clock_text: Option<&[u8]>,
    ) {
        let mut dw = self.double();
        if !dw.copy_front_to_back() {
            return;
        }

        {
            let mut w = dw.back();

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
            let cursor_x = (cursor_col as u32 * char_width) as i16;
            let cursor_y = (cursor_line as i32 * line_height as i32 - scroll_px) as i16;

            {
                let n = w.node_mut(N_CURSOR);
                n.x = cursor_x;
                n.y = cursor_y;
                n.next_sibling = NULL;
            }
            w.mark_changed(N_CURSOR);

            if let Some(ct) = clock_text {
                update_clock_inline(&mut w, ct);
            }

            let (sel_lo, sel_hi) = if sel_start <= sel_end {
                (sel_start as usize, sel_end as usize)
            } else {
                (sel_end as usize, sel_start as usize)
            };

            if sel_lo < sel_hi {
                allocate_selection_rects(
                    &mut w,
                    doc_text,
                    sel_lo,
                    sel_hi,
                    chars_per_line as usize,
                    char_width,
                    line_height,
                    sel_color,
                    content_h,
                    scroll_px,
                );
            }
        }

        dw.swap();
    }

    /// Update document content (line nodes + cursor + selection).
    /// Compacts the data buffer by resetting it and re-pushing all data.
    #[allow(clippy::too_many_arguments)]
    pub fn update_document_content(
        &mut self,
        fb_width: u32,
        fb_height: u32,
        title_bar_h: u32,
        shadow_depth: u32,
        text_inset_x: u32,
        _text_inset_top: u32,
        _chrome_bg: drawing::Color,
        _chrome_border: drawing::Color,
        chrome_title_color: drawing::Color,
        chrome_clock_color: drawing::Color,
        _bg_color: drawing::Color,
        text_color: drawing::Color,
        _cursor_color: drawing::Color,
        sel_color: drawing::Color,
        font_size: u16,
        char_width: u32,
        line_height: u32,
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
        let scene_text_color = dc(text_color);

        let doc_width = fb_width.saturating_sub(2 * text_inset_x);
        let chars_per_line = if char_width > 0 {
            (doc_width / char_width).max(1)
        } else {
            80
        };
        let content_y = title_bar_h + shadow_depth;
        let content_h = fb_height.saturating_sub(content_y);
        let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
        let scroll_px = scroll_lines as i32 * line_height as i32;

        let mut dw = self.double();
        if !dw.copy_front_to_back() {
            return;
        }

        {
            let mut w = dw.back();

            // Remove old dynamic nodes (line nodes + selection rects).
            w.set_node_count(WELL_KNOWN_COUNT);

            // ── Data buffer compaction ──────────────────────────────
            w.reset_data();

            // Re-push title glyph data.
            let title_glyphs = bytes_to_shaped_glyphs(title_label, char_width as u16);
            let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

            // Re-push clock glyph data.
            let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width as u16);
            let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

            // Re-layout visible document text lines.
            let all_runs = layout_mono_lines(
                doc_text,
                chars_per_line as usize,
                line_height as i16,
                scene_text_color,
                font_size,
            );
            let viewport_height_px = content_h as i32;
            let visible_runs = scroll_runs(all_runs, scroll_lines, line_height, viewport_height_px);

            // Push visible line glyph data.
            let mut line_glyph_refs: Vec<(DataRef, u16, i16)> =
                Vec::with_capacity(visible_runs.len());

            for run in &visible_runs {
                let line_text = line_bytes_for_run(doc_text, run);
                let shaped = bytes_to_shaped_glyphs(line_text, char_width as u16);
                let glyph_ref = w.push_shaped_glyphs(&shaped);

                line_glyph_refs.push((glyph_ref, shaped.len() as u16, run.y));
            }

            // Update N_TITLE_TEXT content references (data was reset).
            {
                let n = w.node_mut(N_TITLE_TEXT);
                n.content = Content::Glyphs {
                    color: dc(chrome_title_color),
                    glyphs: title_glyph_ref,
                    glyph_count: title_glyphs.len() as u16,
                    font_size,
                    axis_hash: 0,
                };
                n.content_hash = fnv1a(title_label);
            }

            // Update N_CLOCK_TEXT content references (data was reset).
            {
                let n = w.node_mut(N_CLOCK_TEXT);
                n.content = Content::Glyphs {
                    color: dc(chrome_clock_color),
                    glyphs: clock_glyph_ref,
                    glyph_count: clock_glyphs.len() as u16,
                    font_size,
                    axis_hash: 0,
                };
                n.content_hash = fnv1a(clock_text);
            }
            if mark_clock_changed {
                w.mark_changed(N_CLOCK_TEXT);
            }

            // Re-create per-line Glyphs children under N_DOC_TEXT.
            w.node_mut(N_DOC_TEXT).first_child = NULL;
            w.node_mut(N_DOC_TEXT).content = Content::None;
            w.node_mut(N_DOC_TEXT).content_hash = fnv1a(doc_text);
            let mut prev_line_node: u16 = NULL;

            for &(glyph_ref, glyph_count, y) in &line_glyph_refs {
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
            let cursor_x = (cursor_col as u32 * char_width) as i16;
            let cursor_y = (cursor_line as i32 * line_height as i32 - scroll_px) as i16;

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
                    &mut w,
                    doc_text,
                    sel_lo,
                    sel_hi,
                    chars_per_line as usize,
                    char_width,
                    line_height,
                    dc(sel_color),
                    content_h,
                    scroll_px,
                );
            }
        }

        dw.swap();
    }
}

/// Update the clock text in-place within an already-open back buffer.
fn update_clock_inline(w: &mut scene::SceneWriter<'_>, clock_text: &[u8]) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Glyphs { glyphs, .. } = clock_node.content {
        let advance = 8u16; // monospace char width
        let new_glyphs = bytes_to_shaped_glyphs(clock_text, advance);

        // SAFETY: ShapedGlyph is repr(C) with no padding.
        let new_bytes = unsafe {
            core::slice::from_raw_parts(
                new_glyphs.as_ptr() as *const u8,
                new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
            )
        };

        if w.update_data(glyphs, new_bytes) {
            w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(clock_text);
            w.mark_changed(N_CLOCK_TEXT);
        }
    }
}

/// Allocate selection rectangle nodes as children of N_DOC_TEXT (after
/// the cursor node). Each line of the selection gets one rect node.
#[allow(clippy::too_many_arguments)]
fn allocate_selection_rects(
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
            n.content = Content::FillRect { color: sel_color };
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

// ── Monospace text layout helpers ───────────────────────────────────

/// Convert a byte offset to (visual_line, column) with monospace wrapping.
fn byte_to_line_col(text: &[u8], byte_offset: usize, chars_per_line: usize) -> (usize, usize) {
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
fn layout_mono_lines(
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
fn line_bytes_for_run<'a>(text: &'a [u8], run: &LayoutRun) -> &'a [u8] {
    let start = run.glyphs.offset as usize;
    let len = run.glyphs.length as usize;

    if start + len <= text.len() {
        &text[start..start + len]
    } else {
        &[]
    }
}

/// Filter and reposition runs for a scrolled viewport.
fn scroll_runs(
    runs: Vec<LayoutRun>,
    scroll_lines: u32,
    line_height: u32,
    viewport_height_px: i32,
) -> Vec<LayoutRun> {
    let scroll_px = scroll_lines as i32 * line_height as i32;

    runs.into_iter()
        .filter_map(|mut run| {
            let adjusted_y = run.y as i32 - scroll_px;

            if adjusted_y + line_height as i32 <= 0 {
                return None;
            }
            if adjusted_y >= viewport_height_px {
                return None;
            }

            run.y = adjusted_y as i16;

            Some(run)
        })
        .collect()
}

/// Convert raw ASCII text bytes into ShapedGlyph arrays for monospace rendering.
fn bytes_to_shaped_glyphs(text: &[u8], advance: u16) -> Vec<ShapedGlyph> {
    text.iter()
        .map(|&ch| ShapedGlyph {
            glyph_id: ch as u16,
            x_advance: advance as i16,
            x_offset: 0,
            y_offset: 0,
        })
        .collect()
}
