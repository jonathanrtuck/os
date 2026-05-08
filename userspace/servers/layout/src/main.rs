//! Layout service — pure function from document content to positioned
//! text runs.
//!
//! Reads the document buffer (RO shared VMO from the document service)
//! and viewport state (seqlock register from the presenter). Computes
//! line breaks and positions using the layout library. Writes results
//! to a seqlock-protected VMO.
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

const HANDLE_NS_EP: Handle = Handle(2);

const PAGE_SIZE: usize = 16384;
const RESULTS_VMO_SIZE: usize = PAGE_SIZE;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE101;
const EXIT_DOC_NOT_FOUND: u32 = 0xE102;
const EXIT_DOC_SETUP: u32 = 0xE103;
const EXIT_DOC_MAP: u32 = 0xE104;
const EXIT_RESULTS_CREATE: u32 = 0xE105;
const EXIT_RESULTS_MAP: u32 = 0xE106;
const EXIT_ENDPOINT_CREATE: u32 = 0xE107;

// ── Font metrics adapter ──────────────────────────────────────────

struct MonoMetrics {
    cw: f32,
    lh: f32,
}

impl layout::FontMetrics for MonoMetrics {
    fn char_width(&self, _ch: char) -> f32 {
        self.cw
    }

    fn line_height(&self) -> f32 {
        self.lh
    }
}

// ── Layout server ─────────────────────────────────────────────────

struct LayoutServer {
    doc_va: usize,
    results_va: usize,
    results_vmo: Handle,

    viewport_va: usize,
    viewport_ready: bool,

    last_doc_gen: u32,
    last_line_count: u32,
    last_total_height: i32,
    last_content_len: u32,
    last_viewport_width: u32,
    last_line_height: u32,

    #[allow(dead_code)]
    console_ep: Handle,
}

impl LayoutServer {
    // ── Document buffer reading ───────────────────────────────────

