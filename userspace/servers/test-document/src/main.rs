//! Document service integration test — exercises the document service over IPC.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint
//!
//! Sequence:
//!   1. Watch for "console" and "document"
//!   2. SETUP — receive doc buffer VMO, map it RO
//!   3. INSERT "Hello" at offset 0, verify buffer
//!   4. INSERT " world" at offset 5, verify buffer = "Hello world"
//!   5. DELETE 5 bytes at offset 5, verify buffer = "Hello"
//!   6. UNDO — verify buffer = "Hello world"
//!   7. UNDO — verify buffer = "Hello"
//!   8. REDO — verify buffer = "Hello world"
//!   9. CURSOR_MOVE to position 3, verify via GET_INFO
//!  10. Exit 0 on success

#![no_std]
#![no_main]

use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicU32, Ordering},
};

use abi::types::{Handle, Rights};

const HANDLE_NS_EP: Handle = Handle(2);

const EXIT_CONSOLE_WATCH: u32 = 1;
const EXIT_DOC_WATCH: u32 = 2;
const EXIT_SETUP: u32 = 3;
const EXIT_MAP: u32 = 4;
const EXIT_INSERT_1: u32 = 10;
const EXIT_VERIFY_1: u32 = 11;
const EXIT_INSERT_2: u32 = 12;
const EXIT_VERIFY_2: u32 = 13;
const EXIT_DELETE: u32 = 14;
const EXIT_VERIFY_3: u32 = 15;
const EXIT_UNDO_1: u32 = 20;
const EXIT_VERIFY_4: u32 = 21;
const EXIT_UNDO_2: u32 = 22;
const EXIT_VERIFY_5: u32 = 23;
const EXIT_REDO: u32 = 24;
const EXIT_VERIFY_6: u32 = 25;
const EXIT_CURSOR: u32 = 30;
const EXIT_INFO: u32 = 31;

fn read_doc_content(doc_va: usize, max: usize) -> (usize, usize, u32) {
    loop {
        // SAFETY: doc_va is a valid RO mapping of the document buffer VMO.
        // Read generation with Acquire ordering to synchronize with the
        // document service's Release store.
        let generation = unsafe {
            let generation_ptr =
                (doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

            (*generation_ptr).load(Ordering::Acquire)
        };
        let content_len = unsafe { core::ptr::read_volatile(doc_va as *const u64) as usize };
        let cursor_pos = unsafe { core::ptr::read_volatile((doc_va + 8) as *const u64) as usize };
        // Re-read generation to detect torn reads.
        let generation2 = unsafe {
            let generation_ptr =
                (doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

            (*generation_ptr).load(Ordering::Acquire)
        };

        if generation == generation2 {
            return (content_len.min(max), cursor_pos, generation);
        }
    }
}

fn doc_bytes(doc_va: usize, len: usize) -> &'static [u8] {
    // SAFETY: doc_va + DOC_HEADER_SIZE..+len is within the mapped VMO.
    unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            len,
        )
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

fn delete(doc_ep: Handle, offset: u64, len: u64, exit_code: u32) {
    let req = document_service::DeleteRequest { offset, len };
    let mut data = [0u8; document_service::DeleteRequest::SIZE];

    req.write_to(&mut data);

    let (status, _) = match ipc::client::call_simple(doc_ep, document_service::DELETE, &data) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(exit_code),
    };

    if status != 0 {
        abi::thread::exit(exit_code);
    }
}

fn undo(doc_ep: Handle, exit_code: u32) {
    let (status, _) = match ipc::client::call_simple(doc_ep, document_service::UNDO, &[]) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(exit_code),
    };

    if status != 0 {
        abi::thread::exit(exit_code);
    }
}

fn redo(doc_ep: Handle, exit_code: u32) {
    let (status, _) = match ipc::client::call_simple(doc_ep, document_service::REDO, &[]) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(exit_code),
    };

    if status != 0 {
        abi::thread::exit(exit_code);
    }
}

