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
extern crate piecetable;

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
    head: usize,
    count: usize,
    position: usize,
}

impl UndoState {
    const fn new() -> Self {
        Self {
            snapshots: [0; MAX_UNDO],
            head: 0,
            count: 0,
            position: 0,
        }
    }

    fn idx(&self, logical: usize) -> usize {
        (self.head + logical) % MAX_UNDO
    }

    fn set_initial(&mut self, snap_id: u64) {
        self.head = 0;
        self.snapshots[0] = snap_id;
        self.count = 1;
        self.position = 0;
    }

    fn push(&mut self, snap_id: u64, discarded: &mut [u64; MAX_UNDO]) -> usize {
        let mut n = 0;

        for i in (self.position + 1)..self.count {
            discarded[n] = self.snapshots[self.idx(i)];
            n += 1;
        }

        self.count = self.position + 1;

        if self.count >= MAX_UNDO {
            discarded[n] = self.snapshots[self.idx(0)];
            n += 1;
            self.head = (self.head + 1) % MAX_UNDO;
            self.count -= 1;
            self.position -= 1;
        }

        self.snapshots[self.idx(self.count)] = snap_id;
        self.count += 1;
        self.position = self.count - 1;

        n
    }

    fn undo(&mut self) -> Option<u64> {
        if self.position > 0 {
            self.position -= 1;
            Some(self.snapshots[self.idx(self.position)])
        } else {
            None
        }
    }

    fn redo(&mut self) -> Option<u64> {
        if self.count > 0 && self.position < self.count - 1 {
            self.position += 1;

            Some(self.snapshots[self.idx(self.position)])
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
    sel_anchor: usize,
    generation: u32,
    format: u32,

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
            core::ptr::write_volatile(
                base.add(document_service::DOC_OFFSET_FORMAT) as *mut u32,
                self.format,
            );
            core::ptr::write_volatile(
                base.add(document_service::DOC_OFFSET_SEL_ANCHOR) as *mut u64,
                self.sel_anchor as u64,
            );
        }

        self.generation = self.generation.wrapping_add(1);

        // SAFETY: doc_va + 16 is within the 64-byte header, 4-byte aligned.
        // AtomicU32 Release ordering makes all prior writes (content_len,
        // cursor_pos, sel_anchor, content bytes) visible to readers that
        // Acquire-load this generation counter.
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
        self.sel_anchor = self.cursor_pos;

        self.write_header();

        true
    }

    // ── Piecetable buffer access ─────────────────────────────────────

    fn pt_buf_mut(&mut self) -> &mut [u8] {
        let cap = self.content_capacity();

        // SAFETY: doc_va + DOC_HEADER_SIZE is the start of content bytes,
        // cap bytes are available.
        unsafe {
            core::slice::from_raw_parts_mut(
                (self.doc_va + document_service::DOC_HEADER_SIZE) as *mut u8,
                cap,
            )
        }
    }

