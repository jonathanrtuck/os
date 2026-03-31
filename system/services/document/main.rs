//! Document service — sole owner of the document buffer.
//!
//! Applies all edits (insert, delete, style) from editors via IPC.
//! Manages the undo ring: COW snapshots at edit boundaries.
//! Communicates with the document service (persistence, queries, snapshots).
//! Communicates with decoder services (image decode into Content Region).
//! Signals layout + presenter when the buffer changes so it can re-layout and re-render.
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: document <-> editor (receives write operations from editor)
//! Handle 2: document <-> store service (persistence, snapshot, restore)
//! Handle 3: document <-> decoder (image decode requests/responses)
//! Handle 4: document <-> presenter (sends doc-changed notifications, receives undo/redo requests)

#![no_std]
#![no_main]

extern crate alloc;
extern crate piecetable;

use protocol::{
    edit::{
        self, MSG_CURSOR_MOVE, MSG_SELECTION_UPDATE, MSG_STYLE_APPLY, MSG_STYLE_SET_CURRENT,
        MSG_WRITE_DELETE, MSG_WRITE_DELETE_RANGE, MSG_WRITE_INSERT,
    },
    init::{DocConfig, MSG_DOC_CONFIG},
    store::{
        StoreCommit, StoreCreate, StoreCreateResult, StoreDeleteSnapshot, StoreQuery,
        StoreQueryResult, StoreRead, StoreReadDone, StoreRestore, StoreSnapshot,
        StoreSnapshotResult, MSG_STORE_COMMIT, MSG_STORE_CREATE, MSG_STORE_CREATE_RESULT,
        MSG_STORE_DELETE_SNAPSHOT, MSG_STORE_QUERY, MSG_STORE_QUERY_RESULT, MSG_STORE_READ,
        MSG_STORE_READ_DONE, MSG_STORE_RESTORE, MSG_STORE_RESTORE_RESULT, MSG_STORE_SNAPSHOT,
        MSG_STORE_SNAPSHOT_RESULT,
    },
    view::{
        DocChanged, DocLoaded, ImageDecoded, DOC_CHANGED_CLEAR_SELECTION, MSG_DOC_CHANGED,
        MSG_DOC_LOADED, MSG_IMAGE_DECODED, MSG_REDO_REQUEST, MSG_UNDO_REQUEST,
    },
};

const DOC_HEADER_SIZE: usize = 64;
const MAX_UNDO: usize = 64;

/// Undo coalescing window: edits within this window are grouped into one undo step.
const COALESCE_MS: u64 = 500;

use protocol::edit::DocumentFormat;

/// Undo/redo state: fixed-size ring of COW snapshot IDs.
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
            let dst = &mut self.snapshots;
            for i in 0..MAX_UNDO - 1 {
                dst[i] = dst[i + 1];
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
}

/// PNG decode sub-state.
#[derive(Clone, Copy)]
enum DecodePhase {
    None,
    AwaitingHeader,
    AwaitingDecode { alloc_offset: u32, pixel_bytes: u32 },
    Done,
}

/// Document loading sub-state.
#[derive(Clone, Copy)]
enum DocPhase {
    QueryRich,
    QueryPlain,
    Reading {
        file_id: u64,
        detected_format: DocumentFormat,
    },
    Creating,
    AwaitingUndo,
    Done,
}

// ── Mutable state ───────────────────────────────────────────────────

struct DocModelState {
    doc_buf: *mut u8,
    doc_capacity: usize,
    doc_len: usize,
    doc_file_id: u64,
    doc_format: DocumentFormat,
    cursor_pos: usize,
    content_va: usize,
    content_size: usize,
    content_alloc: protocol::content::ContentAllocator,
    editor_handle: sys::ChannelHandle,
    decoder_handle: sys::ChannelHandle,
    fs_handle: sys::ChannelHandle,
    core_handle: sys::ChannelHandle,
}

// SAFETY: DocModelState is only accessed from the single-threaded event loop.
// The raw pointer fields (doc_buf) point to shared memory mapped by the kernel.
unsafe impl Send for DocModelState {}
unsafe impl Sync for DocModelState {}

static mut STATE: DocModelState = DocModelState {
    doc_buf: core::ptr::null_mut(),
    doc_capacity: 0,
    doc_len: 0,
    doc_file_id: 0,
    doc_format: DocumentFormat::Plain,
    cursor_pos: 0,
    content_va: 0,
    content_size: 0,
    content_alloc: protocol::content::ContentAllocator::empty(),
    editor_handle: sys::ChannelHandle(u8::MAX),
    decoder_handle: sys::ChannelHandle(u8::MAX),
    fs_handle: sys::ChannelHandle(u8::MAX),
    core_handle: sys::ChannelHandle(u8::MAX),
};

