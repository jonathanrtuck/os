//! Layout service integration test — exercises the full layout pipeline.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint
//!
//! Sequence:
//!   1. Watch for console, document, layout services
//!   2. Insert multi-line text into the document
//!   3. Create viewport state VMO, write monospace metrics
//!   4. Call layout SETUP with viewport VMO → receive results VMO
//!   5. Call RECOMPUTE, read results via seqlock
//!   6. Verify line count, positions, and byte ranges
//!   7. Insert more text, RECOMPUTE again, verify updated layout
//!   8. Exit 0 on success

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};

const HANDLE_NS_EP: Handle = Handle(2);
const PAGE_SIZE: usize = 16384;

const EXIT_CONSOLE_WATCH: u32 = 1;
const EXIT_DOC_WATCH: u32 = 2;
const EXIT_LAYOUT_WATCH: u32 = 3;
const EXIT_DOC_SETUP: u32 = 4;
const EXIT_DOC_MAP: u32 = 5;
const EXIT_VIEWPORT_CREATE: u32 = 6;
const EXIT_VIEWPORT_MAP: u32 = 7;
const EXIT_VIEWPORT_DUP: u32 = 8;
const EXIT_LAYOUT_SETUP: u32 = 9;
const EXIT_RESULTS_MAP: u32 = 10;
const EXIT_INSERT_1: u32 = 20;
const EXIT_RECOMPUTE_1: u32 = 21;
const EXIT_LINE_COUNT_1: u32 = 22;
const EXIT_LINE_0: u32 = 23;
const EXIT_LINE_1: u32 = 24;
const EXIT_LINE_2: u32 = 25;
const EXIT_INSERT_2: u32 = 30;
const EXIT_RECOMPUTE_2: u32 = 31;
const EXIT_LINE_COUNT_2: u32 = 32;
const EXIT_INFO_CHECK: u32 = 40;

fn delete_all(doc_ep: Handle) {
    let (status, reply_data) =
        match ipc::client::call_simple(doc_ep, document_service::GET_INFO, &[]) {
            Ok(r) => r,
            Err(_) => return,
        };

    if status != 0 {
        return;
    }

    let info = document_service::InfoReply::read_from(&reply_data);

    if info.content_len > 0 {
        let req = document_service::DeleteRequest {
            offset: 0,
            len: info.content_len,
        };
        let mut data = [0u8; document_service::DeleteRequest::SIZE];

        req.write_to(&mut data);

        let _ = ipc::client::call_simple(doc_ep, document_service::DELETE, &data);
    }
}

fn insert(doc_ep: Handle, offset: u64, text: &[u8], exit_code: u32) {
    let header = document_service::InsertHeader { offset };
    let mut payload = [0u8; ipc::MAX_PAYLOAD];

    header.write_to(&mut payload);

    let copy_len = text.len().min(document_service::InsertHeader::MAX_INLINE);

    payload[document_service::InsertHeader::SIZE..document_service::InsertHeader::SIZE + copy_len]
        .copy_from_slice(&text[..copy_len]);

    let total = document_service::InsertHeader::SIZE + copy_len;
    let (status, _) =
        match ipc::client::call_simple(doc_ep, document_service::INSERT, &payload[..total]) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(exit_code),
        };

    if status != 0 {
        abi::thread::exit(exit_code);
    }
}

fn recompute(layout_ep: Handle, exit_code: u32) -> layout_service::InfoReply {
    let (status, reply_data) =
        match ipc::client::call_simple(layout_ep, layout_service::RECOMPUTE, &[]) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(exit_code),
        };

    if status != 0 {
        abi::thread::exit(exit_code);
    }

    layout_service::InfoReply::read_from(&reply_data)
}

