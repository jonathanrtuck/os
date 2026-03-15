//! Mutable scene graph state backed by a double-buffered shared memory layout.
//!
//! Wraps a `DoubleWriter` operating on shared memory. The core process
//! builds each frame into the back buffer, then `swap()` publishes it.
//! The compositor reads the front buffer from the same shared memory region.

use alloc::vec::Vec;

use scene::{
    byte_to_line_col, layout_mono_lines, line_bytes_for_run, scroll_runs, Border, Color, Content,
    DataRef, DoubleWriter, NodeFlags, ShapedGlyph, TextRun, DOUBLE_SCENE_SIZE, NULL,
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

/// Convert raw ASCII text bytes into ShapedGlyph arrays for monospace rendering.
///
/// Each byte becomes a glyph with glyph_id = byte value (the compositor will
/// map these via cmap). The advance is uniform (monospace). This bridges the
/// old byte-based path to the new shaped glyph scene graph format.
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
                n.flags = NodeFlags::VISIBLE;
            }
            {
                let n = w.node_mut(N_SHADOW);

                n.next_sibling = N_CONTENT;
                n.y = title_bar_h as i16;
                n.width = fb_width as u16;
                n.height = shadow_depth as u16;
                n.background = Color::rgba(0, 0, 0, 40);
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
                let (sel_start_line, sel_start_col) =
                    byte_to_line_col(doc_text, sel_lo, chars_per_line as usize);
                let (sel_end_line, sel_end_col) =
                    byte_to_line_col(doc_text, sel_hi, chars_per_line as usize);
                let sel_bg = dc(sel_color);
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
                        chars_per_line as usize
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
                        n.background = sel_bg;
                        n.flags = NodeFlags::VISIBLE;
                        n.next_sibling = NULL;

                        if prev_sel_node == NULL {
                            w.node_mut(N_CURSOR).next_sibling = sel_id;
                        } else {
                            w.node_mut(prev_sel_node).next_sibling = sel_id;
                        }

                        prev_sel_node = sel_id;
                    }
                }
            }

            w.set_root(N_ROOT);
        }

        dw.swap();
    }
}