fn state() -> &'static mut DocModelState {
    // SAFETY: single-threaded, no reentrancy.
    unsafe { &mut STATE }
}

// ── Document buffer operations (plain text) ─────────────────────────

fn doc_content() -> &'static [u8] {
    let s = state();
    // SAFETY: doc_buf valid, doc_len <= doc_capacity.
    unsafe { core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), s.doc_len) }
}

fn doc_insert(pos: usize, byte: u8) -> bool {
    let s = state();
    if s.doc_len >= s.doc_capacity || pos > s.doc_len {
        return false;
    }
    // SAFETY: doc_buf valid, bounds checked.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if pos < s.doc_len {
            core::ptr::copy(base.add(pos), base.add(pos + 1), s.doc_len - pos);
        }
        *base.add(pos) = byte;
    }
    s.doc_len += 1;
    doc_write_header();
    true
}

fn doc_delete(pos: usize) -> bool {
    let s = state();
    if s.doc_len == 0 || pos >= s.doc_len {
        return false;
    }
    // SAFETY: doc_buf valid, bounds checked.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if pos + 1 < s.doc_len {
            core::ptr::copy(base.add(pos + 1), base.add(pos), s.doc_len - pos - 1);
        }
    }
    s.doc_len -= 1;
    doc_write_header();
    true
}

fn doc_delete_range(start: usize, end: usize) -> bool {
    let s = state();
    if start >= end || start >= s.doc_len || end > s.doc_len {
        return false;
    }
    let del_count = end - start;
    // SAFETY: doc_buf valid, bounds checked.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if end < s.doc_len {
            core::ptr::copy(base.add(end), base.add(start), s.doc_len - end);
        }
    }
    s.doc_len -= del_count;
    doc_write_header();
    true
}

fn doc_write_header() {
    let s = state();
    // SAFETY: doc_buf valid.
    unsafe {
        core::ptr::write_volatile(s.doc_buf as *mut u64, s.doc_len as u64);
        core::ptr::write_volatile(s.doc_buf.add(8) as *mut u64, s.cursor_pos as u64);
    }
}

// ── Rich text operations ────────────────────────────────────────────

fn rich_buf() -> &'static mut [u8] {
    let s = state();
    let cap = s.doc_capacity.saturating_sub(DOC_HEADER_SIZE);
    // SAFETY: doc_buf valid.
    unsafe { core::slice::from_raw_parts_mut(s.doc_buf.add(DOC_HEADER_SIZE), cap) }
}

fn rich_buf_ref() -> &'static [u8] {
    let s = state();
    let cap = s.doc_capacity.saturating_sub(DOC_HEADER_SIZE);
    // SAFETY: doc_buf valid.
    unsafe { core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), cap) }
}

fn rich_total_size() -> usize {
    let buf = rich_buf_ref();
    let h = piecetable::header(buf);
    piecetable::HEADER_SIZE
        + h.style_count as usize * 12
        + h.piece_count as usize * 16
        + h.original_len as usize
        + h.add_len as usize
}

fn rich_sync_header() {
    let s = state();
    let total = rich_total_size();
    s.doc_len = total;
    // SAFETY: doc_buf valid.
    unsafe {
        core::ptr::write_volatile(s.doc_buf as *mut u64, total as u64);
        core::ptr::write_volatile(s.doc_buf.add(8) as *mut u64, s.cursor_pos as u64);
    }
}

fn rich_insert(pos: usize, byte: u8) -> bool {
    let buf = rich_buf();
    let ok = piecetable::insert(buf, pos as u32, byte);
    if ok {
        rich_sync_header();
    }
    ok
}

fn rich_delete(pos: usize) -> bool {
    let buf = rich_buf();
    let ok = piecetable::delete(buf, pos as u32);
    if ok {
        rich_sync_header();
    }
    ok
}

fn rich_delete_range(start: usize, end: usize) -> bool {
    let buf = rich_buf();
    let ok = piecetable::delete_range(buf, start as u32, end as u32);
    if ok {
        rich_sync_header();
    }
    ok
}

fn rich_apply_style(start: usize, end: usize, style_id: u8) {
    let buf = rich_buf();
    piecetable::apply_style(buf, start as u32, end as u32, style_id);
    rich_sync_header();
}

fn rich_set_current_style(style_id: u8) {
    let buf = rich_buf();
    piecetable::set_current_style(buf, style_id);
}

fn rich_text_len() -> usize {
    let buf = rich_buf_ref();
    piecetable::text_len(buf) as usize
}

fn rich_cursor_pos() -> usize {
    let buf = rich_buf_ref();
    piecetable::cursor_pos(buf) as usize
}

fn rich_set_cursor_pos(pos: usize) {
    let buf = rich_buf();
    piecetable::set_cursor_pos(buf, pos as u32);
}

