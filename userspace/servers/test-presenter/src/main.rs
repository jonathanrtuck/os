//! Presenter integration test — verifies scene graph construction.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint
//!
//! Sequence:
//!   1. Wait for test-layout to finish, clear document
//!   2. Insert multi-line text into the document
//!   3. Call presenter BUILD
//!   4. Read scene graph VMO, verify node structure
//!   5. Verify cursor position
//!   6. Exit 0 on success

#![no_std]
#![no_main]

extern crate heap;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use scene::{Content, NODES_OFFSET, NULL, Node, NodeId, ROLE_CARET, SceneHeader};

const HANDLE_NS_EP: Handle = Handle(2);

const EXIT_CONSOLE: u32 = 1;
const EXIT_DOC: u32 = 2;
const EXIT_PRESENTER: u32 = 3;
const EXIT_SCENE_SETUP: u32 = 4;
const EXIT_SCENE_MAP: u32 = 5;
const EXIT_INSERT: u32 = 10;
const EXIT_BUILD: u32 = 11;
const EXIT_NODE_COUNT: u32 = 12;
const EXIT_ROOT: u32 = 13;
const EXIT_VIEWPORT: u32 = 14;
const EXIT_LINE_NODES: u32 = 15;
const EXIT_CURSOR_NODE: u32 = 16;
const EXIT_INFO: u32 = 20;

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

fn read_node(scene_va: usize, id: NodeId) -> Node {
    let offset = NODES_OFFSET + (id as usize) * core::mem::size_of::<Node>();

    unsafe { core::ptr::read((scene_va + offset) as *const Node) }
}

fn read_header(scene_va: usize) -> SceneHeader {
    unsafe { core::ptr::read(scene_va as *const SceneHeader) }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE),
    };

    console::write(console_ep, b"test-presenter: starting\n");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_DOC),
    };
    let presenter_ep = match name::watch(HANDLE_NS_EP, b"presenter") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"test-presenter: presenter not found\n");

            abi::thread::exit(EXIT_PRESENTER);
        }
    };

    console::write(console_ep, b"test-presenter: services found\n");

    // Get scene graph VMO from presenter SETUP.
    let scene_va = {
        let mut buf = [0u8; ipc::message::MSG_SIZE];

        ipc::message::write_request(&mut buf, presenter_service::SETUP, &[]);

        let mut recv_handles = [0u32; 4];
        let result = match abi::ipc::call(
            presenter_ep,
            &mut buf,
            ipc::message::HEADER_SIZE,
            &[],
            &mut recv_handles,
        ) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_SCENE_SETUP),
        };
        let header = ipc::message::Header::read_from(&buf);

        if header.is_error() || result.handle_count == 0 {
            abi::thread::exit(EXIT_SCENE_SETUP);
        }

        let vmo = Handle(recv_handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);

        match abi::vmo::map(vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_SCENE_MAP),
        }
    };

    console::write(console_ep, b"test-presenter: setup OK\n");

    // Wait for test-layout to signal completion, then clear.
    let _ = name::watch(HANDLE_NS_EP, b"test-layout-done");

    delete_all(doc_ep);
    // Insert test text: "Line one\nLine two\nLine three"
    // 3 lines: "Line one" (8), "Line two" (8), "Line three" (10)
    insert(doc_ep, 0, b"Line one\nLine two\nLine three", EXIT_INSERT);

    console::write(console_ep, b"test-presenter: text inserted\n");

    // Trigger presenter BUILD.
    let (status, reply_data) =
        match ipc::client::call_simple(presenter_ep, presenter_service::BUILD, &[]) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_BUILD),
        };

    if status != 0 {
        console::write(console_ep, b"test-presenter: build failed\n");

        abi::thread::exit(EXIT_BUILD);
    }

    let info = presenter_service::InfoReply::read_from(&reply_data);

    console::write(console_ep, b"test-presenter: build OK\n");
    console::write_u32(
        console_ep,
        b"test-presenter: nodes=",
        info.node_count as u32,
    );
    console::write_u32(console_ep, b"test-presenter: lines=", info.line_count);

    // Verify scene graph structure.
    // Expected: root(0) + viewport(1) + 3 line glyphs + cursor = 6 nodes.
    if info.line_count != 3 {
        console::write_u32(
            console_ep,
            b"test-presenter: expected 3 lines, got ",
            info.line_count,
        );

        abi::thread::exit(EXIT_LINE_NODES);
    }

    // node_count = root + viewport + 3 lines + cursor = 6
    if info.node_count < 5 {
        abi::thread::exit(EXIT_NODE_COUNT);
    }

    // Read scene graph header.
    let hdr = read_header(scene_va);

    if hdr.generation == 0 {
        abi::thread::exit(EXIT_ROOT);
    }

    // Root node: has background, non-zero dimensions.
    let root = read_node(scene_va, hdr.root);

    if root.width == 0 || root.height == 0 {
        abi::thread::exit(EXIT_ROOT);
    }

    if root.background.a == 0 {
        abi::thread::exit(EXIT_ROOT);
    }

    console::write(console_ep, b"test-presenter: root OK\n");

    // Viewport node: clips children.
    let viewport_id = root.first_child;

    if viewport_id == NULL {
        abi::thread::exit(EXIT_VIEWPORT);
    }

    let viewport = read_node(scene_va, viewport_id);

    if !viewport.clips_children() {
        abi::thread::exit(EXIT_VIEWPORT);
    }

    console::write(console_ep, b"test-presenter: viewport OK\n");

    // Walk viewport children — expect Glyphs nodes for each line.
    let mut child = viewport.first_child;
    let mut glyph_count = 0u32;
    let mut last_child = child;

    while child != NULL {
        let node = read_node(scene_va, child);

        match node.content {
            Content::Glyphs {
                glyph_count: gc, ..
            } => {
                if gc == 0 {
                    abi::thread::exit(EXIT_LINE_NODES);
                }

                glyph_count += 1;
            }
            Content::None => {
                // Could be cursor node (last child, has background).
            }
            _ => {}
        }

        last_child = child;
        child = node.next_sibling;
    }

    if glyph_count != 3 {
        console::write_u32(
            console_ep,
            b"test-presenter: expected 3 glyph nodes, got ",
            glyph_count,
        );

        abi::thread::exit(EXIT_LINE_NODES);
    }

    console::write(console_ep, b"test-presenter: line nodes OK\n");

    // Cursor node: last child of viewport, has background color.
    let cursor = read_node(scene_va, last_child);

    if cursor.background.a == 0 {
        abi::thread::exit(EXIT_CURSOR_NODE);
    }
    if cursor.role != ROLE_CARET {
        abi::thread::exit(EXIT_CURSOR_NODE);
    }

    console::write(console_ep, b"test-presenter: cursor OK\n");

    // GET_INFO check.
    let (status2, reply2) =
        match ipc::client::call_simple(presenter_ep, presenter_service::GET_INFO, &[]) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_INFO),
        };

    if status2 != 0 {
        abi::thread::exit(EXIT_INFO);
    }

    let info2 = presenter_service::InfoReply::read_from(&reply2);

    if info2.line_count != 3 || info2.content_len != 28 {
        console::write_u32(
            console_ep,
            b"test-presenter: info content_len=",
            info2.content_len,
        );

        abi::thread::exit(EXIT_INFO);
    }

    console::write(console_ep, b"test-presenter: info OK\n");
    console::write(console_ep, b"test-presenter: PASS\n");

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
