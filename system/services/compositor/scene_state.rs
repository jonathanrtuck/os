//! Mutable scene graph state backed by the shared memory layout.
//!
//! Wraps a `SceneWriter` operating on a heap-allocated buffer. Provides
//! the same well-known node indices and build/update API that the
//! compositor event loop uses. When the compositor splits into OS service
//! + compositor processes, the buffer becomes shared memory.

use alloc::vec;
use scene::{Border, Color, Content, DataRef, Node, NodeFlags, SceneWriter, NULL, SCENE_SIZE};

/// Well-known node indices for direct mutation.
pub const N_ROOT: u16 = 0;
pub const N_TITLE_BAR: u16 = 1;
pub const N_TITLE_TEXT: u16 = 2;
pub const N_CLOCK_TEXT: u16 = 3;
pub const N_SHADOW: u16 = 4;
pub const N_CONTENT: u16 = 5;
pub const N_DOC_TEXT: u16 = 6;
pub const N_CURSOR_ICON: u16 = 7;

pub struct SceneState {
    buf: alloc::vec::Vec<u8>,
}

impl SceneState {
    pub fn new() -> Self {
        let mut buf = vec![0u8; SCENE_SIZE];
        // Initialize the shared memory header.
        let _ = SceneWriter::new(&mut buf);

        Self { buf }
    }