fn rich_next_operation() -> u32 {
    let buf = rich_buf();
    piecetable::next_operation(buf)
}

// ── Notification helpers ────────────────────────────────────────────

fn notify_core_doc_changed(core_ch: &ipc::Channel, flags: u8) {
    let s = state();
    let payload = DocChanged {
        doc_len: s.doc_len as u32,
        cursor_pos: s.cursor_pos as u32,
        flags,
        _pad: [0; 3],
    };
    // SAFETY: DocChanged is repr(C) and fits in 60-byte payload.
    let msg = unsafe { ipc::Message::from_payload(MSG_DOC_CHANGED, &payload) };
    core_ch.send(&msg);
    let _ = sys::channel_signal(state().core_handle);
}

fn notify_core_doc_loaded(core_ch: &ipc::Channel) {
    let s = state();
    let payload = DocLoaded {
        doc_len: s.doc_len as u32,
        cursor_pos: s.cursor_pos as u32,
        doc_file_id: s.doc_file_id,
        format: if s.doc_format == DocumentFormat::Rich {
            1
        } else {
            0
        },
        _pad: [0; 3],
    };
    // SAFETY: DocLoaded is repr(C) and fits in 60-byte payload.
    let msg = unsafe { ipc::Message::from_payload(MSG_DOC_LOADED, &payload) };
    core_ch.send(&msg);
    let _ = sys::channel_signal(state().core_handle);
}

fn notify_core_image_decoded(core_ch: &ipc::Channel, content_id: u32, width: u16, height: u16) {
    let payload = ImageDecoded {
        content_id,
        width,
        height,
    };
    // SAFETY: ImageDecoded is repr(C) and fits in 60-byte payload.
    let msg = unsafe { ipc::Message::from_payload(MSG_IMAGE_DECODED, &payload) };
    core_ch.send(&msg);
    let _ = sys::channel_signal(state().core_handle);
}

// ── Snapshot helper ─────────────────────────────────────────────────

fn take_snapshot(fs_ch: &ipc::Channel, undo_state: &mut UndoState) {
    let file_id = state().doc_file_id;
    let snap_payload = StoreSnapshot {
        file_count: 1,
        _pad: 0,
        file_ids: [file_id, 0, 0, 0, 0, 0],
    };
    // SAFETY: StoreSnapshot is repr(C) and fits in 60-byte payload.
    let snap_msg = unsafe { ipc::Message::from_payload(MSG_STORE_SNAPSHOT, &snap_payload) };
    fs_ch.send(&snap_msg);
    let _ = sys::channel_signal(state().fs_handle);

    let mut reply = ipc::Message::new(0);
    if fs_ch.recv_blocking(state().fs_handle.0, &mut reply)
        && reply.msg_type == MSG_STORE_SNAPSHOT_RESULT
    {
        if let Some(protocol::store::Message::StoreSnapshotResult(result)) =
            protocol::store::decode(reply.msg_type, &reply.payload)
        {
            if result.status == 0 {
                let mut discarded = [0u64; MAX_UNDO];
                let n = undo_state.push(result.snapshot_id, &mut discarded);
                for &snap in &discarded[..n] {
                    let del = StoreDeleteSnapshot { snapshot_id: snap };
                    // SAFETY: StoreDeleteSnapshot is repr(C), fits in 60 bytes.
                    let del_msg =
                        unsafe { ipc::Message::from_payload(MSG_STORE_DELETE_SNAPSHOT, &del) };
                    fs_ch.send(&del_msg);
                }
                if n > 0 {
                    let _ = sys::channel_signal(state().fs_handle);
                }
            }
        }
    }
}

// ── Undo/redo implementation ────────────────────────────────────────

fn perform_undo(fs_ch: &ipc::Channel, core_ch: &ipc::Channel, undo_state: &mut UndoState) {
    if let Some(snap_id) = undo_state.undo() {
        let restore_payload = StoreRestore {
            snapshot_id: snap_id,
        };
        // SAFETY: StoreRestore is repr(C) and fits in 60-byte payload.
        let restore_msg =
            unsafe { ipc::Message::from_payload(MSG_STORE_RESTORE, &restore_payload) };
        fs_ch.send(&restore_msg);
        let _ = sys::channel_signal(state().fs_handle);

        let mut reply = ipc::Message::new(0);
        if fs_ch.recv_blocking(state().fs_handle.0, &mut reply)
            && reply.msg_type == MSG_STORE_RESTORE_RESULT
        {
            if let Some(protocol::store::Message::StoreRestoreResult(result)) =
                protocol::store::decode(reply.msg_type, &reply.payload)
            {
                if result.status == 0 {
                    reload_document(fs_ch);
                    notify_core_doc_changed(core_ch, DOC_CHANGED_CLEAR_SELECTION);
                }
            }
        }
    }
}

