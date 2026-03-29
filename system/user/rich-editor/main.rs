//! Rich text editor — content-type-specific input-to-write translator.
//!
//! Handles text/rich documents backed by a piece table. Receives editing
//! key events from core and translates them into document write requests.
//! Navigation, selection, and style shortcuts (Cmd+B/I) are handled by core.
//!
//! This editor mirrors the text-editor for basic editing (insert, delete, tab)
//! but reads document content through the piecetable library instead of the
//! flat buffer header, since the shared memory contains a serialized piece table.
//!
//! # Shared memory
//!
//! Document buffer (read-only mapping from init):
//!   [0..8):   content_len (u64, written by core — piece table total size)
//!   [8..16):  cursor_pos  (u64, written by core)
//!   [64..N):  piece table bytes (header + styles + pieces + text buffers)

#![no_std]
#![no_main]

extern crate piecetable;

use protocol::{
    edit::{
        CursorMove, WriteDelete, WriteDeleteRange, WriteInsert, MSG_CURSOR_MOVE, MSG_WRITE_DELETE,
        MSG_WRITE_DELETE_RANGE, MSG_WRITE_INSERT,
    },
    input::MOD_SHIFT,
};

const DOC_HEADER_SIZE: usize = 64;
// Keycodes (Linux evdev).
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_DELETE: u16 = 111;

fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

/// Read the piece table buffer from the shared document memory.
/// The piece table starts at DOC_HEADER_SIZE offset in the doc buffer.
fn pt_buf(doc_buf: *const u8, capacity: usize) -> &'static [u8] {
    let cap = capacity.saturating_sub(DOC_HEADER_SIZE);
    // SAFETY: doc_buf + DOC_HEADER_SIZE points to piece table bytes.
    // The capacity is set by init and the buffer is valid shared memory.
    unsafe { core::slice::from_raw_parts(doc_buf.add(DOC_HEADER_SIZE), cap) }
}

/// Check if the document buffer contains a valid piece table.
fn is_rich(doc_buf: *const u8, capacity: usize) -> bool {
    if capacity < DOC_HEADER_SIZE + 64 {
        return false;
    }
    let buf = pt_buf(doc_buf, capacity);
    piecetable::validate(buf)
}

/// Read the document length from the shared buffer header (flat path).
fn doc_len(doc_buf: *const u8) -> usize {
    // SAFETY: doc_buf offset 0 holds content_len as u64, written by core.
    unsafe { core::ptr::read_volatile(doc_buf as *const u64) as usize }
}

/// Read flat document content (text/plain fallback).
fn doc_content(doc_buf: *const u8, len: usize) -> &'static [u8] {
    // SAFETY: doc_buf + DOC_HEADER_SIZE is the start of content bytes.
    unsafe { core::slice::from_raw_parts(doc_buf.add(DOC_HEADER_SIZE), len) }
}

