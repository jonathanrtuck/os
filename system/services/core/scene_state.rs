//! Mutable scene graph state backed by a triple-buffered shared memory layout.
//!
//! Wraps a `TripleWriter` operating on shared memory. The core process
//! acquires a free buffer, builds the scene, then publishes. The compositor
//! reads the latest published buffer from the same shared memory region.
//! Mailbox semantics: the writer always has a free buffer (never blocks),
//! and intermediate frames are silently skipped.
//!
//! This module owns the triple buffer lifecycle (acquire/publish). Scene
//! building logic lives in the `layout` module.

use scene::{TripleWriter, TRIPLE_SCENE_SIZE};

use super::layout::{
    build_clock_update, build_cursor_update, build_document_content, build_full_scene,
    build_selection_update, update_single_line,
};
// Re-export layout types and constants used by main.rs.
pub use super::layout::{
    byte_to_line_col, count_lines, SceneConfig, N_CLOCK_TEXT, N_CONTENT, N_CURSOR, N_DOC_TEXT,
    N_ROOT, N_SHADOW, N_TITLE_BAR, N_TITLE_TEXT, WELL_KNOWN_COUNT,
};

pub struct SceneState {
    buf: &'static mut [u8],
}

impl SceneState {
    /// Create from an externally-provided buffer (shared memory).
    pub fn from_buf(buf: &'static mut [u8]) -> Self {
        assert!(buf.len() >= TRIPLE_SCENE_SIZE);

        let _ = TripleWriter::new(buf);

        Self { buf }
    }

    fn triple(&mut self) -> TripleWriter<'_> {
        TripleWriter::from_existing(self.buf)
    }

    /// Build the full scene tree for the text editor screen layout.
    /// Writes to the back buffer and swaps to publish as the new front.
    #[allow(clippy::too_many_arguments)]
    pub fn build_editor_scene(
        &mut self,
        cfg: &SceneConfig,
        doc_text: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        title_label: &[u8],
        clock_text: &[u8],
        scroll_y: i32,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire();
            build_full_scene(
                &mut w,
                cfg,
                doc_text,
                cursor_pos,
                sel_start,
                sel_end,
                title_label,
                clock_text,
                scroll_y,
            );
        }
        tw.publish();
    }

    /// Update only the clock text glyphs via re-push.
    pub fn update_clock(&mut self, cfg: &SceneConfig, clock_text: &[u8]) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_clock_update(&mut w, cfg, clock_text);
        }
        tw.publish();
    }

    /// Update only the cursor position. Zero heap allocations.
    pub fn update_cursor(
        &mut self,
        cfg: &SceneConfig,
        cursor_pos: u32,
        doc_text: &[u8],
        chars_per_line: u32,
        clock_text: Option<&[u8]>,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_cursor_update(
                &mut w,
                cfg,
                cursor_pos,
                doc_text,
                chars_per_line,
                clock_text,
            );
        }
        tw.publish();
    }

    /// Update cursor position and selection rects.
    #[allow(clippy::too_many_arguments)]
    pub fn update_selection(
        &mut self,
        cfg: &SceneConfig,
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        doc_text: &[u8],
        content_h: u32,
        scroll_px: i32,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_selection_update(
                &mut w, cfg, cursor_pos, sel_start, sel_end, doc_text, content_h, scroll_px,
            );
        }
        tw.publish();
    }

    /// Incremental text update for same-line-count edits.
    /// Falls back to full rebuild (compaction) if the data buffer is full.
    #[allow(clippy::too_many_arguments)]
    pub fn update_document_incremental(
        &mut self,
        cfg: &SceneConfig,
        doc_text: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        changed_line: usize,
        title_label: &[u8],
        clock_text: &[u8],
        scroll_y: i32,
        timer_fired: bool,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            let success = update_single_line(
                &mut w,
                cfg,
                doc_text,
                changed_line,
                cursor_pos,
                sel_start,
                sel_end,
                scroll_y,
                if timer_fired { Some(clock_text) } else { None },
            );
            if !success {
                // Compaction: fall back to full rebuild.
                build_document_content(
                    &mut w,
                    cfg,
                    doc_text,
                    cursor_pos,
                    sel_start,
                    sel_end,
                    title_label,
                    clock_text,
                    scroll_y,
                    timer_fired,
                );
            }
        }
        tw.publish();
    }

    /// Update document content (line nodes + cursor + selection).
    /// Compacts the data buffer by resetting it and re-pushing all data.
    #[allow(clippy::too_many_arguments)]
    pub fn update_document_content(
        &mut self,
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
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_document_content(
                &mut w,
                cfg,
                doc_text,
                cursor_pos,
                sel_start,
                sel_end,
                title_label,
                clock_text,
                scroll_y,
                mark_clock_changed,
            );
        }
        tw.publish();
    }
}
