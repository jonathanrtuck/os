//! Document service — sole writer to the document buffer.
//!
//! Applies edit requests from editors via sync IPC. Manages undo ring
//! via COW snapshots through the store service. Signals change
//! notifications for downstream consumers (layout, presenter).
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!
//! Boots, looks up "store" from name service, establishes shared VMO
//! for bulk I/O, creates a document file, registers as "document",
//! and enters an IPC serve loop.

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
const DOC_BUF_PAGES: usize = 4;
const DOC_BUF_SIZE: usize = PAGE_SIZE * DOC_BUF_PAGES;
const STORE_SHARED_SIZE: usize = PAGE_SIZE * 4;

const MAX_UNDO: usize = 64;

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE001;
const EXIT_STORE_NOT_FOUND: u32 = 0xE002;
const EXIT_SHARED_VMO_CREATE: u32 = 0xE003;
const EXIT_SHARED_VMO_MAP: u32 = 0xE004;
const EXIT_SHARED_VMO_DUP: u32 = 0xE005;
const EXIT_DOC_VMO_CREATE: u32 = 0xE006;
const EXIT_DOC_VMO_MAP: u32 = 0xE007;
const EXIT_ENDPOINT_CREATE: u32 = 0xE008;
const EXIT_STORE_CREATE: u32 = 0xE00A;

// ── Undo state ─────────────────────────────────────────────────────

struct UndoState {
    snapshots: [u64; MAX_UNDO],
    count: usize,
    position: usize,
}

impl UndoState {
    const fn new() -> Self {
        Self {
            snapshots: [0; MAX_UNDO],
            count: 0,
            position: 0,
        }
    }

    fn set_initial(&mut self, snap_id: u64) {
        self.snapshots[0] = snap_id;
        self.count = 1;
        self.position = 0;
    }

    fn push(&mut self, snap_id: u64, discarded: &mut [u64; MAX_UNDO]) -> usize {
        let mut n = 0;

        for i in (self.position + 1)..self.count {
            discarded[n] = self.snapshots[i];
            n += 1;
        }

        self.count = self.position + 1;

        if self.count >= MAX_UNDO {
            discarded[n] = self.snapshots[0];
            n += 1;

            for i in 0..MAX_UNDO - 1 {
                self.snapshots[i] = self.snapshots[i + 1];
            }

            self.count -= 1;
            self.position -= 1;
        }

        self.snapshots[self.count] = snap_id;
        self.count += 1;
        self.position = self.count - 1;

        n
    }

    fn undo(&mut self) -> Option<u64> {
        if self.position > 0 {
            self.position -= 1;
            Some(self.snapshots[self.position])
        } else {
            None
        }
    }

    fn redo(&mut self) -> Option<u64> {
        if self.count > 0 && self.position < self.count - 1 {
            self.position += 1;

            Some(self.snapshots[self.position])
        } else {
            None
        }
    }

    fn snapshot_count(&self) -> usize {
        self.count
    }
}

// ── Document server ────────────────────────────────────────────────

struct DocumentServer {
    doc_va: usize,
    doc_capacity: usize,
    doc_vmo: Handle,
    content_len: usize,
    cursor_pos: usize,
    generation: u32,

    store_ep: Handle,
    store_shared_va: usize,
    store_shared_len: usize,
    file_id: u64,

    undo: UndoState,

    #[allow(dead_code)]
    console_ep: Handle,
}

impl DocumentServer {
    fn content_capacity(&self) -> usize {
        self.doc_capacity - document_service::DOC_HEADER_SIZE
    }

    // ── Buffer operations ──────────────────────────────────────────

    fn write_header(&mut self) {
        // SAFETY: doc_va is a valid RW mapping of at least DOC_HEADER_SIZE bytes.
        unsafe {
            let base = self.doc_va as *mut u8;

            core::ptr::write_volatile(base as *mut u64, self.content_len as u64);
            core::ptr::write_volatile(base.add(8) as *mut u64, self.cursor_pos as u64);
        }

        self.generation = self.generation.wrapping_add(1);

        // SAFETY: doc_va + 16 is within the 64-byte header, 4-byte aligned.
        // AtomicU32 Release ordering makes all prior writes (content_len,
        // cursor_pos, content bytes) visible to readers that Acquire-load
        // this generation counter.
        unsafe {
            let gen_ptr =
                (self.doc_va + document_service::DOC_OFFSET_GENERATION) as *const AtomicU32;

            (*gen_ptr).store(self.generation, Ordering::Release);
        }
    }

