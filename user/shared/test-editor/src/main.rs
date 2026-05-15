//! Text editor integration test — sends keystrokes, verifies document edits.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint
//!
//! Sequence:
//!   1. Wait for test-presenter to finish
//!   2. Clear document, connect to editor and doc buffer
//!   3. Send printable characters, verify document content
//!   4. Test backspace, delete, return, tab, shift+tab
//!   5. Exit 0 on success

#![no_std]
#![no_main]

use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicU32, Ordering},
};

use abi::types::{Handle, Rights};

const HANDLE_NS_EP: Handle = Handle(2);

const EXIT_CONSOLE: u32 = 1;
const EXIT_DOC: u32 = 2;
const EXIT_EDITOR: u32 = 3;
const EXIT_DOC_SETUP: u32 = 4;
const EXIT_DOC_MAP: u32 = 5;
const EXIT_CLEAR: u32 = 6;
const EXIT_INSERT_CHAR: u32 = 10;
const EXIT_VERIFY_INSERT: u32 = 11;
const EXIT_BACKSPACE: u32 = 20;
const EXIT_VERIFY_BACKSPACE: u32 = 21;
const EXIT_MORE_CHARS: u32 = 30;
const EXIT_VERIFY_MORE: u32 = 31;
const EXIT_RETURN: u32 = 40;
const EXIT_VERIFY_RETURN: u32 = 41;
const EXIT_TAB: u32 = 50;
const EXIT_VERIFY_TAB: u32 = 51;
const EXIT_SHIFT_TAB: u32 = 60;
const EXIT_VERIFY_SHIFT_TAB: u32 = 61;
const EXIT_DELETE: u32 = 70;
const EXIT_VERIFY_DELETE: u32 = 71;

// ── Document buffer reading ──────────────────────────────────────

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

fn doc_content(doc_va: usize, len: usize) -> &'static [u8] {
    unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            len,
        )
    }
}

// ── Editor IPC helpers ───────────────────────────────────────────

fn dispatch_key(
    editor_ep: Handle,
    key_code: u16,
    modifiers: u8,
    character: u8,
) -> Option<text_editor::KeyReply> {
    let dispatch = text_editor::KeyDispatch {
        key_code,
        modifiers,
        character,
    };
    let mut data = [0u8; text_editor::KeyDispatch::SIZE];

    dispatch.write_to(&mut data);

    match ipc::client::call_simple(editor_ep, text_editor::DISPATCH_KEY, &data) {
        Ok((0, reply_data)) => Some(text_editor::KeyReply::read_from(&reply_data)),
        _ => None,
    }
}

fn dispatch_char(editor_ep: Handle, ch: u8) -> Option<text_editor::KeyReply> {
    dispatch_key(editor_ep, 0, 0, ch)
}

fn dispatch_special(editor_ep: Handle, key_code: u16) -> Option<text_editor::KeyReply> {
    dispatch_key(editor_ep, key_code, 0, 0)
}

fn dispatch_special_shift(editor_ep: Handle, key_code: u16) -> Option<text_editor::KeyReply> {
    dispatch_key(editor_ep, key_code, text_editor::MOD_SHIFT, 0)
}

// ── Document service helpers ─────────────────────────────────────

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

fn move_cursor(doc_ep: Handle, position: u64) {
    let req = document_service::CursorMove { position };
    let mut data = [0u8; document_service::CursorMove::SIZE];

    req.write_to(&mut data);

    let _ = ipc::client::call_simple(doc_ep, document_service::CURSOR_MOVE, &data);
}

// ── Verify helpers ───────────────────────────────────────────────

fn verify_content(doc_va: usize, expected: &[u8], exit_code: u32) {
    let (content_len, _) = read_doc_header(doc_va);

    if content_len != expected.len() {
        abi::thread::exit(exit_code);
    }

    let content = doc_content(doc_va, content_len);

    if content != expected {
        abi::thread::exit(exit_code);
    }
}

fn verify_cursor(doc_va: usize, expected: usize, exit_code: u32) {
    let (_, cursor_pos) = read_doc_header(doc_va);

    if cursor_pos != expected {
        abi::thread::exit(exit_code);
    }
}

