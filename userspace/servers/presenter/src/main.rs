//! Presenter — compiles document state + layout into a scene graph.
//!
//! The OS Service from architecture.md. Reads the document buffer (RO),
//! writes viewport state for the layout service, reads layout results,
//! and builds a scene graph tree (root → viewport → line glyphs + cursor).
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint

#![no_std]
#![no_main]

extern crate alloc;
extern crate heap;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};
use scene::{Color, Content, NodeFlags, SCENE_SIZE, SceneWriter, ShapedGlyph, pt, upt};

const HANDLE_NS_EP: Handle = Handle(2);

const PAGE_SIZE: usize = 16384;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE201;
const EXIT_DOC_NOT_FOUND: u32 = 0xE202;
const EXIT_DOC_SETUP: u32 = 0xE203;
const EXIT_LAYOUT_NOT_FOUND: u32 = 0xE205;
const EXIT_VIEWPORT_CREATE: u32 = 0xE206;
const EXIT_VIEWPORT_MAP: u32 = 0xE207;
const EXIT_LAYOUT_SETUP: u32 = 0xE208;
const EXIT_SCENE_CREATE: u32 = 0xE20A;
const EXIT_SCENE_MAP: u32 = 0xE20B;
const EXIT_ENDPOINT_CREATE: u32 = 0xE20C;
const EXIT_RENDER_NOT_FOUND: u32 = 0xE20D;
const EXIT_RENDER_SETUP: u32 = 0xE20E;
const EXIT_EDITOR_NOT_FOUND: u32 = 0xE20F;

const MAX_GLYPHS_PER_LINE: usize = 256;

static FONT_DATA: &[u8] = include_bytes!("../../../../assets/jetbrains-mono.ttf");

fn build_cmap_table() -> [u16; 128] {
    let mut table = [0u16; 128];

    for ch in 0u8..128 {
        table[ch as usize] = fonts::metrics::glyph_id_for_char(FONT_DATA, ch as char).unwrap_or(0);
    }

    table
}

fn compute_char_advance() -> f32 {
    let gid = fonts::metrics::glyph_id_for_char(FONT_DATA, 'M').unwrap_or(0);

    if let (Some((advance_fu, _)), Some(fm)) = (
        fonts::metrics::glyph_h_metrics(FONT_DATA, gid),
        fonts::metrics::font_metrics(FONT_DATA),
    ) {
        return advance_fu as f32 * presenter_service::FONT_SIZE as f32 / fm.units_per_em as f32;
    }

    presenter_service::CHAR_WIDTH_F32
}

// ── Layout results parsing (from seqlock-read buffer) ────────────

fn parse_layout_header(buf: &[u8]) -> layout_service::LayoutHeader {
    layout_service::LayoutHeader::read_from(buf)
}

fn parse_line_at(buf: &[u8], index: usize) -> layout_service::LineInfo {
    let offset = layout_service::LayoutHeader::SIZE + index * layout_service::LineInfo::SIZE;

    layout_service::LineInfo::read_from(&buf[offset..])
}

// ── Presenter server ──────────────────────────────────────────────

struct Presenter {
    doc_va: usize,
    layout_ep: Handle,

    results_reader: ipc::register::Reader,
    results_buf: [u8; layout_service::RESULTS_VALUE_SIZE],

    scene_buf: &'static mut [u8],
    scene_vmo: Handle,

    viewport_va: usize,

    display_width: u32,
    display_height: u32,

    glyphs: [ShapedGlyph; MAX_GLYPHS_PER_LINE],
    cmap: [u16; 128],
    char_width: f32,

    last_line_count: u32,
    last_cursor_line: u32,
    last_cursor_col: u32,
    last_content_len: u32,

    render_ep: Handle,
    editor_ep: Handle,

    #[allow(dead_code)]
    console_ep: Handle,
}