    fn apply_insert(&mut self, offset: usize, data: &[u8]) -> bool {
        let new_len = self.content_len + data.len();

        if offset > self.content_len || new_len > self.content_capacity() || data.is_empty() {
            return false;
        }

        // SAFETY: doc_va is a valid RW mapping. Header occupies the first
        // DOC_HEADER_SIZE bytes; content starts after. offset + data.len()
        // is within content_capacity (checked above). The copy handles
        // overlapping regions via copy (memmove semantics).
        unsafe {
            let base = (self.doc_va + document_service::DOC_HEADER_SIZE) as *mut u8;

            if offset < self.content_len {
                core::ptr::copy(
                    base.add(offset),
                    base.add(offset + data.len()),
                    self.content_len - offset,
                );
            }

            core::ptr::copy_nonoverlapping(data.as_ptr(), base.add(offset), data.len());
        }

        self.content_len = new_len;
        self.cursor_pos = offset + data.len();

        self.write_header();

        true
    }

    fn apply_delete(&mut self, offset: usize, len: usize) -> bool {
        if len == 0 || offset >= self.content_len || offset + len > self.content_len {
            return false;
        }

        // SAFETY: doc_va is a valid RW mapping. offset + len <= content_len,
        // so all indices are within the mapped content region.
        unsafe {
            let base = (self.doc_va + document_service::DOC_HEADER_SIZE) as *mut u8;
            let remaining = self.content_len - offset - len;

            if remaining > 0 {
                core::ptr::copy(base.add(offset + len), base.add(offset), remaining);
            }
        }

        self.content_len -= len;

        if self.cursor_pos > self.content_len {
            self.cursor_pos = self.content_len;
        } else if self.cursor_pos > offset {
            self.cursor_pos = offset;
        }

        self.write_header();

        true
    }

    // ── Store interaction ──────────────────────────────────────────

    fn flush_to_store(&mut self) {
        if self.content_len == 0 {
            let req = store_service::TruncateRequest {
                file_id: self.file_id,
                len: 0,
            };
            let mut data = [0u8; store_service::TruncateRequest::SIZE];

            req.write_to(&mut data);

            let _ = ipc::client::call_simple(self.store_ep, store_service::TRUNCATE, &data);

            return;
        }

        let mut offset: usize = 0;

        while offset < self.content_len {
            let chunk = (self.content_len - offset).min(self.store_shared_len);

            // SAFETY: doc_va + DOC_HEADER_SIZE + offset..+chunk is within
            // the document buffer. store_shared_va..+chunk is within the
            // shared VMO.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (self.doc_va + document_service::DOC_HEADER_SIZE + offset) as *const u8,
                    self.store_shared_va as *mut u8,
                    chunk,
                );
            }

            let req = store_service::WriteRequest {
                file_id: self.file_id,
                offset: offset as u64,
                vmo_offset: 0,
                len: chunk as u32,
            };
            let mut data = [0u8; store_service::WriteRequest::SIZE];

            req.write_to(&mut data);

            let _ = ipc::client::call_simple(self.store_ep, store_service::WRITE_DOC, &data);

