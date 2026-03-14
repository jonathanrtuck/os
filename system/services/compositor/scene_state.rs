//! Mutable scene graph state: node array + data buffer.
//!
//! The compositor builds this at startup and mutates it in response to
//! events (text changes, cursor moves, scroll, timer ticks). Each frame,
//! the scene renderer reads it and draws to the framebuffer.

use scene::{Border, Color, Content, DataRef, Node, NodeFlags, NodeId, NULL};

pub const DATA_BUF_SIZE: usize = 32768;
pub const MAX_NODES: usize = 64;
/// Well-known node indices for direct mutation.
pub const N_ROOT: usize = 0;
pub const N_TITLE_BAR: usize = 1;
pub const N_TITLE_TEXT: usize = 2;
pub const N_CLOCK_TEXT: usize = 3;
pub const N_SHADOW: usize = 4;
pub const N_CONTENT: usize = 5;
pub const N_DOC_TEXT: usize = 6;
pub const N_CURSOR_ICON: usize = 7;

pub struct SceneState {
    pub nodes: [Node; MAX_NODES],
    pub data: [u8; DATA_BUF_SIZE],
    pub data_used: u32,
    pub node_count: usize,
}

impl SceneState {
    pub fn new() -> Self {
        Self {
            nodes: [Node::EMPTY; MAX_NODES],
            data: [0u8; DATA_BUF_SIZE],
            data_used: 0,
            node_count: 0,
        }
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
        self.data_used = 0;
        self.node_count = 0;

        let title_ref = self.push_data(title_label);
        let clock_ref = self.push_data(clock_text);
        let doc_ref = self.push_data(doc_text);
        let dc = |c: drawing::Color| -> Color { Color::rgba(c.r, c.g, c.b, c.a) };

        // N_ROOT: full-screen background
        self.nodes[N_ROOT] = Node {
            first_child: N_TITLE_BAR as NodeId,
            next_sibling: NULL,
            x: 0,
            y: 0,
            width: fb_width as u16,
            height: fb_height as u16,
            background: dc(bg_color),
            flags: NodeFlags::VISIBLE,
            ..Node::EMPTY
        };
        // N_TITLE_BAR: translucent chrome overlay
        self.nodes[N_TITLE_BAR] = Node {
            first_child: N_TITLE_TEXT as NodeId,
            next_sibling: N_SHADOW as NodeId,
            x: 0,
            y: 0,
            width: fb_width as u16,
            height: title_bar_h as u16,
            background: dc(chrome_bg),
            border: Border {
                color: dc(chrome_border),
                width: 1,
                _pad: [0; 3],
            },
            flags: NodeFlags::VISIBLE,
            ..Node::EMPTY
        };

        // N_TITLE_TEXT: "Text" label in title bar (vertically centered)
        let text_y_offset = (title_bar_h.saturating_sub(line_height)) / 2;

        self.nodes[N_TITLE_TEXT] = Node {
            first_child: NULL,
            next_sibling: N_CLOCK_TEXT as NodeId,
            x: 12,
            y: text_y_offset as i16,
            width: (fb_width / 2) as u16,
            height: line_height as u16,
            content: Content::Text {
                data: title_ref,
                font_size,
                color: dc(chrome_title_color),
                cursor: u32::MAX,
                sel_start: 0,
                sel_end: 0,
            },
            flags: NodeFlags::VISIBLE,
            ..Node::EMPTY
        };

        // N_CLOCK_TEXT: clock on right side of title bar
        // Position will be updated each frame when clock text changes.
        let clock_x = (fb_width - 12 - 80) as i16;

        self.nodes[N_CLOCK_TEXT] = Node {
            first_child: NULL,
            next_sibling: NULL,
            x: clock_x,
            y: text_y_offset as i16,
            width: 80,
            height: line_height as u16,
            content: Content::Text {
                data: clock_ref,
                font_size,
                color: dc(chrome_clock_color),
                cursor: u32::MAX,
                sel_start: 0,
                sel_end: 0,
            },
            flags: NodeFlags::VISIBLE,
            ..Node::EMPTY
        };
        // N_SHADOW: drop shadow below title bar
        // Rendered as a semi-transparent gradient. For now, a simple
        // translucent fill (the gradient will come later).
        self.nodes[N_SHADOW] = Node {
            first_child: NULL,
            next_sibling: N_CONTENT as NodeId,
            x: 0,
            y: title_bar_h as i16,
            width: fb_width as u16,
            height: shadow_depth as u16,
            background: Color::rgba(0, 0, 0, 40),
            flags: NodeFlags::VISIBLE,
            ..Node::EMPTY
        };

        // N_CONTENT: document content area (clips children for scrolling)
        let content_y = title_bar_h + shadow_depth;
        let content_h = fb_height.saturating_sub(content_y);

        self.nodes[N_CONTENT] = Node {
            first_child: N_DOC_TEXT as NodeId,
            next_sibling: N_CURSOR_ICON as NodeId,
            x: 0,
            y: content_y as i16,
            width: fb_width as u16,
            height: content_h as u16,
            flags: NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN,
            ..Node::EMPTY
        };
        // N_DOC_TEXT: document text content
        // Height is u16::MAX so that max_y never clips text rendering.
        // The parent N_CONTENT node clips children to the viewport.
        self.nodes[N_DOC_TEXT] = Node {
            first_child: NULL,
            next_sibling: NULL,
            x: text_inset_x as i16,
            y: 8,
            width: (fb_width - 2 * text_inset_x) as u16,
            height: u16::MAX,
            content: Content::Text {
                data: doc_ref,
                font_size,
                color: dc(text_color),
                cursor: cursor_pos,
                sel_start,
                sel_end,
            },
            flags: NodeFlags::VISIBLE,
            ..Node::EMPTY
        };
        // N_CURSOR_ICON: mouse cursor (hidden until pointer event)
        self.nodes[N_CURSOR_ICON] = Node {
            first_child: NULL,
            next_sibling: NULL,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            flags: NodeFlags::empty(), // invisible by default
            ..Node::EMPTY
        };
        self.node_count = 8;
    }
    /// Append bytes to the data buffer. Returns a DataRef.
    /// If the buffer is full, truncates to fit and returns a shorter DataRef.
    pub fn push_data(&mut self, bytes: &[u8]) -> DataRef {
        let off = self.data_used;
        let avail = DATA_BUF_SIZE.saturating_sub(off as usize);
        let actual = if bytes.len() < avail {
            bytes.len()
        } else {
            avail
        };

        if actual > 0 {
            self.data[off as usize..off as usize + actual].copy_from_slice(&bytes[..actual]);

            self.data_used = off + actual as u32;
        }

        DataRef {
            offset: off,
            length: actual as u32,
        }
    }
    /// Re-allocate a data region with new content (may be different length).
    /// Returns a new DataRef. Old data is abandoned (simple bump allocator).
    pub fn replace_data(&mut self, bytes: &[u8]) -> DataRef {
        self.push_data(bytes)
    }
    /// Reset the data buffer (call when rebuilding the scene).
    pub fn reset_data(&mut self) {
        self.data_used = 0;
    }
    /// Update the clock text.
    pub fn update_clock(&mut self, clock_text: &[u8]) {
        let clock_ref = self.replace_data(clock_text);

        self.nodes[N_CLOCK_TEXT].content = match self.nodes[N_CLOCK_TEXT].content {
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
        self.nodes[N_DOC_TEXT].content = match self.nodes[N_DOC_TEXT].content {
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
    /// Overwrite the data for an existing DataRef (same length).
    pub fn update_data(&mut self, dref: DataRef, bytes: &[u8]) {
        let off = dref.offset as usize;
        let len = dref.length as usize;

        if bytes.len() == len && off + len <= DATA_BUF_SIZE {
            self.data[off..off + len].copy_from_slice(bytes);
        }
    }
    /// Update the document text content and cursor.
    pub fn update_doc_text(&mut self, doc_text: &[u8], cursor_pos: u32, sel_s: u32, sel_e: u32) {
        let doc_ref = self.replace_data(doc_text);

        self.nodes[N_DOC_TEXT].content = match self.nodes[N_DOC_TEXT].content {
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
        self.nodes[N_CONTENT].scroll_y = scroll_y;
    }
}