impl Presenter {
    fn write_viewport(&self) {
        let state = layout_service::ViewportState {
            scroll_y: 0,
            viewport_width: self
                .display_width
                .saturating_sub(presenter_service::MARGIN_LEFT as u32 * 2),
            viewport_height: self
                .display_height
                .saturating_sub(presenter_service::MARGIN_TOP as u32 * 2),
            char_width_fp: layout_service::ViewportState::encode_char_width(self.char_width),
            line_height: presenter_service::LINE_HEIGHT,
        };
        let mut buf = [0u8; layout_service::ViewportState::SIZE];

        state.write_to(&mut buf);

        let mut writer = unsafe {
            ipc::register::Writer::new(
                self.viewport_va as *mut u8,
                layout_service::ViewportState::SIZE,
            )
        };

        writer.write(&buf);
    }

    fn build_scene(&mut self) {
        let _ = ipc::client::call_simple(self.layout_ep, layout_service::RECOMPUTE, &[]);
        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, cursor_pos, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };

        self.results_reader.read(&mut self.results_buf);

        let layout_header = parse_layout_header(&self.results_buf);
        let line_count = layout_header.line_count as usize;
        // SAFETY: doc_va is valid and content_len comes from read_doc_header.
        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };
        let bg = Color::rgb(
            presenter_service::BG_R,
            presenter_service::BG_G,
            presenter_service::BG_B,
        );
        let text_color = Color::rgb(
            presenter_service::TEXT_R,
            presenter_service::TEXT_G,
            presenter_service::TEXT_B,
        );
        let cursor_color = Color::rgb(
            presenter_service::CURSOR_R,
            presenter_service::CURSOR_G,
            presenter_service::CURSOR_B,
        );
        let mut scene = SceneWriter::from_existing(self.scene_buf);

        scene.clear();

        // Root node — full screen background.
        let root = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(root);

            n.width = upt(self.display_width);
            n.height = upt(self.display_height);
            n.background = bg;
        }

        scene.set_root(root);

        // Viewport node — clips children, offset for margins.
        let viewport = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(viewport);

            n.x = pt(presenter_service::MARGIN_LEFT);
            n.y = pt(presenter_service::MARGIN_TOP);
            n.width = upt(self
                .display_width
                .saturating_sub(presenter_service::MARGIN_LEFT as u32 * 2));
            n.height = upt(self
                .display_height
                .saturating_sub(presenter_service::MARGIN_TOP as u32 * 2));
            n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
            n.role = scene::ROLE_DOCUMENT;
        }

        scene.add_child(root, viewport);

        // Per-line glyph nodes.
        let mut cursor_line: u32 = 0;
        let mut cursor_col: u32 = 0;
        let char_advance = (self.char_width * 65536.0) as i32;

        for i in 0..line_count.min(scene::MAX_NODES - 4) {
            let line_info = parse_line_at(&self.results_buf, i);
            let line_start = line_info.byte_offset as usize;
            let line_len = line_info.byte_length as usize;

            if cursor_pos >= line_start && cursor_pos <= line_start + line_len {
                cursor_line = i as u32;
                cursor_col = (cursor_pos - line_start) as u32;
            }

            if line_len == 0 {
                continue;
            }

            let line_bytes = if line_start + line_len <= content_len {
                &content[line_start..line_start + line_len]
            } else {
                continue;
            };
            let glyph_count = line_len.min(MAX_GLYPHS_PER_LINE);

            for (j, &byte) in line_bytes.iter().enumerate().take(glyph_count) {
                let gid = if byte < 128 {
                    self.cmap[byte as usize]
                } else {
                    0
                };

                self.glyphs[j] = ShapedGlyph {
                    glyph_id: gid,
                    _pad: 0,
                    x_advance: char_advance,
                    x_offset: 0,
                    y_offset: 0,
                };
            }

            let glyph_ref = scene.push_shaped_glyphs(&self.glyphs[..glyph_count]);
            let line_node = match scene.alloc_node() {
                Some(id) => id,
                None => break,
            };

            {
                let n = scene.node_mut(line_node);

                n.x = scene::f32_to_mpt(line_info.x);
                n.y = pt(line_info.y);
                n.width = upt(line_info.width as u32 + 1);
                n.height = upt(presenter_service::LINE_HEIGHT);
                n.content = Content::Glyphs {
                    color: text_color,
                    glyphs: glyph_ref,
                    glyph_count: glyph_count as u16,
                    font_size: presenter_service::FONT_SIZE,
                    style_id: 0,
                };
                n.role = scene::ROLE_PARAGRAPH;
            }

            scene.add_child(viewport, line_node);
        }

        // Handle cursor past last line.
        if line_count > 0 && cursor_pos >= content_len {
            let last = parse_line_at(&self.results_buf, line_count - 1);
            let last_end = last.byte_offset as usize + last.byte_length as usize;

            if cursor_pos >= last_end && cursor_pos > last.byte_offset as usize {
                cursor_line = (line_count - 1) as u32;
                cursor_col = (cursor_pos - last.byte_offset as usize) as u32;
            }
        }

        // Cursor node.
        let cursor_x = cursor_col as f32 * presenter_service::CHAR_WIDTH_F32;
        let cursor_y = cursor_line as i32 * presenter_service::LINE_HEIGHT as i32;

        if let Some(cursor_node) = scene.alloc_node() {
            let n = scene.node_mut(cursor_node);

            n.x = scene::f32_to_mpt(cursor_x);
            n.y = pt(cursor_y);
            n.width = upt(presenter_service::CURSOR_WIDTH);
            n.height = upt(presenter_service::LINE_HEIGHT);
            n.background = cursor_color;
            n.role = scene::ROLE_CARET;

            scene.add_child(viewport, cursor_node);
        }

        scene.commit();

        self.last_line_count = line_count as u32;
        self.last_cursor_line = cursor_line;
        self.last_cursor_col = cursor_col;
        self.last_content_len = content_len as u32;
    }

    fn request_render(&self) {
        let _ = ipc::client::call_simple(self.render_ep, render::comp::RENDER, &[]);
    }

    fn make_info_reply(&self) -> presenter_service::InfoReply {
        let scene = SceneWriter::from_existing(unsafe {
            core::slice::from_raw_parts_mut(self.scene_buf.as_ptr() as *mut u8, SCENE_SIZE)
        });

        presenter_service::InfoReply {
            node_count: scene.node_count(),
            generation: scene.generation(),
            line_count: self.last_line_count,
            cursor_line: self.last_cursor_line,
            cursor_col: self.last_cursor_col,
            content_len: self.last_content_len,
        }
    }
}