            offset += chunk;
        }

        let req = store_service::TruncateRequest {
            file_id: self.file_id,
            len: self.content_len as u64,
        };
        let mut data = [0u8; store_service::TruncateRequest::SIZE];

        req.write_to(&mut data);

        let _ = ipc::client::call_simple(self.store_ep, store_service::TRUNCATE, &data);
    }

    fn reload_from_store(&mut self) {
        self.content_len = 0;

        let mut offset: usize = 0;

        loop {
            let max = self.store_shared_len.min(self.content_capacity() - offset);

            if max == 0 {
                break;
            }

            let req = store_service::ReadRequest {
                file_id: self.file_id,
                offset: offset as u64,
                vmo_offset: 0,
                max_len: max as u32,
            };
            let mut data = [0u8; store_service::ReadRequest::SIZE];

            req.write_to(&mut data);

            match ipc::client::call_simple(self.store_ep, store_service::READ_DOC, &data) {
                Ok((0, payload)) => {
                    let reply = store_service::ReadReply::read_from(&payload);
                    let n = reply.bytes_read as usize;

                    if n == 0 {
                        break;
                    }

                    // SAFETY: store_shared_va..+n is within the shared VMO.
                    // doc_va + DOC_HEADER_SIZE + offset..+n is within the
                    // document buffer (bounded by content_capacity).
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            self.store_shared_va as *const u8,
                            (self.doc_va + document_service::DOC_HEADER_SIZE + offset) as *mut u8,
                            n,
                        );
                    }

                    offset += n;

                    if n < max {
                        break;
                    }
                }
                _ => break,
            }
        }

        self.content_len = offset;

        if self.cursor_pos > self.content_len {
            self.cursor_pos = self.content_len;
        }

        self.write_header();
    }

    fn take_snapshot(&mut self) -> Option<u64> {
        let req = store_service::SnapshotRequest {
            file_id: self.file_id,
        };
        let mut data = [0u8; store_service::SnapshotRequest::SIZE];

        req.write_to(&mut data);

        let (status, payload) =
            ipc::client::call_simple(self.store_ep, store_service::SNAPSHOT, &data).ok()?;

        if status != 0 {
            return None;
        }

        Some(store_service::SnapshotReply::read_from(&payload).snapshot_id)
    }

    fn delete_snapshot(&self, snap_id: u64) {
        let req = store_service::DeleteSnapshotRequest {
            snapshot_id: snap_id,
        };
        let mut data = [0u8; store_service::DeleteSnapshotRequest::SIZE];

        req.write_to(&mut data);

        let _ = ipc::client::call_simple(self.store_ep, store_service::DELETE_SNAPSHOT, &data);
    }

    fn restore_to_snapshot(&mut self, snap_id: u64) -> bool {
        let req = store_service::RestoreRequest {
            file_id: self.file_id,
            snapshot_id: snap_id,
        };
        let mut data = [0u8; store_service::RestoreRequest::SIZE];

        req.write_to(&mut data);

        matches!(
            ipc::client::call_simple(self.store_ep, store_service::RESTORE, &data),
            Ok((0, _))
        )
    }

    // ── Undo/Redo ──────────────────────────────────────────────────

    fn persist_and_snapshot(&mut self) {
        self.flush_to_store();

        if let Some(snap_id) = self.take_snapshot() {
            let mut discarded = [0u64; MAX_UNDO];
            let n = self.undo.push(snap_id, &mut discarded);

            for &id in &discarded[..n] {
                self.delete_snapshot(id);
            }
        }
    }

    fn perform_undo(&mut self) -> bool {
        if let Some(snap_id) = self.undo.undo()
            && self.restore_to_snapshot(snap_id)
        {
            self.reload_from_store();

            return true;
        }

        false
    }

    fn perform_redo(&mut self) -> bool {
        if let Some(snap_id) = self.undo.redo()
            && self.restore_to_snapshot(snap_id)
        {
            self.reload_from_store();

            return true;
        }

        false
    }

    // ── Reply helpers ──────────────────────────────────────────────

    fn reply_edit(&self, msg: Incoming<'_>) {
        let reply = document_service::EditReply {
            content_len: self.content_len as u64,
            cursor_pos: self.cursor_pos as u64,
        };
        let mut data = [0u8; document_service::EditReply::SIZE];

        reply.write_to(&mut data);

        let _ = msg.reply_ok(&data, &[]);
    }
}

// ── Dispatch ───────────────────────────────────────────────────────

impl Dispatch for DocumentServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            document_service::SETUP => {
                let ro = Rights(Rights::READ.0 | Rights::MAP.0);