    fn writer(&mut self) -> SceneWriter<'_> {
        SceneWriter::from_existing(&mut self.buf)
    }

    /// Build the initial scene tree for the text editor screen layout.
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
        font_size: u16,
        line_height: u32,
        doc_text: &[u8],
        cursor_pos: u32,
        sel_start: u32,
        sel_end: u32,
        title_label: &[u8],
        clock_text: &[u8],
    ) {
        let mut w = self.writer();

        w.clear();

        let title_ref = w.push_data(title_label);
        let clock_ref = w.push_data(clock_text);
        let doc_ref = w.push_data(doc_text);
        let dc = |c: drawing::Color| -> Color { Color::rgba(c.r, c.g, c.b, c.a) };
        // Allocate all well-known nodes in order (sequential IDs).
        let _root = w.alloc_node().unwrap(); // 0
        let _title_bar = w.alloc_node().unwrap(); // 1
        let _title_text = w.alloc_node().unwrap(); // 2
        let _clock_text = w.alloc_node().unwrap(); // 3
        let _shadow = w.alloc_node().unwrap(); // 4
        let _content = w.alloc_node().unwrap(); // 5
        let _doc_text = w.alloc_node().unwrap(); // 6
        let _cursor_icon = w.alloc_node().unwrap(); // 7

        // N_ROOT: full-screen background
        {
            let n = w.node_mut(N_ROOT);

            n.first_child = N_TITLE_BAR;
            n.width = fb_width as u16;
            n.height = fb_height as u16;
            n.background = dc(bg_color);
            n.flags = NodeFlags::VISIBLE;
        }
        // N_TITLE_BAR: translucent chrome overlay
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

        // N_TITLE_TEXT: label in title bar
        let text_y_offset = (title_bar_h.saturating_sub(line_height)) / 2;

        {
            let n = w.node_mut(N_TITLE_TEXT);

            n.next_sibling = N_CLOCK_TEXT;
            n.x = 12;
            n.y = text_y_offset as i16;
            n.width = (fb_width / 2) as u16;
            n.height = line_height as u16;
            n.content = Content::Text {
                data: title_ref,
                font_size,
                color: dc(chrome_title_color),
                cursor: u32::MAX,
                sel_start: 0,
                sel_end: 0,
            };
            n.flags = NodeFlags::VISIBLE;
        }

        // N_CLOCK_TEXT: clock on right side
        let clock_x = (fb_width - 12 - 80) as i16;

        {
            let n = w.node_mut(N_CLOCK_TEXT);

            n.x = clock_x;
            n.y = text_y_offset as i16;
            n.width = 80;
            n.height = line_height as u16;
            n.content = Content::Text {
                data: clock_ref,
                font_size,
                color: dc(chrome_clock_color),
                cursor: u32::MAX,
                sel_start: 0,
                sel_end: 0,
            };
            n.flags = NodeFlags::VISIBLE;
        }
        // N_SHADOW: drop shadow below title bar
        {
            let n = w.node_mut(N_SHADOW);

            n.next_sibling = N_CONTENT;
            n.y = title_bar_h as i16;
            n.width = fb_width as u16;
            n.height = shadow_depth as u16;
            n.background = Color::rgba(0, 0, 0, 40);
            n.flags = NodeFlags::VISIBLE;
        }

        // N_CONTENT: document content area (clips children for scrolling)
        let content_y = title_bar_h + shadow_depth;
        let content_h = fb_height.saturating_sub(content_y);

        {
            let n = w.node_mut(N_CONTENT);

            n.first_child = N_DOC_TEXT;
            n.next_sibling = N_CURSOR_ICON;
            n.y = content_y as i16;
            n.width = fb_width as u16;
            n.height = content_h as u16;
            n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
        }
        // N_DOC_TEXT: document text content
        {
            let n = w.node_mut(N_DOC_TEXT);

            n.x = text_inset_x as i16;
            n.y = 8;
            n.width = (fb_width - 2 * text_inset_x) as u16;
            n.height = u16::MAX;
            n.content = Content::Text {
                data: doc_ref,
                font_size,
                color: dc(text_color),
                cursor: cursor_pos,
                sel_start,
                sel_end,
            };
            n.flags = NodeFlags::VISIBLE;
        }
        // N_CURSOR_ICON: mouse cursor (hidden until pointer event)
        {
            let n = w.node_mut(N_CURSOR_ICON);

            n.flags = NodeFlags::empty();
        }

        w.set_root(N_ROOT);
        w.commit();
    }
    pub fn data_buf(&self) -> &[u8] {
        let used = {
            let hdr = unsafe { &*(self.buf.as_ptr() as *const scene::SceneHeader) };

            hdr.data_used as usize
        };

        &self.buf[scene::DATA_OFFSET..scene::DATA_OFFSET + used]
    }
    pub fn nodes(&self) -> &[Node] {
        let hdr = unsafe { &*(self.buf.as_ptr() as *const scene::SceneHeader) };
        let count = hdr.node_count as usize;
        let ptr = unsafe { self.buf.as_ptr().add(scene::NODES_OFFSET) as *const Node };

        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    pub fn node_count(&self) -> usize {
        let hdr = unsafe { &*(self.buf.as_ptr() as *const scene::SceneHeader) };

        hdr.node_count as usize
    }
    /// Update the clock text.
    pub fn update_clock(&mut self, clock_text: &[u8]) {
        let mut w = self.writer();
        let clock_ref = w.push_data(clock_text);
        let n = w.node_mut(N_CLOCK_TEXT);

        n.content = match n.content {
            Content::Text {
                font_size,
                color,
                cursor,
                sel_start,
                sel_end,
                ..
            } => Content::Text {
                data: clock_ref,
                font_size,
                color,
                cursor,
                sel_start,
                sel_end,
            },
            other => other,
        };
    }
    /// Update just the cursor position and selection (no text change).
    pub fn update_cursor(&mut self, cursor_pos: u32, sel_s: u32, sel_e: u32) {
        let mut w = self.writer();
        let n = w.node_mut(N_DOC_TEXT);

        n.content = match n.content {
            Content::Text {
                data,
                font_size,
                color,
                ..
            } => Content::Text {
                data,
                font_size,
                color,
                cursor: cursor_pos,
                sel_start: sel_s,
                sel_end: sel_e,
            },
            other => other,
        };
    }
    /// Update the document text content and cursor.
    pub fn update_doc_text(&mut self, doc_text: &[u8], cursor_pos: u32, sel_s: u32, sel_e: u32) {
        let mut w = self.writer();
        let doc_ref = w.push_data(doc_text);
        let n = w.node_mut(N_DOC_TEXT);

        n.content = match n.content {
            Content::Text {
                font_size, color, ..
            } => Content::Text {
                data: doc_ref,
                font_size,
                color,
                cursor: cursor_pos,
                sel_start: sel_s,
                sel_end: sel_e,
            },
            other => other,
        };
    }
    /// Update scroll offset on the content area.
    pub fn update_scroll(&mut self, scroll_y: i32) {
        let mut w = self.writer();

        w.node_mut(N_DOC_TEXT).scroll_y = scroll_y;
    }
}
