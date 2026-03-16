//! Mutable scene graph state backed by a double-buffered shared memory layout.
//!
//! Wraps a `DoubleWriter` operating on shared memory. The core process
//! builds each frame into the back buffer, then `swap()` publishes it.
//! The compositor reads the front buffer from the same shared memory region.

use alloc::vec::Vec;

use scene::{
    fnv1a, Border, Color, Content, DataRef, DoubleWriter, NodeFlags, ShapedGlyph, TextRun,
    DATA_BUFFER_SIZE, DOUBLE_SCENE_SIZE, NULL,
};

/// Well-known node indices for direct mutation.
pub const N_ROOT: u16 = 0;
pub const N_TITLE_BAR: u16 = 1;
pub const N_TITLE_TEXT: u16 = 2;
pub const N_CLOCK_TEXT: u16 = 3;
pub const N_SHADOW: u16 = 4;
pub const N_CONTENT: u16 = 5;
pub const N_DOC_TEXT: u16 = 6;
pub const N_CURSOR: u16 = 7;

/// Number of well-known nodes (indices 0..7). Selection rects start at 8.
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
    /// here — this is the proto-OS-service. The compositor just renders
    /// the resulting positioned TextRun arrays and rectangle nodes.
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
            char_width as u16,
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
            // Push only visible line glyph data (scroll-filtered).
            let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());

            for mut run in visible_runs {
                let line_text = line_bytes_for_run(doc_text, &run);
                let shaped = bytes_to_shaped_glyphs(line_text, char_width as u16);

                run.glyphs = w.push_shaped_glyphs(&shaped);
                run.glyph_count = shaped.len() as u16;

                final_runs.push(run);
            }

            let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);
            // Build title/clock as single-run text.
            let title_run = TextRun {
                glyphs: title_glyph_ref,
                glyph_count: title_glyphs.len() as u16,
                x: 0,
                y: 0,
                color: dc(chrome_title_color),
                advance: char_width as u16,
                font_size,
                axis_hash: 0,
            };
            let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);
            let clock_run = TextRun {
                glyphs: clock_glyph_ref,
                glyph_count: clock_glyphs.len() as u16,
                x: 0,
                y: 0,
                color: dc(chrome_clock_color),
                advance: char_width as u16,
                font_size,
                axis_hash: 0,
            };
            let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);
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
                n.content = Content::Text {
                    runs: title_runs_ref,
                    run_count: title_run_count,
                    _pad: [0; 2],
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
                n.content = Content::Text {
                    runs: clock_runs_ref,
                    run_count: clock_run_count,
                    _pad: [0; 2],
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

                n.first_child = N_CURSOR;
                n.x = text_inset_x as i16;
                n.y = 8;
                n.width = doc_width as u16;
                n.height = content_h as u16;
                // scroll_y = 0: core pre-applies scroll to run positions
                // and cursor/selection rects. The compositor just renders.
                n.scroll_y = 0;
                n.content = Content::Text {
                    runs: doc_runs_ref,
                    run_count: doc_run_count,
                    _pad: [0; 2],
                };
                n.content_hash = fnv1a(doc_text);
                n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
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
                n.background = dc(cursor_color);
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
    ///
    /// If `update_data` returns false (length mismatch — should not happen
    /// for clock text, but defensive), falls back to a full rebuild via
    /// `build_editor_scene`.
    pub fn update_clock(&mut self, clock_text: &[u8]) {
        let mut dw = self.double();
        if !dw.copy_front_to_back() {
            return; // Reader still on back buffer — skip update.
        }

        let mut updated_ok = false;

        {
            let mut w = dw.back();

            // Read the clock node's Content::Text to find the glyph DataRef.
            let clock_node = w.node(N_CLOCK_TEXT);
            if let Content::Text { runs, .. } = clock_node.content {
                // Resolve the TextRun from the data buffer.
                let run_size = core::mem::size_of::<TextRun>();
                let runs_start = scene::DATA_OFFSET + runs.offset as usize;
                let runs_end = runs_start + run_size;
                if runs_end <= w.data_buf().len() + scene::DATA_OFFSET {
                    // SAFETY: TextRun is repr(C) and the data buffer is
                    // aligned to TextRun alignment by push_text_runs.
                    let run_ptr = unsafe {
                        (w.data_buf().as_ptr() as *const u8)
                            .add(runs.offset as usize) as *const TextRun
                    };
                    let text_run = unsafe { core::ptr::read(run_ptr) };
                    let glyph_dref = text_run.glyphs;

                    // Build new glyphs from clock_text.
                    let new_glyphs = bytes_to_shaped_glyphs(clock_text, text_run.advance);

                    // SAFETY: ShapedGlyph is repr(C) with no padding.
                    // Clock text is always exactly 8 bytes ("HH:MM:SS"
                    // from format_time_hms), producing 8 ShapedGlyph
                    // entries = 64 bytes — matching the original glyph
                    // data length. update_data will return false if a
                    // length mismatch occurs (defensive guard).
                    let new_bytes = unsafe {
                        core::slice::from_raw_parts(
                            new_glyphs.as_ptr() as *const u8,
                            new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                        )
                    };

                    // In-place overwrite (same length: 8 glyphs = 64 bytes).
                    if w.update_data(glyph_dref, new_bytes) {
                        // Update content_hash to reflect new clock text.
                        w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(clock_text);
                        w.mark_changed(N_CLOCK_TEXT);
                        updated_ok = true;
                    }
                }
            }
        }

        if updated_ok {
            dw.swap();
        }
        // If update_data failed (length mismatch), skip the swap — the
        // caller (event loop) will detect that no scene update occurred
        // and the next timer tick will retry. In practice this path is
        // unreachable because clock text is always 8 bytes.
    }

    /// Update only the cursor position. Zero heap allocations.
    /// Only N_CURSOR is marked changed.
    ///
    /// When `clock_text` is `Some`, also updates the clock glyph data
    /// in-place and marks N_CLOCK_TEXT changed — used when a timer tick
    /// coincides with a cursor-only change so both are updated in one frame.
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
            return; // Reader still on back buffer — skip update.
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

            // Update clock in-place if timer fired simultaneously.
            if let Some(ct) = clock_text {
                update_clock_inline(&mut w, ct);
            }
        }

        dw.swap();
    }

    /// Update cursor position and selection rects. Truncates node count
    /// to WELL_KNOWN_COUNT (removing old selection rects), updates the
    /// cursor's x/y, then allocates new selection rects. Marks N_CURSOR
    /// and all new selection nodes as changed.
    ///
    /// When `clock_text` is `Some`, also updates the clock glyph data
    /// in-place and marks N_CLOCK_TEXT changed — used when a timer tick
    /// coincides with a selection change so both are updated in one frame.
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
            return; // Reader still on back buffer — skip update.
        }

        {
            let mut w = dw.back();

            // Remove old selection rects by truncating node count.
            w.set_node_count(WELL_KNOWN_COUNT);

            // Update cursor position (e.g., click-to-reposition).
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

            // Update clock in-place if timer fired simultaneously.
            if let Some(ct) = clock_text {
                update_clock_inline(&mut w, ct);
            }

            // Build new selection rects if selection exists.
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

    /// Update document content (text runs + cursor + selection).
    /// Compacts the data buffer by resetting it and re-pushing all text
    /// data (title, clock, document). This keeps data_used proportional
    /// to visible content regardless of how many incremental updates
    /// have occurred — no fallback to full rebuild from data exhaustion.
    /// Marks N_DOC_TEXT, N_CURSOR, and any selection nodes as changed.
    /// When `mark_clock_changed` is true, also marks N_CLOCK_TEXT as
    /// changed — used when a timer tick coincides with a text change so
    /// both the document and clock are updated in a single frame.
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
            return; // Reader still on back buffer — skip update.
        }

        {
            let mut w = dw.back();

            // Remove old selection rects by truncating node count.
            w.set_node_count(WELL_KNOWN_COUNT);

            // ── Data buffer compaction ──────────────────────────────
            // Reset the data buffer and re-push all text data (title,
            // clock, document). This keeps data_used bounded to the
            // current visible content instead of accumulating old data
            // across incremental updates.
            w.reset_data();

            // Re-push title glyph data and TextRun.
            let title_glyphs = bytes_to_shaped_glyphs(title_label, char_width as u16);
            let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
            let title_run = TextRun {
                glyphs: title_glyph_ref,
                glyph_count: title_glyphs.len() as u16,
                x: 0,
                y: 0,
                color: dc(chrome_title_color),
                advance: char_width as u16,
                font_size,
                axis_hash: 0,
            };
            let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);

            // Re-push clock glyph data and TextRun.
            let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width as u16);
            let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);
            let clock_run = TextRun {
                glyphs: clock_glyph_ref,
                glyph_count: clock_glyphs.len() as u16,
                x: 0,
                y: 0,
                color: dc(chrome_clock_color),
                advance: char_width as u16,
                font_size,
                axis_hash: 0,
            };
            let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);

            // Re-layout visible document text lines.
            let all_runs = layout_mono_lines(
                doc_text,
                chars_per_line as usize,
                line_height as i16,
                scene_text_color,
                char_width as u16,
                font_size,
            );
            let viewport_height_px = content_h as i32;
            let visible_runs = scroll_runs(all_runs, scroll_lines, line_height, viewport_height_px);

            let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());

            for mut run in visible_runs {
                let line_text = line_bytes_for_run(doc_text, &run);
                let shaped = bytes_to_shaped_glyphs(line_text, char_width as u16);

                run.glyphs = w.push_shaped_glyphs(&shaped);
                run.glyph_count = shaped.len() as u16;

                final_runs.push(run);
            }

            let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

            // Update N_TITLE_TEXT content references (data was reset).
            {
                let n = w.node_mut(N_TITLE_TEXT);
                n.content = Content::Text {
                    runs: title_runs_ref,
                    run_count: title_run_count,
                    _pad: [0; 2],
                };
                n.content_hash = fnv1a(title_label);
            }

            // Update N_CLOCK_TEXT content references (data was reset).
            {
                let n = w.node_mut(N_CLOCK_TEXT);
                n.content = Content::Text {
                    runs: clock_runs_ref,
                    run_count: clock_run_count,
                    _pad: [0; 2],
                };
                n.content_hash = fnv1a(clock_text);
            }
            if mark_clock_changed {
                w.mark_changed(N_CLOCK_TEXT);
            }

            // Update N_DOC_TEXT content.
            {
                let n = w.node_mut(N_DOC_TEXT);
                n.content = Content::Text {
                    runs: doc_runs_ref,
                    run_count: doc_run_count,
                    _pad: [0; 2],
                };
                n.content_hash = fnv1a(doc_text);
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
/// Uses the same technique as `SceneState::update_clock`: reads the
/// glyph DataRef from N_CLOCK_TEXT's TextRun, overwrites the shaped
/// glyph array with new clock text bytes, and marks N_CLOCK_TEXT changed.
///
/// This is a building block for combining clock updates with other
/// incremental updates (cursor, selection) in a single copy/swap cycle,
/// avoiding the full rebuild that would otherwise be needed when a
/// timer tick coincides with an input change.
fn update_clock_inline(w: &mut scene::SceneWriter<'_>, clock_text: &[u8]) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Text { runs, .. } = clock_node.content {
        let run_size = core::mem::size_of::<TextRun>();
        let runs_offset = runs.offset as usize;
        if runs_offset + run_size <= w.data_buf().len() {
            // SAFETY: TextRun is repr(C) and the data buffer is aligned
            // to TextRun alignment by push_text_runs.
            let run_ptr =
                unsafe { (w.data_buf().as_ptr() as *const u8).add(runs_offset) as *const TextRun };
            let text_run = unsafe { core::ptr::read(run_ptr) };
            let glyph_dref = text_run.glyphs;

            // Build new glyphs from clock_text.
            let new_glyphs = bytes_to_shaped_glyphs(clock_text, text_run.advance);

            // SAFETY: ShapedGlyph is repr(C) with no padding. Clock text
            // is always exactly 8 bytes ("HH:MM:SS"), producing 8 glyphs
            // = 64 bytes — matching the original glyph data length.
            let new_bytes = unsafe {
                core::slice::from_raw_parts(
                    new_glyphs.as_ptr() as *const u8,
                    new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                )
            };

            if w.update_data(glyph_dref, new_bytes) {
                w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(clock_text);
                w.mark_changed(N_CLOCK_TEXT);
            }
        }
    }
}