fn perform_redo(fs_ch: &ipc::Channel, core_ch: &ipc::Channel, undo_state: &mut UndoState) {
    if let Some(snap_id) = undo_state.redo() {
        let restore_payload = StoreRestore {
            snapshot_id: snap_id,
        };
        // SAFETY: StoreRestore is repr(C) and fits in 60-byte payload.
        let restore_msg =
            unsafe { ipc::Message::from_payload(MSG_STORE_RESTORE, &restore_payload) };
        fs_ch.send(&restore_msg);
        let _ = sys::channel_signal(state().fs_handle);

        let mut reply = ipc::Message::new(0);
        if fs_ch.recv_blocking(state().fs_handle.0, &mut reply)
            && reply.msg_type == MSG_STORE_RESTORE_RESULT
        {
            if let Some(protocol::store::Message::StoreRestoreResult(result)) =
                protocol::store::decode(reply.msg_type, &reply.payload)
            {
                if result.status == 0 {
                    reload_document(fs_ch);
                    notify_core_doc_changed(core_ch, DOC_CHANGED_CLEAR_SELECTION);
                }
            }
        }
    }
}

/// Re-read document content from the document service after undo/redo restore.
fn reload_document(fs_ch: &ipc::Channel) {
    let s = state();
    let read_payload = StoreRead {
        file_id: s.doc_file_id,
        target_va: 0,
        capacity: s.doc_capacity as u32,
        _pad: 0,
    };
    // SAFETY: StoreRead is repr(C) and fits in 60-byte payload.
    let read_msg = unsafe { ipc::Message::from_payload(MSG_STORE_READ, &read_payload) };
    fs_ch.send(&read_msg);
    let _ = sys::channel_signal(state().fs_handle);

    let mut reply = ipc::Message::new(0);
    if fs_ch.recv_blocking(state().fs_handle.0, &mut reply) && reply.msg_type == MSG_STORE_READ_DONE
    {
        if let Some(protocol::store::Message::StoreReadDone(done)) =
            protocol::store::decode(reply.msg_type, &reply.payload)
        {
            if done.status == 0 {
                let s = state();
                s.doc_len = done.len as usize;
                if s.doc_format == DocumentFormat::Rich {
                    let text_len = rich_text_len();
                    s.cursor_pos = rich_cursor_pos();
                    if s.cursor_pos > text_len {
                        s.cursor_pos = text_len;
                    }
                } else {
                    if s.cursor_pos > s.doc_len {
                        s.cursor_pos = s.doc_len;
                    }
                    doc_write_header();
                }
            }
        }
    }
}

// ── Boot: load document ─────────────────────────────────────────────

const BOOT_TIMEOUT_NS: u64 = 5_000_000_000;