fn read_results(
    results_va: usize,
    max_lines: usize,
) -> (layout_service::LayoutHeader, [layout_service::LineInfo; 64]) {
    let mut buf = [0u8; layout_service::RESULTS_VALUE_SIZE];
    let read_size =
        layout_service::LayoutHeader::SIZE + max_lines.min(64) * layout_service::LineInfo::SIZE;
    let mut reader = unsafe { ipc::register::Reader::new(results_va as *const u8, read_size) };

    reader.read(&mut buf[..read_size]);

    let header = layout_service::LayoutHeader::read_from(&buf);
    let mut lines = [layout_service::LineInfo {
        byte_offset: 0,
        byte_length: 0,
        x: 0.0,
        y: 0,
        width: 0.0,
    }; 64];
    let count = (header.line_count as usize).min(64);

    for (i, line) in lines.iter_mut().enumerate().take(count) {
        let off = layout_service::LayoutHeader::SIZE + i * layout_service::LineInfo::SIZE;

        *line = layout_service::LineInfo::read_from(&buf[off..]);
    }

    (header, lines)
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    // ── Connect to services ───────────────────────────────────────

    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_WATCH),
    };

    console::write(console_ep, b"test-layout: starting\n");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"test-layout: doc not found\n");

            abi::thread::exit(EXIT_DOC_WATCH);
        }
    };

    let layout_ep = match name::watch(HANDLE_NS_EP, b"layout") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"test-layout: layout not found\n");

            abi::thread::exit(EXIT_LAYOUT_WATCH);
        }
    };

    console::write(console_ep, b"test-layout: services found\n");

    // ── Get document buffer VMO ───────────────────────────────────

    let _doc_va = {
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

    // ── Create viewport state VMO ─────────────────────────────────

    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let viewport_size = ipc::register::required_size(layout_service::ViewportState::SIZE);
    let viewport_vmo_size = viewport_size.next_multiple_of(PAGE_SIZE);
    let viewport_vmo = match abi::vmo::create(viewport_vmo_size, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_CREATE),
    };
    let viewport_va = match abi::vmo::map(viewport_vmo, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_MAP),
    };

    // Initialize and write viewport state: monospace 10pt chars, 20pt
    // line height, 200pt viewport width.
    ipc::register::init(viewport_va as *mut u8);

    let viewport = layout_service::ViewportState {
        scroll_y: 0,
        viewport_width: 200,
        viewport_height: 600,
        char_width_fp: layout_service::ViewportState::encode_char_width(10.0),
        line_height: 20,
    };
    let mut vp_buf = [0u8; layout_service::ViewportState::SIZE];

    viewport.write_to(&mut vp_buf);

    {
        let mut writer = unsafe {
            ipc::register::Writer::new(viewport_va as *mut u8, layout_service::ViewportState::SIZE)
        };

        writer.write(&vp_buf);
    }

    console::write(console_ep, b"test-layout: viewport ready\n");

    // ── Layout SETUP: send viewport VMO, receive results VMO ──────

    let viewport_dup = match abi::handle::dup(viewport_vmo, Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_VIEWPORT_DUP),
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
            Err(_) => {
                console::write(console_ep, b"test-layout: setup call failed\n");

                abi::thread::exit(EXIT_LAYOUT_SETUP);
            }
        };
        let header = ipc::message::Header::read_from(&buf);

        if header.is_error() || result.handle_count == 0 {
            console::write(console_ep, b"test-layout: setup reply error\n");

            abi::thread::exit(EXIT_LAYOUT_SETUP);
        }

        let results_vmo = Handle(recv_handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);

        match abi::vmo::map(results_vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_RESULTS_MAP),
        }
    };

    console::write(console_ep, b"test-layout: setup OK\n");

    // Wait for test-document to signal completion, then clear.
    let _ = name::watch(HANDLE_NS_EP, b"test-doc-done");

    delete_all(doc_ep);

    console::write(console_ep, b"test-layout: doc cleared\n");

    // ── Test 1: Insert multi-line text, verify layout ─────────────
    //
    // "Hello world\nSecond line\nThird"
    // With 10pt monospace chars and 200pt width (20 chars per line):
    //   Line 0: "Hello world" (11 chars, 110pt)
    //   Line 1: "Second line" (11 chars, 110pt)
    //   Line 2: "Third"       (5 chars, 50pt)

    insert(doc_ep, 0, b"Hello world\nSecond line\nThird", EXIT_INSERT_1);

    console::write(console_ep, b"test-layout: inserted text\n");

    let info = recompute(layout_ep, EXIT_RECOMPUTE_1);

    console::write(console_ep, b"test-layout: recompute done\n");

    if info.line_count != 3 {
        console::write_u32(
            console_ep,
            b"test-layout: expected 3 lines, got ",
            info.line_count,
        );

        abi::thread::exit(EXIT_LINE_COUNT_1);
    }

    let (header, lines) = read_results(results_va, 8);

    if header.line_count != 3 {
        abi::thread::exit(EXIT_LINE_COUNT_1);
    }

    // Line 0: "Hello world" at offset 0, length 11, y=0, width=110.
    if lines[0].byte_offset != 0
        || lines[0].byte_length != 11
        || lines[0].y != 0
        || (lines[0].width - 110.0).abs() > 0.5
    {
        console::write_u32(console_ep, b"test-layout: line0 off=", lines[0].byte_offset);
        console::write_u32(console_ep, b"test-layout: line0 len=", lines[0].byte_length);

        abi::thread::exit(EXIT_LINE_0);
    }
    // Line 1: "Second line" at offset 12, length 11, y=20.
    if lines[1].byte_offset != 12
        || lines[1].byte_length != 11
        || lines[1].y != 20
        || (lines[1].width - 110.0).abs() > 0.5
    {
        abi::thread::exit(EXIT_LINE_1);
    }
    // Line 2: "Third" at offset 24, length 5, y=40.
    if lines[2].byte_offset != 24
        || lines[2].byte_length != 5
        || lines[2].y != 40
        || (lines[2].width - 50.0).abs() > 0.5
    {
        abi::thread::exit(EXIT_LINE_2);
    }

    console::write(console_ep, b"test-layout: lines verified\n");

    // ── Test 2: Append text, re-layout, verify update ─────────────
    //
    // Append " line\nFourth" after "Third" (offset 29, 12 bytes).
    // Document becomes: "Hello world\nSecond line\nThird line\nFourth" (41 bytes)
    //   Line 0: "Hello world" (11 chars)
    //   Line 1: "Second line" (11 chars)
    //   Line 2: "Third line"  (10 chars)
    //   Line 3: "Fourth"      (6 chars)

    insert(doc_ep, 29, b" line\nFourth", EXIT_INSERT_2);

    let info2 = recompute(layout_ep, EXIT_RECOMPUTE_2);

    if info2.line_count != 4 {
        console::write_u32(
            console_ep,
            b"test-layout: expected 4 lines, got ",
            info2.line_count,
        );

        abi::thread::exit(EXIT_LINE_COUNT_2);
    }

    let (header2, lines2) = read_results(results_va, 8);

    if header2.line_count != 4 || header2.content_len != 41 {
        abi::thread::exit(EXIT_LINE_COUNT_2);
    }
    // Verify the updated line 2 and new line 3.
    if lines2[2].byte_offset != 24
        || lines2[2].byte_length != 10
        || (lines2[2].width - 100.0).abs() > 0.5
    {
        abi::thread::exit(EXIT_LINE_COUNT_2);
    }
    if lines2[3].byte_offset != 35
        || lines2[3].byte_length != 6
        || lines2[3].y != 60
        || (lines2[3].width - 60.0).abs() > 0.5
    {
        abi::thread::exit(EXIT_LINE_COUNT_2);
    }

    console::write(console_ep, b"test-layout: update verified\n");

    // ── Test 3: GET_INFO ──────────────────────────────────────────

    let (status, reply_data) =
        match ipc::client::call_simple(layout_ep, layout_service::GET_INFO, &[]) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_INFO_CHECK),
        };

    if status != 0 {
        abi::thread::exit(EXIT_INFO_CHECK);
    }

    let info3 = layout_service::InfoReply::read_from(&reply_data);

    if info3.line_count != 4
        || info3.content_len != 41
        || info3.viewport_width != 200
        || info3.line_height != 20
    {
        abi::thread::exit(EXIT_INFO_CHECK);
    }

    console::write(console_ep, b"test-layout: info OK\n");
    console::write(console_ep, b"test-layout: PASS\n");

    let done_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0),
    };

    name::register(HANDLE_NS_EP, b"test-layout-done", done_ep);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