// ── Dispatch ──────────────────────────────────────────────────────

impl Dispatch for Presenter {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            presenter_service::SETUP => match abi::handle::dup(self.scene_vmo, Rights::READ_MAP) {
                Ok(dup) => {
                    let reply = presenter_service::SetupReply {
                        display_width: self.display_width,
                        display_height: self.display_height,
                    };
                    let mut data = [0u8; presenter_service::SetupReply::SIZE];

                    reply.write_to(&mut data);

                    let _ = msg.reply_ok(&data, &[dup.0]);
                }
                Err(_) => {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            },
            presenter_service::BUILD => {
                self.build_scene();
                self.request_render();

                let reply = self.make_info_reply();
                let mut data = [0u8; presenter_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            presenter_service::GET_INFO => {
                let reply = self.make_info_reply();
                let mut data = [0u8; presenter_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }

            presenter_service::KEY_EVENT => {
                if msg.payload.len() >= text_editor::KeyDispatch::SIZE {
                    let dispatch = text_editor::KeyDispatch::read_from(msg.payload);
                    let mut data = [0u8; text_editor::KeyDispatch::SIZE];

                    dispatch.write_to(&mut data);

                    let _ =
                        ipc::client::call_simple(self.editor_ep, text_editor::DISPATCH_KEY, &data);

                    self.build_scene();
                    self.request_render();
                }

                let _ = msg.reply_empty();
            }

            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_NOT_FOUND),
    };

    console::write(console_ep, b"presenter: starting\n");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: doc not found\n");

            abi::thread::exit(EXIT_DOC_NOT_FOUND);
        }
    };
    let doc_va =
        match ipc::client::setup_map_vmo(doc_ep, document_service::SETUP, &[], Rights::READ_MAP) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_DOC_SETUP),
        };

    console::write(console_ep, b"presenter: doc connected\n");

    let layout_ep = match name::watch(HANDLE_NS_EP, b"layout") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: layout not found\n");

            abi::thread::exit(EXIT_LAYOUT_NOT_FOUND);
        }
    };
    let viewport_vmo_size = ipc::register::required_size(layout_service::ViewportState::SIZE)
        .next_multiple_of(PAGE_SIZE);
    let viewport_vmo = match abi::vmo::create(viewport_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let viewport_va = match abi::vmo::map(viewport_vmo, 0, Rights::READ_WRITE_MAP) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_MAP),
    };

    ipc::register::init(viewport_va as *mut u8);

    let viewport_dup = match abi::handle::dup(viewport_vmo, Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let results_va = match ipc::client::setup_map_vmo(
        layout_ep,
        layout_service::SETUP,
        &[viewport_dup.0],
        Rights::READ_MAP,
    ) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_LAYOUT_SETUP),
    };

    console::write(console_ep, b"presenter: layout connected\n");

    // SAFETY: results_va is a valid RO mapping of the layout results
    // VMO, 8-byte aligned, at least RESULTS_VALUE_SIZE + HEADER_SIZE
    // bytes. The layout service is the sole writer.
    let results_reader = unsafe {
        ipc::register::Reader::new(results_va as *const u8, layout_service::RESULTS_VALUE_SIZE)
    };
    let scene_vmo_size = SCENE_SIZE.next_multiple_of(PAGE_SIZE);
    let scene_vmo = match abi::vmo::create(scene_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_SCENE_CREATE),
    };
    let scene_va = match abi::vmo::map(scene_vmo, 0, Rights::READ_WRITE_MAP) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
    };
    // SAFETY: scene_va is a valid RW mapping of at least SCENE_SIZE
    // bytes. The presenter is the sole writer.
    let scene_buf = unsafe { core::slice::from_raw_parts_mut(scene_va as *mut u8, SCENE_SIZE) };
    let _ = SceneWriter::new(scene_buf);

    // Connect to compositor — send scene graph VMO so it can read our output.
    let render_ep = match name::watch(HANDLE_NS_EP, b"render") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: render not found\n");

            abi::thread::exit(EXIT_RENDER_NOT_FOUND);
        }
    };
    let scene_dup = match abi::handle::dup(scene_vmo, Rights::READ_MAP) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RENDER_SETUP),
    };
    let mut reply_buf = [0u8; ipc::message::MSG_SIZE];
    let mut recv_handles = [0u32; 4];
    let (display_width, display_height) = match ipc::client::call(
        render_ep,
        render::comp::SETUP,
        &[],
        &[scene_dup.0],
        &mut recv_handles,
        &mut reply_buf,
    ) {
        Ok(reply) if !reply.is_error() && reply.payload.len() >= render::comp::SetupReply::SIZE => {
            let sr = render::comp::SetupReply::read_from(reply.payload);

            (sr.display_width, sr.display_height)
        }
        _ => (
            presenter_service::DEFAULT_WIDTH,
            presenter_service::DEFAULT_HEIGHT,
        ),
    };

    console::write(console_ep, b"presenter: render connected\n");

    let editor_ep = match name::watch(HANDLE_NS_EP, b"editor.text") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: editor not found\n");

            abi::thread::exit(EXIT_EDITOR_NOT_FOUND);
        }
    };

    console::write(console_ep, b"presenter: editor connected\n");

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_ENDPOINT_CREATE),
    };

    name::register(HANDLE_NS_EP, b"presenter", own_ep);

    let mut server = Presenter {
        doc_va,
        layout_ep,
        results_reader,
        results_buf: [0u8; layout_service::RESULTS_VALUE_SIZE],
        scene_buf,
        scene_vmo,
        viewport_va,
        display_width,
        display_height,
        glyphs: [ShapedGlyph {
            glyph_id: 0,
            _pad: 0,
            x_advance: 0,
            x_offset: 0,
            y_offset: 0,
        }; MAX_GLYPHS_PER_LINE],
        cmap: build_cmap_table(),
        char_width: compute_char_advance(),
        last_line_count: 0,
        last_cursor_line: 0,
        last_cursor_col: 0,
        last_content_len: 0,
        render_ep,
        editor_ep,
        console_ep,
    };

    // Initial render: write viewport, build scene graph, tell compositor.
    server.write_viewport();
    server.build_scene();
    server.request_render();

    console::write(console_ep, b"presenter: ready\n");

    ipc::server::serve(own_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
