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

use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicU32, Ordering},
};

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};
use scene::{Color, Content, NodeFlags, SCENE_SIZE, SceneWriter, ShapedGlyph, pt, upt};

const HANDLE_NS_EP: Handle = Handle(2);

const PAGE_SIZE: usize = 16384;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE201;
const EXIT_DOC_NOT_FOUND: u32 = 0xE202;
const EXIT_DOC_SETUP: u32 = 0xE203;
const EXIT_DOC_MAP: u32 = 0xE204;
const EXIT_LAYOUT_NOT_FOUND: u32 = 0xE205;
const EXIT_VIEWPORT_CREATE: u32 = 0xE206;
const EXIT_VIEWPORT_MAP: u32 = 0xE207;
const EXIT_LAYOUT_SETUP: u32 = 0xE208;
const EXIT_RESULTS_MAP: u32 = 0xE209;
const EXIT_SCENE_CREATE: u32 = 0xE20A;
const EXIT_SCENE_MAP: u32 = 0xE20B;
const EXIT_ENDPOINT_CREATE: u32 = 0xE20C;

const MAX_GLYPHS_PER_LINE: usize = 256;

// ── Free helpers (avoid borrow conflicts with scene_buf) ──────────

fn read_line_at(results_va: usize, index: usize) -> layout_service::LineInfo {
    let offset = ipc::register::HEADER_SIZE
        + layout_service::LayoutHeader::SIZE
        + index * layout_service::LineInfo::SIZE;
    let mut buf = [0u8; layout_service::LineInfo::SIZE];

    // SAFETY: results_va + offset is within the layout results VMO.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (results_va + offset) as *const u8,
            buf.as_mut_ptr(),
            layout_service::LineInfo::SIZE,
        );
    }

    layout_service::LineInfo::read_from(&buf)
}

fn read_layout_header(results_va: usize) -> layout_service::LayoutHeader {
    let mut buf = [0u8; layout_service::LayoutHeader::SIZE];

    unsafe {
        core::ptr::copy_nonoverlapping(
            (results_va + ipc::register::HEADER_SIZE) as *const u8,
            buf.as_mut_ptr(),
            layout_service::LayoutHeader::SIZE,
        );
    }

    layout_service::LayoutHeader::read_from(&buf)
}

fn read_doc_header(doc_va: usize) -> (usize, usize) {
    loop {
        let generation = unsafe {
            let ptr = (doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

            (*ptr).load(Ordering::Acquire)
        };
        let content_len = unsafe { core::ptr::read_volatile(doc_va as *const u64) as usize };
        let cursor_pos = unsafe { core::ptr::read_volatile((doc_va + 8) as *const u64) as usize };
        let generation2 = unsafe {
            let ptr = (doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

            (*ptr).load(Ordering::Acquire)
        };

        if generation == generation2 {
            return (content_len, cursor_pos);
        }

        core::hint::spin_loop();
    }
}

fn doc_content_at(doc_va: usize, len: usize) -> &'static [u8] {
    unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            len,
        )
    }
}

// ── Presenter server ──────────────────────────────────────────────

struct Presenter {
    doc_va: usize,
    layout_ep: Handle,
    results_va: usize,

    scene_buf: &'static mut [u8],
    scene_vmo: Handle,

    viewport_va: usize,

    display_width: u32,
    display_height: u32,

    last_line_count: u32,
    last_cursor_line: u32,
    last_cursor_col: u32,
    last_content_len: u32,

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
            char_width_fp: layout_service::ViewportState::encode_char_width(
                presenter_service::CHAR_WIDTH_F32,
            ),
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
        let doc_va = self.doc_va;
        let results_va = self.results_va;
        let (content_len, cursor_pos) = read_doc_header(doc_va);
        let layout_header = read_layout_header(results_va);
        let line_count = layout_header.line_count as usize;
        let content = doc_content_at(doc_va, content_len);
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
        let char_advance = (presenter_service::CHAR_WIDTH_F32 * 65536.0) as i32;