/// Allocate selection rectangle nodes as children of N_DOC_TEXT (after
/// the cursor node). Each line of the selection gets one rect node.
/// Marks each new selection node as changed.
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

        // Scroll-adjust selection rect y position.
        let sel_y = line as i32 * line_height as i32 - scroll_px;

        // Skip selection rects outside visible viewport.
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
///
/// Returns one `TextRun` per visual line. The `glyphs` DataRef is a
/// placeholder: `offset` = byte position in source text, `length` =
/// byte count. The caller must replace these with actual `push_data`
/// results before writing to the scene graph.
fn layout_mono_lines(
    text: &[u8],
    chars_per_line: usize,
    line_height: i16,
    color: Color,
    advance: u16,
    font_size: u16,
) -> Vec<TextRun> {
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

        runs.push(TextRun {
            glyphs: DataRef {
                offset: pos as u32,
                length: line_len as u32,
            },
            glyph_count: line_len as u16,
            x: 0,
            y: line_y,
            color,
            advance,
            font_size,
            axis_hash: 0,
        });

        line_y = line_y.saturating_add(line_height);
        pos = if line_end < text.len() && text[line_end] == b'\n' {
            line_end + 1
        } else {
            line_end
        };
    }

    if runs.is_empty() {
        runs.push(TextRun {
            glyphs: DataRef {
                offset: 0,
                length: 0,
            },
            glyph_count: 0,
            x: 0,
            y: 0,
            color,
            advance,
            font_size,
            axis_hash: 0,
        });
    }

    runs
}

/// Extract source text bytes for a run using its placeholder DataRef.
fn line_bytes_for_run<'a>(text: &'a [u8], run: &TextRun) -> &'a [u8] {
    let start = run.glyphs.offset as usize;
    let len = run.glyphs.length as usize;

    if start + len <= text.len() {
        &text[start..start + len]
    } else {
        &[]
    }
}

/// Filter and reposition runs for a scrolled viewport.
///
/// `scroll_lines` is the number of visual lines scrolled (top of viewport).
/// `viewport_height_px` is the visible area height in pixels.
/// Returns only runs visible in the viewport, with y adjusted.
fn scroll_runs(
    runs: Vec<TextRun>,
    scroll_lines: u32,
    line_height: u32,
    viewport_height_px: i32,
) -> Vec<TextRun> {
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
///
/// Each byte becomes a glyph with `glyph_id` = byte value (the compositor maps
/// these via cmap). The advance is uniform (monospace). This bridges the old
/// byte-based path to the new shaped glyph scene graph format.
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