    fn pt_buf(&self) -> &[u8] {
        let cap = self.content_capacity();

        // SAFETY: doc_va + DOC_HEADER_SIZE is the start of content bytes.
        unsafe {
            core::slice::from_raw_parts(
                (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                cap,
            )
        }
    }

    fn apply_insert_rich(&mut self, offset: usize, data: &[u8]) -> bool {
        if data.is_empty() {
            return false;
        }

        let (new_content_len, new_cursor_pos) = {
            let buf = self.pt_buf_mut();

            if !piecetable::insert_bytes(buf, offset as u32, data) {
                return false;
            }

            let text_len = piecetable::text_len(buf) as usize;
            let cursor = (offset + data.len()).min(text_len);

            piecetable::set_cursor_pos(buf, cursor as u32);

            (piecetable::total_size(buf), cursor)
        };

        self.content_len = new_content_len;
        self.cursor_pos = new_cursor_pos;
        self.sel_anchor = self.cursor_pos;

        self.write_header();

        true
    }

    fn apply_delete_rich(&mut self, offset: usize, len: usize) -> bool {
        if len == 0 {
            return false;
        }

        // Pre-check text length before taking a mutable borrow.
        let text_len = piecetable::text_len(self.pt_buf()) as usize;

        if offset >= text_len || offset + len > text_len {
            return false;
        }

        let (new_content_len, new_cursor_pos) = {
            let buf = self.pt_buf_mut();

            if !piecetable::delete_range(buf, offset as u32, (offset + len) as u32) {
                return false;
            }

            let new_text_len = piecetable::text_len(buf) as usize;
            let cursor = offset.min(new_text_len);

            piecetable::set_cursor_pos(buf, cursor as u32);

            (piecetable::total_size(buf), cursor)
        };

        self.content_len = new_content_len;
        self.cursor_pos = new_cursor_pos;
        self.sel_anchor = self.cursor_pos;

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

        self.sel_anchor = self.cursor_pos;

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

        self.sel_anchor = self.cursor_pos;

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

// ── Format-aware helpers ──────────────────────────────────────────

impl DocumentServer {
    fn rich_text_len(&self) -> usize {
        if self.format == document_service::FORMAT_RICH {
            piecetable::text_len(self.pt_buf()) as usize
        } else {
            self.content_len
        }
    }

    fn do_insert(&mut self, offset: usize, data: &[u8]) -> bool {
        if self.format == document_service::FORMAT_RICH {
            self.apply_insert_rich(offset, data)
        } else {
            self.apply_insert(offset, data)
        }
    }

    fn do_delete(&mut self, offset: usize, len: usize) -> bool {
        if self.format == document_service::FORMAT_RICH {
            self.apply_delete_rich(offset, len)
        } else {
            self.apply_delete(offset, len)
        }
    }
}

// ── Dispatch ───────────────────────────────────────────────────────

impl Dispatch for DocumentServer {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            document_service::SETUP => match abi::handle::dup(self.doc_vmo, Rights::READ_MAP) {
                Ok(dup) => {
                    let reply = document_service::SetupReply {
                        content_len: self.content_len as u64,
                        cursor_pos: self.cursor_pos as u64,
                        format: self.format,
                        file_id: self.file_id,
                    };
                    let mut data = [0u8; document_service::SetupReply::SIZE];

                    reply.write_to(&mut data);

                    let _ = msg.reply_ok(&data, &[dup.0]);
                }
                Err(_) => {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            },

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

                if self.do_insert(header.offset as usize, data) {
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

                if self.do_delete(req.offset as usize, req.len as usize) {
                    self.persist_and_snapshot();
                    self.reply_edit(msg);
                } else {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);
                }
            }

            document_service::REPLACE => {
                if msg.payload.len() < document_service::ReplaceHeader::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let header = document_service::ReplaceHeader::read_from(msg.payload);
                let offset = header.offset as usize;
                let delete_len = header.delete_len as usize;
                let replacement = &msg.payload[document_service::ReplaceHeader::SIZE..];

                if delete_len > 0 && !self.do_delete(offset, delete_len) {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                if !replacement.is_empty() && !self.do_insert(offset, replacement) {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                self.persist_and_snapshot();
                self.reply_edit(msg);
            }

            document_service::CURSOR_MOVE => {
                if msg.payload.len() < document_service::CursorMove::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = document_service::CursorMove::read_from(msg.payload);
                let pos = req.position as usize;
                let tl = self.rich_text_len();

                if pos <= tl {
                    self.cursor_pos = pos;
                    self.sel_anchor = pos;

                    if self.format == document_service::FORMAT_RICH {
                        piecetable::set_cursor_pos(self.pt_buf_mut(), pos as u32);
                    }

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
                let tl = self.rich_text_len();

                if anchor <= tl && cursor <= tl {
                    self.sel_anchor = anchor;
                    self.cursor_pos = cursor;

                    if self.format == document_service::FORMAT_RICH {
                        piecetable::set_cursor_pos(self.pt_buf_mut(), cursor as u32);
                        piecetable::set_selection(self.pt_buf_mut(), anchor as u32, cursor as u32);
                    }

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

            document_service::STYLE_APPLY => {
                if self.format != document_service::FORMAT_RICH {
                    let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);

                    return;
                }

                if msg.payload.len() < document_service::StyleApplyRequest::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let req = document_service::StyleApplyRequest::read_from(msg.payload);

                piecetable::apply_style(
                    self.pt_buf_mut(),
                    req.start as u32,
                    req.end as u32,
                    req.style_id,
                );

                self.content_len = piecetable::total_size(self.pt_buf());

                self.persist_and_snapshot();
                self.write_header();
                self.reply_edit(msg);
            }

            document_service::GET_INFO => {
                let reply = document_service::InfoReply {
                    content_len: self.content_len as u64,
                    cursor_pos: self.cursor_pos as u64,
                    format: self.format,
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
    let store_vmo = abi::vmo::create(STORE_SHARED_SIZE, 0).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_SHARED_VMO_CREATE);
    });
    let store_shared_va =
        abi::vmo::map(store_vmo, 0, Rights::READ_WRITE_MAP).unwrap_or_else(|_| {
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
        let media = b"text/rich";
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
    let doc_va = abi::vmo::map(doc_vmo, 0, Rights::READ_WRITE_MAP).unwrap_or_else(|_| {
        abi::thread::exit(EXIT_DOC_VMO_MAP);
    });
    // Initialize piece table with the style stress test document.
    // 32 styles exercising all axes: 3 font families, sizes 10–48pt,
    // weights 100–900, italic/underline/strikethrough, 7 vivid colors.
    let content_buf = unsafe {
        core::slice::from_raw_parts_mut(
            (doc_va + document_service::DOC_HEADER_SIZE) as *mut u8,
            DOC_BUF_SIZE - document_service::DOC_HEADER_SIZE,
        )
    };

    piecetable::init(content_buf, content_buf.len());

    use piecetable::{
        FLAG_ITALIC, FLAG_STRIKETHROUGH, FLAG_UNDERLINE, FONT_MONO, FONT_SANS, FONT_SERIF,
        ROLE_BODY, ROLE_CODE, ROLE_EMPHASIS, ROLE_HEADING1, ROLE_HEADING2, ROLE_HEADING3,
        ROLE_STRONG, Style,
    };

    // Style 0: body (Sans 14pt Regular Black)
    piecetable::add_style(content_buf, &piecetable::default_body_style());
    // Style 1: Title — Sans 48pt Bold Red
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_HEADING1,
            weight: 700,
            flags: 0,
            font_size_pt: 48,
            color: [0xFF, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 2: Subtitle — Serif 24pt Regular Blue
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SERIF,
            role: ROLE_HEADING2,
            weight: 400,
            flags: 0,
            font_size_pt: 24,
            color: [0x00, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 3: Sans 36pt Bold Green
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 700,
            flags: 0,
            font_size_pt: 36,
            color: [0x00, 0xAA, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 4: Mono 10pt Regular Orange
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_MONO,
            role: ROLE_CODE,
            weight: 400,
            flags: 0,
            font_size_pt: 10,
            color: [0xFF, 0x88, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 5: Serif 18pt Italic Purple
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SERIF,
            role: ROLE_EMPHASIS,
            weight: 400,
            flags: FLAG_ITALIC,
            font_size_pt: 18,
            color: [0x88, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 6: Mono 16pt Regular Cyan
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_MONO,
            role: ROLE_CODE,
            weight: 400,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0xCC, 0xCC, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 7: Sans 20pt Bold Magenta
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_STRONG,
            weight: 700,
            flags: 0,
            font_size_pt: 20,
            color: [0xFF, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 8: Serif 14pt Italic Red
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SERIF,
            role: ROLE_EMPHASIS,
            weight: 400,
            flags: FLAG_ITALIC,
            font_size_pt: 14,
            color: [0xFF, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Styles 9–15: Sans 14pt Regular in rainbow colors
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0xFF, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0x00, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0x00, 0xAA, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0xFF, 0x88, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0x88, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0x00, 0xCC, 0xCC, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 14,
            color: [0xFF, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    // Styles 16–24: Sans 16pt weight ramp 100–900
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 100,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 200,
            flags: 0,
            font_size_pt: 16,
            color: [0x22, 0x22, 0x22, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 300,
            flags: 0,
            font_size_pt: 16,
            color: [0x44, 0x44, 0x44, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 500,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 600,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_STRONG,
            weight: 700,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 800,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_BODY,
            weight: 900,
            flags: 0,
            font_size_pt: 16,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 25: Serif 22pt Bold Italic Underline — deep blue
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SERIF,
            role: ROLE_HEADING3,
            weight: 700,
            flags: FLAG_ITALIC | FLAG_UNDERLINE,
            font_size_pt: 22,
            color: [0x00, 0x44, 0xCC, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 26: Mono 12pt Italic Strikethrough — dark red
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_MONO,
            role: ROLE_CODE,
            weight: 400,
            flags: FLAG_ITALIC | FLAG_STRIKETHROUGH,
            font_size_pt: 12,
            color: [0xCC, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 27: Sans 28pt Bold Green
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_HEADING2,
            weight: 700,
            flags: 0,
            font_size_pt: 28,
            color: [0x00, 0xAA, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 28: Serif 12pt Regular Black
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SERIF,
            role: ROLE_BODY,
            weight: 400,
            flags: 0,
            font_size_pt: 12,
            color: [0x00, 0x00, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 29: Mono 14pt Bold Cyan
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_MONO,
            role: ROLE_CODE,
            weight: 700,
            flags: 0,
            font_size_pt: 14,
            color: [0x00, 0xCC, 0xCC, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 30: Sans 40pt Italic Magenta
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SANS,
            role: ROLE_HEADING1,
            weight: 400,
            flags: FLAG_ITALIC,
            font_size_pt: 40,
            color: [0xFF, 0x00, 0xFF, 0xFF],
            _pad: [0; 2],
        },
    );
    // Style 31: Serif 32pt Bold Underline Orange
    piecetable::add_style(
        content_buf,
        &Style {
            font_family: FONT_SERIF,
            role: ROLE_HEADING1,
            weight: 700,
            flags: FLAG_UNDERLINE,
            font_size_pt: 32,
            color: [0xFF, 0x88, 0x00, 0xFF],
            _pad: [0; 2],
        },
    );

    // Build the stress test text with tracked byte ranges for style application.
    // Each tuple: (start_byte, end_byte, style_id).
    let mut text: [u8; 1024] = [0; 1024];
    let mut pos: usize = 0;
    let mut ranges: [(usize, usize, u8); 48] = [(0, 0, 0); 48];
    let mut ri: usize = 0;

    macro_rules! span {
        ($style:expr, $s:expr) => {{
            let s = pos;
            let bytes = $s.as_bytes();
            text[pos..pos + bytes.len()].copy_from_slice(bytes);
            pos += bytes.len();
            ranges[ri] = (s, pos, $style);
            ri += 1;
        }};
    }
    macro_rules! plain {
        ($s:expr) => {{
            let bytes = $s.as_bytes();
            text[pos..pos + bytes.len()].copy_from_slice(bytes);
            pos += bytes.len();
        }};
    }

    // Line 1: Title
    span!(1, "Style Stress Test");
    plain!("\n");
    // Line 2: Subtitle
    span!(2, "32 Styles, 3 Fonts, 9 Weights, Vivid Colors");
    plain!("\n");
    // Line 3: Baseline alignment — mixed sizes
    span!(3, "Sans 36pt Bold Green");
    span!(4, " Mono 10pt Orange");
    span!(5, " Serif 18pt Italic Purple");
    plain!("\n");
    // Line 4: More mixed sizes
    span!(1, "Sans 48pt Red");
    plain!(" body 14pt");
    span!(6, " Mono 16pt Cyan");
    plain!("\n");
    // Line 5: Font family showcase
    span!(7, "Sans 20pt Bold Magenta");
    span!(8, " Serif 14pt Italic Red");
    span!(6, " Mono 16pt Cyan");
    plain!("\n");
    // Line 6: Color parade
    span!(9, "Red");
    span!(10, " Blue");
    span!(11, " Green");
    span!(12, " Orange");
    span!(13, " Purple");
    span!(14, " Cyan");
    span!(15, " Magenta");
    plain!("\n");
    // Line 7: Weight ramp (100–900)
    span!(16, "Thin ");
    span!(17, "ExLight ");
    span!(18, "Light ");
    span!(19, "Regular ");
    span!(20, "Medium ");
    span!(21, "SemiBold ");
    span!(22, "Bold ");
    span!(23, "ExBold ");
    span!(24, "Black");
    plain!("\n");
    // Line 8: Flag combinations
    span!(25, "Serif 22pt Bold Italic Underline Blue");
    span!(26, " Mono 12pt Italic Strike Red");
    plain!("\n");
    // Line 9: More size mixing
    span!(27, "Sans 28pt Bold Green");
    span!(28, " Serif 12pt Regular");
    span!(29, " Mono 14pt Bold Cyan");
    plain!("\n");
    // Line 10: Large italic
    span!(30, "Sans 40pt Italic Magenta");
    plain!("\n");
    // Line 11: Large bold underline
    span!(31, "Serif 32pt Bold Underline Orange");
    plain!("\n");
    // Line 12: Mixed paragraph with inline styles
    plain!("This is body text with ");
    span!(7, "bold magenta");
    plain!(" and ");
    span!(8, "italic red");
    plain!(" and ");
    span!(6, "mono cyan");
    plain!(" inline.\n");

    piecetable::insert_bytes(content_buf, 0, &text[..pos]);

    for &(start, end, style_id) in &ranges[..ri] {
        piecetable::apply_style(content_buf, start as u32, end as u32, style_id);
    }

    let content_len = piecetable::total_size(content_buf);

    console::write(console_ep, b"document: rich text initialized\n");

    // Take initial undo snapshot.
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
        content_len,
        cursor_pos: 0,
        sel_anchor: 0,
        generation: 0,
        format: document_service::FORMAT_RICH,
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