                match abi::handle::dup(self.doc_vmo, ro) {
                    Ok(dup) => {
                        let reply = document_service::SetupReply {
                            content_len: self.content_len as u64,
                            cursor_pos: self.cursor_pos as u64,
                            format: document_service::FORMAT_PLAIN,
                            file_id: self.file_id,
                        };
                        let mut data = [0u8; document_service::SetupReply::SIZE];

                        reply.write_to(&mut data);

                        let _ = msg.reply_ok(&data, &[dup.0]);
                    }
                    Err(_) => {
                        let _ = msg.reply_error(ipc::STATUS_INVALID);
                    }
                }
            }

            document_service::INSERT => {
                if msg.payload.len() < document_service::InsertHeader::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let header = document_service::InsertHeader::read_from(msg.payload);
                let data = &msg.payload[document_service::InsertHeader::SIZE..];

                if data.is_empty() {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                if self.apply_insert(header.offset as usize, data) {
                    self.persist_and_snapshot();
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            }

            document_service::DELETE => {
                if msg.payload.len() < document_service::DeleteRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = document_service::DeleteRequest::read_from(msg.payload);

                if self.apply_delete(req.offset as usize, req.len as usize) {
                    self.persist_and_snapshot();
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            }

            document_service::CURSOR_MOVE => {
                if msg.payload.len() < document_service::CursorMove::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = document_service::CursorMove::read_from(msg.payload);
                let pos = req.position as usize;

                if pos <= self.content_len {
                    self.cursor_pos = pos;

                    self.write_header();
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            }

            document_service::SELECT => {
                if msg.payload.len() < document_service::Selection::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let sel = document_service::Selection::read_from(msg.payload);
                let anchor = sel.anchor as usize;
                let cursor = sel.cursor as usize;

                if anchor <= self.content_len && cursor <= self.content_len {
                    self.cursor_pos = cursor;

                    self.write_header();
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            }

            document_service::UNDO => {
                if self.perform_undo() {
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                }
            }

            document_service::REDO => {
                if self.perform_redo() {
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_NOT_FOUND);
                }
            }

            document_service::GET_INFO => {
                let reply = document_service::InfoReply {
                    content_len: self.content_len as u64,
                    cursor_pos: self.cursor_pos as u64,
                    format: document_service::FORMAT_PLAIN,
                    file_id: self.file_id,
                    snapshot_count: self.undo.snapshot_count() as u32,
                };
                let mut data = [0u8; document_service::InfoReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }

            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

// ── Entry point ────────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_NOT_FOUND),
    };

    console::write(console_ep, b"document: starting\n");

    let store_ep = match name::watch(HANDLE_NS_EP, b"store") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"document: store not found\n");

            abi::thread::exit(EXIT_STORE_NOT_FOUND);
        }
    };

    console::write(console_ep, b"document: store found\n");

    // Create shared VMO for bulk I/O with store.
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let store_vmo = abi::vmo::create(STORE_SHARED_SIZE, 0).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_SHARED_VMO_CREATE);
    });
    let store_shared_va = abi::vmo::map(store_vmo, 0, rw).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_SHARED_VMO_MAP);
    });
    let store_dup = abi::handle::dup(store_vmo, Rights::ALL).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_SHARED_VMO_DUP);
    });

    {
        let mut buf = [0u8; ipc::message::MSG_SIZE];

        ipc::message::write_request(&mut buf, store_service::SETUP, &[]);

        let _ = abi::ipc::call(
            store_ep,
            &mut buf,
            ipc::message::HEADER_SIZE,
            &[store_dup.0],
            &mut [],
        );
    }

    console::write(console_ep, b"document: store setup done\n");

    // Create a document file in the store.
    let file_id = {
        let media = b"text/plain";
        let req = store_service::CreateRequest {
            media_type_len: media.len() as u16,
        };
        let mut payload = [0u8; 32];

        req.write_to(&mut payload);

        payload
            [store_service::CreateRequest::SIZE..store_service::CreateRequest::SIZE + media.len()]
            .copy_from_slice(media);

        let total_len = store_service::CreateRequest::SIZE + media.len();
        let mut buf = [0u8; ipc::message::MSG_SIZE];
        let reply = ipc::client::call(
            store_ep,
            store_service::CREATE,
            &payload[..total_len],
            &[],
            &mut [],
            &mut buf,
        );

        match reply {
            Ok(r) if !r.is_error() && r.payload.len() >= store_service::CreateReply::SIZE => {
                store_service::CreateReply::read_from(r.payload).file_id
            }
            _ => {
                console::write(console_ep, b"document: create failed\n");

                abi::thread::exit(EXIT_STORE_CREATE);
            }
        }
    };

    console::write(console_ep, b"document: file created\n");

    // Create document buffer VMO.
    let doc_vmo = abi::vmo::create(DOC_BUF_SIZE, 0).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_DOC_VMO_CREATE);
    });
    let doc_va = abi::vmo::map(doc_vmo, 0, rw).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_DOC_VMO_MAP);
    });
    // Take initial undo snapshot (empty document).
    let mut undo = UndoState::new();

    {
        let req = store_service::SnapshotRequest { file_id };
        let mut data = [0u8; store_service::SnapshotRequest::SIZE];

        req.write_to(&mut data);

        if let Ok((0, payload)) = ipc::client::call_simple(store_ep, store_service::SNAPSHOT, &data)
        {
            let reply = store_service::SnapshotReply::read_from(&payload);

            undo.set_initial(reply.snapshot_id);
        }
    }

    // Register with name service.
    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_ENDPOINT_CREATE),
    };

    name::register(HANDLE_NS_EP, b"document", own_ep);

    console::write(console_ep, b"document: ready\n");

    let mut server = DocumentServer {
        doc_va,
        doc_capacity: DOC_BUF_SIZE,
        doc_vmo,
        content_len: 0,
        cursor_pos: 0,
        generation: 0,
        store_ep,
        store_shared_va,
        store_shared_len: STORE_SHARED_SIZE,
        file_id,
        undo,
        console_ep,
    };

    server.write_header();

    ipc::server::serve(own_ep, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