/// Boot state machine: queries document service, reads document, takes initial snapshot.
fn boot_load_document(
    fs_ch: &ipc::Channel,
    decoder_ch: &ipc::Channel,
    core_ch: &ipc::Channel,
    undo_state: &mut UndoState,
    img_offset: u32,
    img_length: u32,
) {
    let mut msg = ipc::Message::new(0);

    // Start async decode if image present.
    let mut decode_phase = DecodePhase::None;
    let has_image = img_length > 0;
    if has_image {
        let hdr_req = protocol::decode::DecodeRequest {
            file_offset: img_offset,
            file_length: img_length,
            content_offset: 0,
            max_output: 0,
            request_id: 1,
            flags: protocol::decode::DECODE_FLAG_HEADER_ONLY,
        };
        // SAFETY: DecodeRequest is repr(C) and fits in 60-byte payload.
        let req_msg =
            unsafe { ipc::Message::from_payload(protocol::decode::MSG_DECODE_REQUEST, &hdr_req) };
        decoder_ch.send(&req_msg);
        let _ = sys::channel_signal(state().decoder_handle);
        decode_phase = DecodePhase::AwaitingHeader;
    }

    // Query for text/rich document.
    {
        let media = b"text/rich";
        let mut query_payload = StoreQuery {
            query_type: 0,
            data_len: media.len() as u32,
            data: [0u8; 48],
        };
        query_payload.data[..media.len()].copy_from_slice(media);
        // SAFETY: StoreQuery is repr(C) and fits in 60-byte payload.
        let query_msg = unsafe { ipc::Message::from_payload(MSG_STORE_QUERY, &query_payload) };
        fs_ch.send(&query_msg);
        let _ = sys::channel_signal(state().fs_handle);
    }

    let mut doc_phase = DocPhase::QueryRich;
    let mut image_ready = !has_image;
    let mut doc_ready = false;
    let mut undo_ready = false;

    let counter_freq = sys::counter_freq();
    let boot_start = sys::counter();
    let boot_timeout_ticks = if counter_freq > 0 {
        BOOT_TIMEOUT_NS as u128 * counter_freq as u128 / 1_000_000_000
    } else {
        u128::MAX
    } as u64;

    loop {
        let mut wait_handles = alloc::vec![state().fs_handle.0];
        if !image_ready {
            wait_handles.push(state().decoder_handle.0);
        }
        let _ = sys::wait(&wait_handles, 100_000_000); // 100ms poll

        // ── Decoder replies ─────────────────────────────────────────
        while !image_ready && decoder_ch.try_recv(&mut msg) {
            if let Some(protocol::decode::Message::Response(resp)) =
                protocol::decode::decode(msg.msg_type, &msg.payload)
            {
                match decode_phase {
                    DecodePhase::AwaitingHeader => {
                        if resp.status == protocol::decode::DecodeStatus::HeaderOk as u8
                            && resp.width > 0
                            && resp.height > 0
                        {
                            let pixel_bytes = resp.width as u32 * resp.height as u32 * 4;
                            let s = state();
                            if let Some(alloc_offset) = s.content_alloc.allocate(pixel_bytes) {
                                let dec_req = protocol::decode::DecodeRequest {
                                    file_offset: img_offset,
                                    file_length: img_length,
                                    content_offset: alloc_offset,
                                    max_output: pixel_bytes,
                                    request_id: 2,
                                    flags: 0,
                                };
                                let req_msg = unsafe {
                                    ipc::Message::from_payload(
                                        protocol::decode::MSG_DECODE_REQUEST,
                                        &dec_req,
                                    )
                                };
                                decoder_ch.send(&req_msg);
                                let _ = sys::channel_signal(state().decoder_handle);
                                decode_phase = DecodePhase::AwaitingDecode {
                                    alloc_offset,
                                    pixel_bytes,
                                };
                            } else {
                                image_ready = true;
                                decode_phase = DecodePhase::Done;
                            }
                        } else {
                            image_ready = true;
                            decode_phase = DecodePhase::Done;
                        }
                    }
                    DecodePhase::AwaitingDecode {
                        alloc_offset,
                        pixel_bytes: _,
                    } => {
                        if resp.status == protocol::decode::DecodeStatus::Ok as u8 {
                            let s = state();
                            // SAFETY: content_va is mapped read-write.
                            let header = unsafe {
                                &mut *(s.content_va as *mut protocol::content::ContentRegionHeader)
                            };
                            let entry_idx = header.entry_count as usize;
                            if entry_idx < protocol::content::MAX_CONTENT_ENTRIES {
                                let content_id = protocol::content::CONTENT_ID_DYNAMIC_START;
                                header.entries[entry_idx] = protocol::content::ContentEntry {
                                    content_id,
                                    offset: alloc_offset,
                                    length: resp.bytes_written,
                                    class: protocol::content::ContentClass::Pixels as u8,
                                    _pad: [0; 3],
                                    width: resp.width as u16,
                                    height: resp.height as u16,
                                    generation: 0,
                                };
                                header.entry_count += 1;
                                notify_core_image_decoded(
                                    core_ch,
                                    content_id,
                                    resp.width as u16,
                                    resp.height as u16,
                                );
                                sys::print(b"     PNG decoded into Content Region\n");
                            }
                        } else {
                            sys::print(b"     PNG decode failed\n");
                        }
                        image_ready = true;
                        decode_phase = DecodePhase::Done;
                    }
                    _ => {}
                }
            }
        }

        // ── Document service replies ────────────────────────────────
        while fs_ch.try_recv(&mut msg) {
            match doc_phase {
                DocPhase::QueryRich => {
                    if let Some(protocol::store::Message::StoreQueryResult(result)) =
                        protocol::store::decode(msg.msg_type, &msg.payload)
                    {
                        if result.count > 0 {
                            let s = state();
                            let read_payload = StoreRead {
                                file_id: result.file_ids[0],
                                target_va: 0,
                                capacity: s.doc_capacity as u32,
                                _pad: 0,
                            };
                            let read_msg = unsafe {
                                ipc::Message::from_payload(MSG_STORE_READ, &read_payload)
                            };
                            fs_ch.send(&read_msg);
                            let _ = sys::channel_signal(state().fs_handle);
                            doc_phase = DocPhase::Reading {
                                file_id: result.file_ids[0],
                                detected_format: DocumentFormat::Rich,
                            };
                        } else {
                            let media = b"text/plain";
                            let mut query_payload = StoreQuery {
                                query_type: 0,
                                data_len: media.len() as u32,
                                data: [0u8; 48],
                            };
                            query_payload.data[..media.len()].copy_from_slice(media);
                            let query_msg = unsafe {
                                ipc::Message::from_payload(MSG_STORE_QUERY, &query_payload)
                            };
                            fs_ch.send(&query_msg);
                            let _ = sys::channel_signal(state().fs_handle);
                            doc_phase = DocPhase::QueryPlain;
                        }
                    }
                }
                DocPhase::QueryPlain => {
                    if let Some(protocol::store::Message::StoreQueryResult(result)) =
                        protocol::store::decode(msg.msg_type, &msg.payload)
                    {
                        if result.count > 0 {
                            let s = state();
                            let read_payload = StoreRead {
                                file_id: result.file_ids[0],
                                target_va: 0,
                                capacity: s.doc_capacity as u32,
                                _pad: 0,
                            };
                            let read_msg = unsafe {
                                ipc::Message::from_payload(MSG_STORE_READ, &read_payload)
                            };
                            fs_ch.send(&read_msg);
                            let _ = sys::channel_signal(state().fs_handle);
                            doc_phase = DocPhase::Reading {
                                file_id: result.file_ids[0],
                                detected_format: DocumentFormat::Plain,
                            };
                        } else {
                            sys::print(b"     creating new text document\n");
                            let media = b"text/plain";
                            let mut create_payload = StoreCreate {
                                media_type_len: media.len() as u32,
                                _pad: 0,
                                media_type: [0u8; 52],
                            };
                            create_payload.media_type[..media.len()].copy_from_slice(media);
                            let create_msg = unsafe {
                                ipc::Message::from_payload(MSG_STORE_CREATE, &create_payload)
                            };
                            fs_ch.send(&create_msg);
                            let _ = sys::channel_signal(state().fs_handle);
                            doc_phase = DocPhase::Creating;
                        }
                    }
                }
                DocPhase::Reading {
                    file_id,
                    detected_format,
                } => {
                    if let Some(protocol::store::Message::StoreReadDone(done)) =
                        protocol::store::decode(msg.msg_type, &msg.payload)
                    {
                        if done.status == 0 && done.len > 0 {
                            let s = state();
                            s.doc_file_id = file_id;
                            // SAFETY: doc_buf valid, done.len <= capacity.
                            let content_start = unsafe {
                                core::slice::from_raw_parts(
                                    s.doc_buf.add(DOC_HEADER_SIZE),
                                    done.len as usize,
                                )
                            };
                            if detected_format == DocumentFormat::Rich
                                && done.len >= 64
                                && piecetable::validate(content_start)
                            {
                                s.doc_format = DocumentFormat::Rich;
                                s.doc_len = done.len as usize;
                                s.cursor_pos = rich_cursor_pos();
                                doc_write_header();
                                sys::print(b"     rich text document loaded\n");
                            } else {
                                s.doc_format = DocumentFormat::Plain;
                                s.doc_len = done.len as usize;
                                doc_write_header();
                                sys::print(b"     plain text document loaded\n");
                            }
                        } else {
                            state().doc_file_id = file_id;
                            state().doc_format = detected_format;
                        }
                    }
                    doc_ready = true;
                    if state().doc_file_id != 0 {
                        let snap_payload = StoreSnapshot {
                            file_count: 1,
                            _pad: 0,
                            file_ids: [state().doc_file_id, 0, 0, 0, 0, 0],
                        };
                        let snap_msg = unsafe {
                            ipc::Message::from_payload(MSG_STORE_SNAPSHOT, &snap_payload)
                        };
                        fs_ch.send(&snap_msg);
                        let _ = sys::channel_signal(state().fs_handle);
                        doc_phase = DocPhase::AwaitingUndo;
                    } else {
                        undo_ready = true;
                        doc_phase = DocPhase::Done;
                    }
                }
                DocPhase::Creating => {
                    if let Some(protocol::store::Message::StoreCreateResult(result)) =
                        protocol::store::decode(msg.msg_type, &msg.payload)
                    {
                        if result.status == 0 {
                            state().doc_file_id = result.file_id;
                            state().doc_format = DocumentFormat::Plain;
                            sys::print(b"     text document created\n");
                        } else {
                            sys::print(b"     warning: document create failed\n");
                        }
                    }
                    doc_ready = true;
                    if state().doc_file_id != 0 {
                        let snap_payload = StoreSnapshot {
                            file_count: 1,
                            _pad: 0,
                            file_ids: [state().doc_file_id, 0, 0, 0, 0, 0],
                        };
                        let snap_msg = unsafe {
                            ipc::Message::from_payload(MSG_STORE_SNAPSHOT, &snap_payload)
                        };
                        fs_ch.send(&snap_msg);
                        let _ = sys::channel_signal(state().fs_handle);
                        doc_phase = DocPhase::AwaitingUndo;
                    } else {
                        undo_ready = true;
                        doc_phase = DocPhase::Done;
                    }
                }
                DocPhase::AwaitingUndo => {
                    if let Some(protocol::store::Message::StoreSnapshotResult(result)) =
                        protocol::store::decode(msg.msg_type, &msg.payload)
                    {
                        if result.status == 0 {
                            undo_state.set_initial(result.snapshot_id);
                            sys::print(b"     initial undo snapshot taken\n");
                        }
                    }
                    undo_ready = true;
                    doc_phase = DocPhase::Done;
                }
                DocPhase::Done => {}
            }
        }

        if image_ready && doc_ready && undo_ready {
            sys::print(b"     boot complete\n");
            break;
        }

        if sys::counter() - boot_start > boot_timeout_ticks {
            sys::print(b"     boot timeout - proceeding\n");
            break;
        }
    }

    // Notify core that the document is loaded.
    notify_core_doc_loaded(core_ch);
}