// ── Entry point ──────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE),
    };

    console::write(console_ep, b"test-editor: starting\n");

    // Wait for test-presenter to finish.
    let _ = name::watch(HANDLE_NS_EP, b"test-pres-done");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_DOC),
    };
    let editor_ep = match name::watch(HANDLE_NS_EP, b"editor.text") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"test-editor: editor not found\n");

            abi::thread::exit(EXIT_EDITOR);
        }
    };

    // Get doc buffer VMO from document service SETUP.
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

    console::write(console_ep, b"test-editor: setup done\n");

    // Clear document from prior tests.
    delete_all(doc_ep);
    verify_content(doc_va, b"", EXIT_CLEAR);

    // ── Test 1: Insert printable characters "Hi" ─────────────────
    let reply = dispatch_char(editor_ep, b'H');

    if reply.is_none() || reply.unwrap().action != text_editor::ACTION_INSERTED {
        abi::thread::exit(EXIT_INSERT_CHAR);
    }

    dispatch_char(editor_ep, b'i');
    verify_content(doc_va, b"Hi", EXIT_VERIFY_INSERT);
    verify_cursor(doc_va, 2, EXIT_VERIFY_INSERT);

    console::write(console_ep, b"test-editor: insert OK\n");

    // ── Test 2: Backspace → "H" ──────────────────────────────────
    let reply = dispatch_special(editor_ep, text_editor::HID_KEY_BACKSPACE);

    if reply.is_none() || reply.unwrap().action != text_editor::ACTION_DELETED {
        abi::thread::exit(EXIT_BACKSPACE);
    }

    verify_content(doc_va, b"H", EXIT_VERIFY_BACKSPACE);
    verify_cursor(doc_va, 1, EXIT_VERIFY_BACKSPACE);

    console::write(console_ep, b"test-editor: backspace OK\n");

    // ── Test 3: More characters → "Hello" ────────────────────────
    for &ch in b"ello" {
        if dispatch_char(editor_ep, ch).is_none() {
            abi::thread::exit(EXIT_MORE_CHARS);
        }
    }

    verify_content(doc_va, b"Hello", EXIT_VERIFY_MORE);
    verify_cursor(doc_va, 5, EXIT_VERIFY_MORE);

    console::write(console_ep, b"test-editor: more chars OK\n");

    // ── Test 4: Return → "Hello\n" ───────────────────────────────
    let reply = dispatch_special(editor_ep, text_editor::HID_KEY_RETURN);

    if reply.is_none() || reply.unwrap().action != text_editor::ACTION_INSERTED {
        abi::thread::exit(EXIT_RETURN);
    }

    verify_content(doc_va, b"Hello\n", EXIT_VERIFY_RETURN);
    verify_cursor(doc_va, 6, EXIT_VERIFY_RETURN);

    console::write(console_ep, b"test-editor: return OK\n");

    // ── Test 5: Tab → "Hello\n    " ──────────────────────────────
    let reply = dispatch_special(editor_ep, text_editor::HID_KEY_TAB);

    if reply.is_none() || reply.unwrap().action != text_editor::ACTION_INSERTED {
        abi::thread::exit(EXIT_TAB);
    }

    verify_content(doc_va, b"Hello\n    ", EXIT_VERIFY_TAB);
    verify_cursor(doc_va, 10, EXIT_VERIFY_TAB);

    console::write(console_ep, b"test-editor: tab OK\n");

    // ── Test 6: Shift+Tab (dedent) → "Hello\n" ──────────────────
    let reply = dispatch_special_shift(editor_ep, text_editor::HID_KEY_TAB);

    if reply.is_none() || reply.unwrap().action != text_editor::ACTION_DELETED {
        abi::thread::exit(EXIT_SHIFT_TAB);
    }

    verify_content(doc_va, b"Hello\n", EXIT_VERIFY_SHIFT_TAB);

    console::write(console_ep, b"test-editor: shift+tab OK\n");

    // ── Test 7: Delete key at position 4 → "Hell\n" ─────────────
    // Move cursor to position 4 (before 'o').
    move_cursor(doc_ep, 4);
    verify_cursor(doc_va, 4, EXIT_DELETE);

    let reply = dispatch_special(editor_ep, text_editor::HID_KEY_DELETE);

    if reply.is_none() || reply.unwrap().action != text_editor::ACTION_DELETED {
        abi::thread::exit(EXIT_DELETE);
    }

    verify_content(doc_va, b"Hell\n", EXIT_VERIFY_DELETE);

    console::write(console_ep, b"test-editor: delete OK\n");

    // ── Done ─────────────────────────────────────────────────────
    console::write(console_ep, b"test-editor: PASS\n");

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