/// Find the start of the line containing `pos` (previous '\n' + 1, or 0).
fn line_start_flat(text: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i > 0 && text[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

/// Find line start in a piece table document using byte_at lookups.
fn line_start_rich(buf: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i > 0 {
        if let Some(b'\n') = piecetable::byte_at(buf, (i - 1) as u32) {
            break;
        }
        i -= 1;
    }
    i
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x9D rich-editor starting\n");

    // Handle 0: init config channel — read editor config.
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    let config = if init_ch.try_recv(&mut msg) {
        protocol::init::decode_editor(msg.msg_type, &msg.payload)
    } else {
        None
    };
    let Some(protocol::init::EditorMessage::EditorConfig(config)) = config else {
        sys::print(b"rich-editor: no config message\n");
        sys::exit();
    };
    let doc_buf = config.doc_va as *const u8;
    let doc_capacity = config.doc_capacity as usize;

    if doc_buf.is_null() {
        sys::print(b"rich-editor: no document buffer\n");
        sys::exit();
    }

    sys::print(b"     document buffer mapped (read-only)\n");

    // Handle 1: OS service (core) — receives key events, cursor sync.
    let os_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Handle 2: Document-model (A) — sends write operations.
    let docmodel_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 1) };

    const OS_HANDLE: u8 = 1;
    const DOCMODEL_HANDLE: u8 = 2;

    // Editor-local cursor position (byte offset in document).
    // Synced from core via MSG_SET_CURSOR.
    let mut cursor: usize = 0;

    sys::print(b"     entering event loop\n");

    loop {
        let _ = sys::wait(&[OS_HANDLE], u64::MAX);

        while os_ch.try_recv(&mut msg) {
            // Cursor sync from core (navigation, click, selection-delete).
            if let Some(protocol::edit::Message::SetCursor(cm)) =
                protocol::edit::decode(msg.msg_type, &msg.payload)
            {
                cursor = cm.position as usize;
                continue;
            }

            let key = match protocol::input::decode(msg.msg_type, &msg.payload) {
                Some(protocol::input::Message::KeyEvent(k)) => k,
                _ => continue,
            };

            if key.pressed != 1 {
                continue;
            }

            // Determine text length based on document format.
            let rich = is_rich(doc_buf, doc_capacity);
            let len = if rich {
                let buf = pt_buf(doc_buf, doc_capacity);
                piecetable::text_len(buf) as usize
            } else {
                doc_len(doc_buf)
            };

            let shift = key.modifiers & MOD_SHIFT != 0;

            match key.keycode {
                // ── Backspace: delete character before cursor ────
                KEY_BACKSPACE => {
                    if cursor > 0 {
                        cursor -= 1;
                        let del = WriteDelete {
                            position: cursor as u32,
                        };
                        let del_msg = unsafe { ipc::Message::from_payload(MSG_WRITE_DELETE, &del) };
                        docmodel_ch.send(&del_msg);
                        let _ = sys::channel_signal(sys::ChannelHandle(DOCMODEL_HANDLE));
                    }
                }

                // ── Delete: delete character after cursor ────────
                KEY_DELETE => {
                    if cursor < len {
                        let del = WriteDelete {
                            position: cursor as u32,
                        };
                        let del_msg = unsafe { ipc::Message::from_payload(MSG_WRITE_DELETE, &del) };
                        docmodel_ch.send(&del_msg);
                        let _ = sys::channel_signal(sys::ChannelHandle(DOCMODEL_HANDLE));
                    }
                }

                // ── Tab / Shift+Tab ─────────────────────────────
                KEY_TAB => {
                    if shift {
                        // Shift+Tab: dedent — remove up to 4 leading spaces.
                        if rich {
                            let buf = pt_buf(doc_buf, doc_capacity);
                            let ls = line_start_rich(buf, cursor);
                            let mut spaces = 0usize;
                            while spaces < 4 && ls + spaces < len {
                                if piecetable::byte_at(buf, (ls + spaces) as u32) == Some(b' ') {
                                    spaces += 1;
                                } else {
                                    break;
                                }
                            }
                            if spaces > 0 {
                                let del_range = WriteDeleteRange {
                                    start: ls as u32,
                                    end: (ls + spaces) as u32,
                                };
                                let del_msg = unsafe {
                                    ipc::Message::from_payload(MSG_WRITE_DELETE_RANGE, &del_range)
                                };
                                docmodel_ch.send(&del_msg);
                                let removed_before = if cursor >= ls + spaces {
                                    spaces
                                } else if cursor > ls {
                                    cursor - ls
                                } else {
                                    0
                                };
                                cursor -= removed_before;
                                let cm = CursorMove {
                                    position: cursor as u32,
                                };
                                let cm_msg =
                                    unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                                docmodel_ch.send(&cm_msg);
                                let _ = sys::channel_signal(sys::ChannelHandle(DOCMODEL_HANDLE));
                            }
                        } else {
                            // Flat text path.
                            let text = doc_content(doc_buf, len);
                            let ls = line_start_flat(text, cursor);
                            let mut spaces = 0usize;
                            while spaces < 4 && ls + spaces < len && text[ls + spaces] == b' ' {
                                spaces += 1;
                            }
                            if spaces > 0 {
                                let del_range = WriteDeleteRange {
                                    start: ls as u32,
                                    end: (ls + spaces) as u32,
                                };
                                let del_msg = unsafe {
                                    ipc::Message::from_payload(MSG_WRITE_DELETE_RANGE, &del_range)
                                };
                                docmodel_ch.send(&del_msg);
                                let removed_before = if cursor >= ls + spaces {
                                    spaces
                                } else if cursor > ls {
                                    cursor - ls
                                } else {
                                    0
                                };
                                cursor -= removed_before;
                                let cm = CursorMove {
                                    position: cursor as u32,
                                };
                                let cm_msg =
                                    unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                                docmodel_ch.send(&cm_msg);
                                let _ = sys::channel_signal(sys::ChannelHandle(DOCMODEL_HANDLE));
                            }
                        }
                    } else {
                        // Tab: insert 4 spaces.
                        if len + 4 <= doc_capacity {
                            for _ in 0..4 {
                                let insert = WriteInsert {
                                    position: cursor as u32,
                                    byte: b' ',
                                };
                                let ins_msg = unsafe {
                                    ipc::Message::from_payload(MSG_WRITE_INSERT, &insert)
                                };
                                docmodel_ch.send(&ins_msg);
                                cursor += 1;
                            }
                            let _ = sys::channel_signal(sys::ChannelHandle(DOCMODEL_HANDLE));
                        }
                    }
                }

                // ── Printable character ──────────────────────────
                _ => {
                    if key.ascii != 0 && key.ascii != 0x08 && len < doc_capacity {
                        let insert = WriteInsert {
                            position: cursor as u32,
                            byte: key.ascii,
                        };
                        let ins_msg =
                            unsafe { ipc::Message::from_payload(MSG_WRITE_INSERT, &insert) };
                        docmodel_ch.send(&ins_msg);
                        cursor += 1;
                        let _ = sys::channel_signal(sys::ChannelHandle(DOCMODEL_HANDLE));
                    }
                }
            }
        }
    }
}