    fn read_doc_content(&self) -> (usize, u32) {
        loop {
            // SAFETY: doc_va is a valid RO mapping of the document buffer.
            let generation = unsafe {
                let ptr =
                    (self.doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

                (*ptr).load(Ordering::Acquire)
            };
            let content_len =
                unsafe { core::ptr::read_volatile(self.doc_va as *const u64) as usize };
            let generation2 = unsafe {
                let ptr =
                    (self.doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

                (*ptr).load(Ordering::Acquire)
            };

            if generation == generation2 {
                return (content_len, generation);
            }

            core::hint::spin_loop();
        }
    }

    fn doc_content(&self, len: usize) -> &[u8] {
        // SAFETY: doc_va + DOC_HEADER_SIZE..+len is within the mapped VMO.
        unsafe {
            core::slice::from_raw_parts(
                (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                len,
            )
        }
    }

    // ── Viewport state reading ────────────────────────────────────

    fn read_viewport(&self) -> layout_service::ViewportState {
        let mut buf = [0u8; layout_service::ViewportState::SIZE];
        // SAFETY: viewport_va is a valid RO mapping of the viewport
        // state register. Use the seqlock reader protocol.
        let mut reader = unsafe {
            ipc::register::Reader::new(
                self.viewport_va as *const u8,
                layout_service::ViewportState::SIZE,
            )
        };

        reader.read(&mut buf);

        layout_service::ViewportState::read_from(&buf)
    }

    // ── Layout computation ────────────────────────────────────────

    fn recompute(&mut self) {
        if !self.viewport_ready {
            return;
        }

        let (content_len, doc_gen) = self.read_doc_content();
        let viewport = self.read_viewport();
        let cw = viewport.char_width();
        let lh = viewport.line_height as f32;

        if cw <= 0.0 || lh <= 0.0 || viewport.viewport_width == 0 {
            return;
        }

        let metrics = MonoMetrics { cw, lh };
        let max_width = viewport.viewport_width as f32;
        let content = self.doc_content(content_len);
        let result = layout::layout_paragraph(
            content,
            &metrics,
            max_width,
            layout::Alignment::Left,
            &layout::WordBreaker,
        );
        let line_count = result.lines.len().min(layout_service::MAX_LINES);

        self.write_results(
            &result.lines[..line_count],
            result.total_height,
            content_len,
        );

        self.last_doc_gen = doc_gen;
        self.last_line_count = line_count as u32;
        self.last_total_height = result.total_height;
        self.last_content_len = content_len as u32;
        self.last_viewport_width = viewport.viewport_width;
        self.last_line_height = viewport.line_height;
    }

    // ── Results writing (via ipc::register::Writer) ──────────────

    fn write_results(
        &mut self,
        lines: &[layout::LayoutLine],
        total_height: i32,
        content_len: usize,
    ) {
        let mut buf = [0u8; layout_service::RESULTS_VALUE_SIZE];
        let header = layout_service::LayoutHeader {
            line_count: lines.len() as u32,
            total_height,
            content_len: content_len as u32,
            _reserved: 0,
        };

        header.write_to(&mut buf);

        for (i, line) in lines.iter().enumerate() {
            let off = layout_service::LayoutHeader::SIZE + i * layout_service::LineInfo::SIZE;
            let info = layout_service::LineInfo {
                byte_offset: line.byte_offset,
                byte_length: line.byte_length,
                x: line.x,
                y: line.y,
                width: line.width,
            };

            info.write_to(&mut buf[off..]);
        }

        // SAFETY: results_va is a valid RW mapping, 8-byte aligned,
        // of at least RESULTS_VMO_SIZE bytes. This service is the
        // sole writer.
        let mut writer = unsafe {
            ipc::register::Writer::new(
                self.results_va as *mut u8,
                layout_service::RESULTS_VALUE_SIZE,
            )
        };

        writer.write(&buf);
    }
}

// ── Dispatch ──────────────────────────────────────────────────────

impl Dispatch for LayoutServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            layout_service::SETUP => {
                if msg.handles.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let viewport_vmo = Handle(msg.handles[0]);
                let ro = Rights(Rights::READ.0 | Rights::MAP.0);

                match abi::vmo::map(viewport_vmo, 0, ro) {
                    Ok(va) => {
                        self.viewport_va = va;
                        self.viewport_ready = true;
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);

                        return;
                    }
                }

                let ro = Rights(Rights::READ.0 | Rights::MAP.0);

                match abi::handle::dup(self.results_vmo, ro) {
                    Ok(dup) => {
                        let reply = layout_service::SetupReply {
                            max_lines: layout_service::MAX_LINES as u32,
                        };
                        let mut data = [0u8; layout_service::SetupReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[dup.0]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }
            layout_service::RECOMPUTE => {
                self.recompute();

                let reply = layout_service::InfoReply {
                    line_count: self.last_line_count,
                    total_height: self.last_total_height,
                    content_len: self.last_content_len,
                    viewport_width: self.last_viewport_width,
                    line_height: self.last_line_height,
                };
                let mut data = [0u8; layout_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            layout_service::GET_INFO => {
                let reply = layout_service::InfoReply {
                    line_count: self.last_line_count,
                    total_height: self.last_total_height,
                    content_len: self.last_content_len,
                    viewport_width: self.last_viewport_width,
                    line_height: self.last_line_height,
                };
                let mut data = [0u8; layout_service::InfoReply::SIZE];

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

    console::write(console_ep, b"layout: starting\n");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"layout: document not found\n");

            abi::thread::exit(EXIT_DOC_NOT_FOUND);
        }
    };

    console::write(console_ep, b"layout: document found\n");

    // SETUP — get the document buffer VMO.
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
            Err(_) => {
                console::write(console_ep, b"layout: doc setup failed\n");

                abi::thread::exit(EXIT_DOC_SETUP);
            }
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

    console::write(console_ep, b"layout: doc buffer mapped\n");

    // Create layout results VMO.
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let results_vmo = match abi::vmo::create(RESULTS_VMO_SIZE, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_RESULTS_CREATE),
    };
    let results_va = match abi::vmo::map(results_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_RESULTS_MAP),
    };

    // Initialize seqlock generation to 0.
    ipc::register::init(results_va as *mut u8);

    // Register with name service.
    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_ENDPOINT_CREATE),
    };

    name::register(HANDLE_NS_EP, b"layout", own_ep);

    console::write(console_ep, b"layout: ready\n");

    let mut server = LayoutServer {
        doc_va,
        results_va,
        results_vmo,
        viewport_va: 0,
        viewport_ready: false,
        last_doc_gen: 0,
        last_line_count: 0,
        last_total_height: 0,
        last_content_len: 0,
        last_viewport_width: 0,
        last_line_height: 0,
        console_ep,
    };

    ipc::server::serve(own_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