fn verify(doc_va: usize, expected: &[u8], exit_code: u32) {
    let (len, _, _) = read_doc_content(doc_va, expected.len() + 1);

    if len != expected.len() {
        abi::thread::exit(exit_code);
    }

    let content = doc_bytes(doc_va, len);

    if content != expected {
        abi::thread::exit(exit_code);
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_WATCH),
    };

    console::write(console_ep, b"test-doc: starting\n");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"test-doc: document not found\n");

            abi::thread::exit(EXIT_DOC_WATCH);
        }
    };

    console::write(console_ep, b"test-doc: connected\n");

    // SETUP — receive the document buffer VMO.
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
            Err(_) => abi::thread::exit(EXIT_SETUP),
        };
        let header = ipc::message::Header::read_from(&buf);

        if header.is_error() || result.handle_count == 0 {
            abi::thread::exit(EXIT_SETUP);
        }

        let vmo = Handle(recv_handles[0]);
        let ro = Rights(Rights::READ.0 | Rights::MAP.0);

        match abi::vmo::map(vmo, 0, ro) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_MAP),
        }
    };

    console::write(console_ep, b"test-doc: setup OK\n");

    // 1. Insert "Hello" at offset 0.
    insert(doc_ep, 0, b"Hello", EXIT_INSERT_1);
    verify(doc_va, b"Hello", EXIT_VERIFY_1);

    console::write(console_ep, b"test-doc: insert 1 OK\n");

    // 2. Insert " world" at offset 5.
    insert(doc_ep, 5, b" world", EXIT_INSERT_2);
    verify(doc_va, b"Hello world", EXIT_VERIFY_2);

    console::write(console_ep, b"test-doc: insert 2 OK\n");

    // 3. Delete " world" (6 bytes at offset 5).
    delete(doc_ep, 5, 6, EXIT_DELETE);
    verify(doc_va, b"Hello", EXIT_VERIFY_3);

    console::write(console_ep, b"test-doc: delete OK\n");

    // 4. Undo — should restore "Hello world".
    undo(doc_ep, EXIT_UNDO_1);
    verify(doc_va, b"Hello world", EXIT_VERIFY_4);

    console::write(console_ep, b"test-doc: undo 1 OK\n");

    // 5. Undo — should restore "Hello".
    undo(doc_ep, EXIT_UNDO_2);
    verify(doc_va, b"Hello", EXIT_VERIFY_5);

    console::write(console_ep, b"test-doc: undo 2 OK\n");

    // 6. Redo — should restore "Hello world".
    redo(doc_ep, EXIT_REDO);
    verify(doc_va, b"Hello world", EXIT_VERIFY_6);

    console::write(console_ep, b"test-doc: redo OK\n");

    // 7. Cursor move + GET_INFO.
    {
        let req = document_service::CursorMove { position: 3 };
        let mut data = [0u8; document_service::CursorMove::SIZE];

        req.write_to(&mut data);

        let (status, _) =
            match ipc::client::call_simple(doc_ep, document_service::CURSOR_MOVE, &data) {
                Ok(r) => r,
                Err(_) => abi::thread::exit(EXIT_CURSOR),
            };

        if status != 0 {
            abi::thread::exit(EXIT_CURSOR);
        }
    }

    {
        let (status, reply_data) =
            match ipc::client::call_simple(doc_ep, document_service::GET_INFO, &[]) {
                Ok(r) => r,
                Err(_) => abi::thread::exit(EXIT_INFO),
            };

        if status != 0 {
            abi::thread::exit(EXIT_INFO);
        }

        let info = document_service::InfoReply::read_from(&reply_data);

        if info.cursor_pos != 3 || info.content_len != 11 {
            abi::thread::exit(EXIT_INFO);
        }
    }

    console::write(console_ep, b"test-doc: cursor+info OK\n");
    console::write(console_ep, b"test-doc: PASS\n");

    // Signal completion so downstream tests can serialize.
    let done_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0),
    };

    name::register(HANDLE_NS_EP, b"test-doc-done", done_ep);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
