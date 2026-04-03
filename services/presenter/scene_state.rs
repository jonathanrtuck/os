//! Mutable scene graph state backed by a triple-buffered shared memory layout.
//!
//! Wraps a `TripleWriter` operating on shared memory. The core process
//! acquires a free buffer, builds the scene, then publishes. The compositor
//! reads the latest published buffer from the same shared memory region.
//! Mailbox semantics: the writer always has a free buffer (never blocks),
//! and intermediate frames are silently skipped.
//!
//! This module owns the triple buffer lifecycle (acquire/publish). Scene
//! building logic lives in the `scene` module.

use scene::{TripleWriter, TRIPLE_SCENE_SIZE};

use super::layout::{
    build_clock_update, build_cursor_update, build_document_content, build_full_scene,
    build_loading_scene, build_rich_document_content, build_selection_update, delete_line,
    insert_line, update_single_line, update_spinner_angle,
};
// Re-export scene types and constants used by main.rs.
pub use super::layout::{
    byte_to_line_col, count_lines, SceneConfig, N_CLOCK_TEXT, N_CONTENT, N_CURSOR, N_DOC_IMAGE,
    N_DOC_TEXT, N_PAGE, N_ROOT, N_SHADOW, N_STRIP, N_TITLE_BAR, N_TITLE_ICON, N_TITLE_TEXT,
    WELL_KNOWN_COUNT,
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

    /// Current scene graph generation (incremented on each publish).
    pub fn generation(&mut self) -> u32 {
        self.triple().generation()
    }

    /// Generation the compositor last finished reading.
    pub fn reader_done_gen(&mut self) -> u32 {
        self.triple().reader_done_gen()
    }

    /// Read-only view of the most recently published scene nodes.
    pub fn latest_nodes(&self) -> &[scene::Node] {
        let ptr = self.buf.as_ptr();
        let latest = unsafe {
            let ctrl = ptr.add(3 * scene::SCENE_SIZE) as *const u32;
            core::sync::atomic::AtomicU32::from_ptr(ctrl as *mut u32)
                .load(core::sync::atomic::Ordering::Relaxed)
        };
        let off = (latest as usize) * scene::SCENE_SIZE;
        let hdr = unsafe { &*(ptr.add(off) as *const scene::SceneHeader) };
        let count = (hdr.node_count as usize).min(scene::MAX_NODES);
        let node_ptr = unsafe { ptr.add(off + scene::NODES_OFFSET) as *const scene::Node };
        unsafe { core::slice::from_raw_parts(node_ptr, count) }
    }

    /// Read-only view of the most recently published data buffer.
    pub fn latest_data_buf(&self) -> &[u8] {
        let ptr = self.buf.as_ptr();
        let latest = unsafe {
            let ctrl = ptr.add(3 * scene::SCENE_SIZE) as *const u32;
            core::sync::atomic::AtomicU32::from_ptr(ctrl as *mut u32)
                .load(core::sync::atomic::Ordering::Relaxed)
        };
        let off = (latest as usize) * scene::SCENE_SIZE;
        let hdr = unsafe { &*(ptr.add(off) as *const scene::SceneHeader) };
        let used = (hdr.data_used as usize).min(scene::DATA_BUFFER_SIZE);
        let data_start = off + scene::DATA_OFFSET;
        &self.buf[data_start..data_start + used]
    }

    pub fn build_loading(&mut self, fb_width: u32, fb_height: u32) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire();
            build_loading_scene(&mut w, fb_width, fb_height);
        }
        tw.publish();
    }

    pub fn update_spinner(&mut self, angle: f32) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            update_spinner_angle(&mut w, angle);
        }
        tw.publish();
    }

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
        scroll_y: scene::Mpt,
        cursor_opacity: u8,
        slide_offset: scene::Mpt,
        active_space: u8,
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
                cursor_opacity,
                slide_offset,
                active_space,
            );
        }
        tw.publish();
    }

    pub fn apply_slide(&mut self, slide_offset: scene::Mpt) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            let n = w.node_mut(N_STRIP);
            n.child_offset_x = -scene::mpt_to_f32(slide_offset);
            n.child_offset_y = 0.0;
            w.mark_dirty(N_STRIP);
        }
        tw.publish();
    }

    pub fn update_clock(&mut self, cfg: &SceneConfig, clock_text: &[u8]) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_clock_update(&mut w, cfg, clock_text);
        }
        tw.publish();
    }

    pub fn update_cursor(
        &mut self,
        cfg: &SceneConfig,
        cursor_pos: u32,
        doc_text: &[u8],
        chars_per_line: u32,
        clock_text: Option<&[u8]>,
        cursor_opacity: u8,
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
                cursor_opacity,
            );
        }
        tw.publish();
    }

    /// Lightweight cursor opacity update — no scene rebuild, no allocations.
    /// Used for blink fades and selection opacity changes that don't require
    /// repositioning the cursor or rebuilding document content.
    pub fn update_cursor_blink(&mut self, cursor_opacity: u8) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            let n = w.node_mut(N_CURSOR);
            n.opacity = cursor_opacity;
            w.mark_dirty(N_CURSOR);
        }
        tw.publish();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_selection(
        &mut self,
        cfg: &SceneConfig,
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        doc_text: &[u8],
        content_h: u32,
        scroll_pt: i32,
        cursor_opacity: u8,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_selection_update(
                &mut w,
                cfg,
                cursor_pos,
                sel_start,
                sel_end,
                doc_text,
                content_h,
                scroll_pt,
                cursor_opacity,
            );
        }
        tw.publish();
    }

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
        scroll_y: scene::Mpt,
        timer_fired: bool,
        cursor_opacity: u8,
        active_space: u8,
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
                cursor_opacity,
            );
            if !success {
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
                    cursor_opacity,
                    active_space,
                );
            }
        }
        tw.publish();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_document_insert_line(
        &mut self,
        cfg: &SceneConfig,
        doc_text: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        title_label: &[u8],
        clock_text: &[u8],
        scroll_y: scene::Mpt,
        timer_fired: bool,
        cursor_opacity: u8,
        active_space: u8,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            let success = insert_line(
                &mut w,
                cfg,
                doc_text,
                cursor_pos,
                sel_start,
                sel_end,
                scroll_y,
                if timer_fired { Some(clock_text) } else { None },
                cursor_opacity,
            );
            if !success {
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
                    cursor_opacity,
                    active_space,
                );
            }
        }
        tw.publish();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_document_delete_line(
        &mut self,
        cfg: &SceneConfig,
        doc_text: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        title_label: &[u8],
        clock_text: &[u8],
        scroll_y: scene::Mpt,
        timer_fired: bool,
        cursor_opacity: u8,
        active_space: u8,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            let success = delete_line(
                &mut w,
                cfg,
                doc_text,
                cursor_pos,
                sel_start,
                sel_end,
                scroll_y,
                if timer_fired { Some(clock_text) } else { None },
                cursor_opacity,
            );
            if !success {
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
                    cursor_opacity,
                    active_space,
                );
            }
        }
        tw.publish();
    }

    pub fn apply_opacity(&mut self, root_opacity: u8, selection_opacity: u8) {
        if root_opacity == 255 && selection_opacity == 255 {
            return;
        }
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            w.node_mut(N_ROOT).opacity = root_opacity;
            w.mark_dirty(N_ROOT);
            if selection_opacity < 255 {
                let mut sel_id = w.node(N_CURSOR).next_sibling;
                while sel_id != scene::NULL {
                    w.node_mut(sel_id).opacity = selection_opacity;
                    w.mark_dirty(sel_id);
                    sel_id = w.node(sel_id).next_sibling;
                }
            }
        }
        tw.publish();
    }

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
        scroll_y: scene::Mpt,
        mark_clock_changed: bool,
        cursor_opacity: u8,
        active_space: u8,
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
                cursor_opacity,
                active_space,
            );
        }
        tw.publish();
    }

    /// Update document content for a text/rich document.
    /// Reads layout from B's shared memory — no local computation.
    #[allow(clippy::too_many_arguments)]
    pub fn update_rich_document_content(
        &mut self,
        cfg: &SceneConfig,
        pt_buf: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        title_label: &[u8],
        clock_text: &[u8],
        scroll_y: scene::Mpt,
        mark_clock_changed: bool,
        cursor_opacity: u8,
        active_space: u8,
    ) {
        let mut tw = self.triple();
        {
            let mut w = tw.acquire_copy();
            build_rich_document_content(
                &mut w,
                cfg,
                pt_buf,
                cursor_pos,
                sel_start,
                sel_end,
                title_label,
                clock_text,
                scroll_y,
                mark_clock_changed,
                cursor_opacity,
                active_space,
            );
        }
        tw.publish();
    }
}