        for i in 0..line_count.min(scene::MAX_NODES - 4) {
            let line_info = read_line_at(results_va, i);
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
            let mut glyphs = [ShapedGlyph {
                glyph_id: 0,
                _pad: 0,
                x_advance: 0,
                x_offset: 0,
                y_offset: 0,
            }; MAX_GLYPHS_PER_LINE];

            for (j, &byte) in line_bytes.iter().enumerate().take(glyph_count) {
                glyphs[j] = ShapedGlyph {
                    glyph_id: byte as u16,
                    _pad: 0,
                    x_advance: char_advance,
                    x_offset: 0,
                    y_offset: 0,
                };
            }

            let glyph_ref = scene.push_shaped_glyphs(&glyphs[..glyph_count]);
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
            let last = read_line_at(results_va, line_count - 1);
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
            presenter_service::SETUP => {
                let ro = Rights(Rights::READ.0 | Rights::MAP.0);

                match abi::handle::dup(self.scene_vmo, ro) {
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
                }
            }
            presenter_service::BUILD => {
                self.build_scene();

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
    let doc_va = {
        let mut buf = [0u8; ipc::message::MSG_SIZE];

        ipc::message::write_request(&mut buf, document_service::SETUP, &[]);

        let mut recv_handles = [0u32; 4];
        let result = match abi::ipc::call(
            doc_ep,
            &mut buf,
            ipc::message::HEADER_SIZE,
            &[],
            &mut recv_handles,
        ) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_DOC_SETUP),
        };
        let header = ipc::message::Header::read_from(&buf);

        if header.is_error() || result.handle_count == 0 {
            abi::thread::exit(EXIT_DOC_SETUP);
        }

        let vmo = Handle(recv_handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);

        match abi::vmo::map(vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_DOC_MAP),
        }
    };

    console::write(console_ep, b"presenter: doc connected\n");

    let layout_ep = match name::watch(HANDLE_NS_EP, b"layout") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"presenter: layout not found\n");

            abi::thread::exit(EXIT_LAYOUT_NOT_FOUND);
        }
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let viewport_vmo_size = ipc::register::required_size(layout_service::ViewportState::SIZE)
        .next_multiple_of(PAGE_SIZE);
    let viewport_vmo = match abi::vmo::create(viewport_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let viewport_va = match abi::vmo::map(viewport_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_MAP),
    };

    ipc::register::init(viewport_va as *mut u8);

    let viewport_dup = match abi::handle::dup(viewport_vmo, Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let results_va = {
        let mut buf = [0u8; ipc::message::MSG_SIZE];

        ipc::message::write_request(&mut buf, layout_service::SETUP, &[]);

        let mut recv_handles = [0u32; 4];
        let result = match abi::ipc::call(
            layout_ep,
            &mut buf,
            ipc::message::HEADER_SIZE,
            &[viewport_dup.0],
            &mut recv_handles,
        ) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_LAYOUT_SETUP),
        };
        let header = ipc::message::Header::read_from(&buf);

        if header.is_error() || result.handle_count == 0 {
            abi::thread::exit(EXIT_LAYOUT_SETUP);
        }

        let vmo = Handle(recv_handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);

        match abi::vmo::map(vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_RESULTS_MAP),
        }
    };

    console::write(console_ep, b"presenter: layout connected\n");

    let scene_vmo_size = SCENE_SIZE.next_multiple_of(PAGE_SIZE);
    let scene_vmo = match abi::vmo::create(scene_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_SCENE_CREATE),
    };
    let scene_va = match abi::vmo::map(scene_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
    };
    // SAFETY: scene_va is a valid RW mapping of at least SCENE_SIZE
    // bytes. The presenter is the sole writer.
    let scene_buf = unsafe { core::slice::from_raw_parts_mut(scene_va as *mut u8, SCENE_SIZE) };
    let _ = SceneWriter::new(scene_buf);
    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_ENDPOINT_CREATE),
    };

    name::register(HANDLE_NS_EP, b"presenter", own_ep);

    console::write(console_ep, b"presenter: ready\n");

    let mut server = Presenter {
        doc_va,
        layout_ep,
        results_va,
        scene_buf,
        scene_vmo,
        viewport_va,
        display_width: presenter_service::DEFAULT_WIDTH,
        display_height: presenter_service::DEFAULT_HEIGHT,
        last_line_count: 0,
        last_cursor_line: 0,
        last_cursor_col: 0,
        last_content_len: 0,
        console_ep,
    };

    server.write_viewport();

    ipc::server::serve(own_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