// ── Entry point ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x84 document starting\n");

    // Read config from init channel.
    // SAFETY: channel_shm_va(0) is the init channel SHM region.
    let init_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_DOC_CONFIG {
        sys::print(b"document: no config message\n");
        sys::exit();
    }

    let Some(protocol::init::DocMessage::DocConfig(config)) =
        protocol::init::decode_doc(msg.msg_type, &msg.payload)
    else {
        sys::print(b"document: bad config payload\n");
        sys::exit();
    };

    if config.doc_va == 0 {
        sys::print(b"document: bad config\n");
        sys::exit();
    }

    {
        let s = state();
        s.doc_buf = config.doc_va as *mut u8;
        s.doc_capacity = config.doc_capacity as usize;
        s.doc_len = 0;
        s.content_va = config.content_va as usize;
        s.content_size = config.content_size as usize;
        s.editor_handle = sys::ChannelHandle(config.editor_handle);
        s.decoder_handle = sys::ChannelHandle(config.decoder_handle);
        s.fs_handle = sys::ChannelHandle(config.fs_handle);
        s.core_handle = sys::ChannelHandle(config.core_handle);
        if config.content_va != 0 && config.content_size > 0 {
            // SAFETY: content_va is mapped read-write.
            let header =
                unsafe { &*(config.content_va as *const protocol::content::ContentRegionHeader) };
            s.content_alloc = protocol::content::ContentAllocator::new(
                header.next_alloc,
                config.content_size as u32,
            );
        }
    }
    doc_write_header();

    sys::print(b"     config received\n");

    // Set up IPC channels.
    // SAFETY: channel_shm_va(N) are bases of channel SHM regions mapped by kernel.
    //
    // Endpoint assignment: init sends endpoint A (index 0) of each channel
    // to document, except for the presenter↔document channel where document receives endpoint B (index 1).
    // The endpoint parameter must match the ChannelId endpoint index.
    let editor_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().editor_handle.0 as usize),
            ipc::PAGE_SIZE,
            0, // document receives endpoint A (index 0) of editor↔document channel
        )
    };
    let fs_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().fs_handle.0 as usize),
            ipc::PAGE_SIZE,
            0, // document receives endpoint A (index 0) of document↔store channel
        )
    };
    let decoder_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().decoder_handle.0 as usize),
            ipc::PAGE_SIZE,
            0, // document receives endpoint A (index 0) of document↔decoder channel
        )
    };
    let core_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().core_handle.0 as usize),
            ipc::PAGE_SIZE,
            1, // document receives endpoint B (index 1) of presenter↔document channel
        )
    };

    // ── Boot: load document and decode image ────────────────────────
    let mut undo_state = UndoState::new();
    boot_load_document(
        &fs_ch,
        &decoder_ch,
        &core_ch,
        &mut undo_state,
        config.img_file_store_offset,
        config.img_file_store_length,
    );

    sys::print(b"     entering event loop\n");

    // ── Main event loop ─────────────────────────────────────────────
    let counter_freq = sys::counter_freq();
    let mut snapshot_pending = false;
    let mut last_edit_ms: u64 = 0;

    loop {
        let now_ms = if counter_freq > 0 {
            sys::counter() * 1000 / counter_freq
        } else {
            0
        };

        // Compute timeout: if snapshot pending, wake for coalesce deadline.
        let timeout_ns = if snapshot_pending {
            let snap_deadline_ms = last_edit_ms + COALESCE_MS;
            let snap_remaining_ms = snap_deadline_ms.saturating_sub(now_ms);
            snap_remaining_ms.saturating_mul(1_000_000).max(1_000_000)
        } else {
            u64::MAX
        };

        let _ = sys::wait(
            &[state().editor_handle.0, state().core_handle.0],
            timeout_ns,
        );

        let now_ms = if counter_freq > 0 {
            sys::counter() * 1000 / counter_freq
        } else {
            0
        };

        let mut text_changed = false;

        // ── Process editor write requests ───────────────────────────
        let is_rich = state().doc_format == DocumentFormat::Rich;
        while editor_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_WRITE_INSERT => {
                    let Some(edit::Message::WriteInsert(insert)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let pos = insert.position as usize;
                    let ok = if is_rich {
                        rich_insert(pos, insert.byte)
                    } else {
                        doc_insert(pos, insert.byte)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = pos + 1;
                        if is_rich {
                            rich_set_cursor_pos(s.cursor_pos);
                        }
                        text_changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    let Some(edit::Message::WriteDelete(del)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let pos = del.position as usize;
                    let ok = if is_rich {
                        rich_delete(pos)
                    } else {
                        doc_delete(pos)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = pos;
                        if is_rich {
                            rich_set_cursor_pos(s.cursor_pos);
                        }
                        text_changed = true;
                    }
                }
                MSG_WRITE_DELETE_RANGE => {
                    let Some(edit::Message::WriteDeleteRange(dr)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let start = dr.start as usize;
                    let end = dr.end as usize;
                    let ok = if is_rich {
                        rich_delete_range(start, end)
                    } else {
                        doc_delete_range(start, end)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = start;
                        if is_rich {
                            rich_set_cursor_pos(s.cursor_pos);
                        }
                        text_changed = true;
                    }
                }
                MSG_CURSOR_MOVE => {
                    let Some(edit::Message::CursorMove(cm)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let pos = cm.position as usize;
                    let doc_text_len = if is_rich {
                        rich_text_len()
                    } else {
                        state().doc_len
                    };
                    if pos <= doc_text_len {
                        state().cursor_pos = pos;
                        if is_rich {
                            rich_set_cursor_pos(pos);
                        } else {
                            doc_write_header();
                        }
                    }
                }
                MSG_SELECTION_UPDATE => {
                    // Selection state is managed by core — just forward.
                }
                MSG_STYLE_APPLY => {
                    if !is_rich {
                        continue;
                    }
                    let Some(edit::Message::StyleApply(sa)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    rich_apply_style(sa.start as usize, sa.end as usize, sa.style_id);
                    text_changed = true;
                }
                MSG_STYLE_SET_CURRENT => {
                    if !is_rich {
                        continue;
                    }
                    let Some(edit::Message::StyleSetCurrent(sc)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    rich_set_current_style(sc.style_id);
                }
                _ => {}
            }
        }

        // ── Process core requests (undo/redo + delete range) ─────────
        let mut undo_requested = false;
        let mut redo_requested = false;
        while core_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_UNDO_REQUEST => {
                    undo_requested = true;
                }
                MSG_REDO_REQUEST => {
                    redo_requested = true;
                }
                // Core forwards selection/word delete as MSG_WRITE_DELETE_RANGE.
                MSG_WRITE_DELETE_RANGE => {
                    let Some(edit::Message::WriteDeleteRange(dr)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let start = dr.start as usize;
                    let end = dr.end as usize;
                    let ok = if is_rich {
                        rich_delete_range(start, end)
                    } else {
                        doc_delete_range(start, end)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = start;
                        if is_rich {
                            rich_set_cursor_pos(s.cursor_pos);
                        }
                        text_changed = true;
                    }
                }
                _ => {}
            }
        }

        // ── Persist + snapshot ──────────────────────────────────────
        if text_changed {
            let file_id = state().doc_file_id;
            let commit_payload = StoreCommit { file_id };
            // SAFETY: StoreCommit is repr(C) and fits in 60-byte payload.
            let commit_msg =
                unsafe { ipc::Message::from_payload(MSG_STORE_COMMIT, &commit_payload) };
            fs_ch.send(&commit_msg);
            let _ = sys::channel_signal(state().fs_handle);

            snapshot_pending = true;
            last_edit_ms = now_ms;

            // Notify core that the document buffer changed.
            notify_core_doc_changed(&core_ch, 0);
        }

        // Flush pending snapshot on typing pause.
        if snapshot_pending && !text_changed && now_ms.saturating_sub(last_edit_ms) >= COALESCE_MS {
            if state().doc_format == DocumentFormat::Rich {
                rich_next_operation();
            }
            take_snapshot(&fs_ch, &mut undo_state);
            snapshot_pending = false;
        }

        // Flush pending snapshot before undo/redo.
        if snapshot_pending && (undo_requested || redo_requested) {
            if state().doc_format == DocumentFormat::Rich {
                rich_next_operation();
            }
            take_snapshot(&fs_ch, &mut undo_state);
            snapshot_pending = false;
        }

        if undo_requested {
            perform_undo(&fs_ch, &core_ch, &mut undo_state);
        } else if redo_requested {
            perform_redo(&fs_ch, &core_ch, &mut undo_state);
        }
    }
}
