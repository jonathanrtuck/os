//! Text editor — content-type-specific input-to-write translator.
//!
//! Receives editing key events from core and translates them into
//! document write requests. Navigation and selection are handled by
//! core (the OS service owns cursor movement because the OS owns
//! layout). The editor handles only content mutation:
//!
//! - Character insertion (printable keys)
//! - Single-character deletion (Backspace, Delete)
//! - Tab (insert 4 spaces) / Shift+Tab (dedent)
//!
//! The editor has a **read-only** shared memory mapping of the document
//! buffer (hardware-enforced via page table attributes). It reads document
//! content for context-aware editing (e.g., finding leading spaces for
//! dedent). All writes go through IPC to core (sole writer).
//!
//! # IPC protocol
//!
//! Receives: MSG_KEY_EVENT (keycode + ascii + modifiers) from core
//!           MSG_SET_CURSOR (cursor position sync from core)
//!           MSG_EDITOR_CONFIG (layout dimensions)
//! Sends:    MSG_WRITE_INSERT (position + byte to insert)
//!           MSG_WRITE_DELETE (position to delete before cursor)
//!           MSG_WRITE_DELETE_RANGE (delete byte range)
//!
//! # Shared memory
//!
//! Document buffer (read-only mapping from init):
//!   [0..8):   content_len (u64, written by core)
//!   [8..16):  cursor_pos  (u64, written by core)
//!   [64..N):  content bytes

#![no_std]
#![no_main]

use protocol::{
    edit::{
        CursorMove, WriteDelete, WriteDeleteRange, WriteInsert, MSG_CURSOR_MOVE, MSG_SET_CURSOR,
        MSG_WRITE_DELETE, MSG_WRITE_DELETE_RANGE, MSG_WRITE_INSERT,
    },
    editor::{EditorConfig, MSG_EDITOR_CONFIG},
    input::{KeyEvent, MOD_SHIFT, MSG_KEY_EVENT},
};

const DOC_HEADER_SIZE: usize = 64;
// Keycodes (Linux evdev).
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_DELETE: u16 = 111;

fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

/// Read the document length from the shared buffer header.
fn doc_len(doc_buf: *const u8) -> usize {
    unsafe { core::ptr::read_volatile(doc_buf as *const u64) as usize }
}

/// Read the document content as a byte slice.
fn doc_content(doc_buf: *const u8, len: usize) -> &'static [u8] {
    // SAFETY: doc_buf + DOC_HEADER_SIZE is the start of content bytes,
    // len bytes are valid (maintained by core as sole writer).
    unsafe { core::slice::from_raw_parts(doc_buf.add(DOC_HEADER_SIZE), len) }
}

/// Find the start of the line containing `pos` (previous '\n' + 1, or 0).
fn line_start(text: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i > 0 && text[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x9D text-editor starting\n");

    // Handle 0: init config channel — read editor config.
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_EDITOR_CONFIG {
        sys::print(b"text-editor: no config message\n");
        sys::exit();
    }

    let config: EditorConfig = unsafe { msg.payload_as() };
    let doc_buf = config.doc_va as *const u8;
    let doc_capacity = config.doc_capacity as usize;

    if doc_buf.is_null() {
        sys::print(b"text-editor: no document buffer\n");
        sys::exit();
    }

    sys::print(b"     document buffer mapped (read-only)\n");

    // Handle 1: OS service (core) — bidirectional.
    // SHM slot 1, endpoint 1: we receive input events, send write requests.
    let os_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    const OS_HANDLE: u8 = 1;

    // Editor-local cursor position (byte offset in document).
    // Synced from core via MSG_SET_CURSOR.
    let mut cursor: usize = 0;

    sys::print(b"     entering event loop\n");

    loop {
        let _ = sys::wait(&[OS_HANDLE], u64::MAX);

        while os_ch.try_recv(&mut msg) {
            // Cursor sync from core (navigation, click, selection-delete).
            if msg.msg_type == MSG_SET_CURSOR {
                let cm: CursorMove = unsafe { msg.payload_as() };
                cursor = cm.position as usize;
                continue;
            }
            if msg.msg_type != MSG_KEY_EVENT {
                continue;
            }

            let key: KeyEvent = unsafe { msg.payload_as() };

            if key.pressed != 1 {
                continue;
            }

            let len = doc_len(doc_buf);
            let shift = key.modifiers & MOD_SHIFT != 0;

            match key.keycode {
                // ── Backspace: delete character before cursor ────
                KEY_BACKSPACE => {
                    if cursor > 0 {
                        cursor -= 1;
                        let del = WriteDelete {
                            position: cursor as u32,
                        };
                        let del_msg =
                            unsafe { ipc::Message::from_payload(MSG_WRITE_DELETE, &del) };
                        os_ch.send(&del_msg);
                        let _ = sys::channel_signal(OS_HANDLE);
                    }
                }

                // ── Delete: delete character after cursor ────────
                KEY_DELETE => {
                    if cursor < len {
                        let del = WriteDelete {
                            position: cursor as u32,
                        };
                        let del_msg =
                            unsafe { ipc::Message::from_payload(MSG_WRITE_DELETE, &del) };
                        os_ch.send(&del_msg);
                        let _ = sys::channel_signal(OS_HANDLE);
                    }
                }

                // ── Tab / Shift+Tab ─────────────────────────────
                KEY_TAB => {
                    if shift {
                        // Shift+Tab: dedent — remove up to 4 leading spaces.
                        let text = doc_content(doc_buf, len);
                        let ls = line_start(text, cursor);
                        let mut spaces = 0usize;
                        while spaces < 4
                            && ls + spaces < len
                            && text[ls + spaces] == b' '
                        {
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
                            os_ch.send(&del_msg);
                            // Adjust cursor: move back by however many spaces
                            // were removed before the cursor.
                            let removed_before = if cursor >= ls + spaces {
                                spaces
                            } else if cursor > ls {
                                cursor - ls
                            } else {
                                0
                            };
                            cursor -= removed_before;
                            // Tell core the new cursor position.
                            let cm = CursorMove {
                                position: cursor as u32,
                            };
                            let cm_msg =
                                unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                            os_ch.send(&cm_msg);
                            let _ = sys::channel_signal(OS_HANDLE);
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
                                os_ch.send(&ins_msg);
                                cursor += 1;
                            }
                            let _ = sys::channel_signal(OS_HANDLE);
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
                        os_ch.send(&ins_msg);
                        cursor += 1;
                        let _ = sys::channel_signal(OS_HANDLE);
                    }
                }
            }
        }
    }
}
