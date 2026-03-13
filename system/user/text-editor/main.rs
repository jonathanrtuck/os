//! Text editor — editor process demonstrating the edit protocol.
//!
//! Receives input events from the OS service (compositor) via IPC channel,
//! processes them into document write requests, and sends those requests
//! back to the OS service. The editor never writes to the document directly
//! — "never make the wrong path the happy path."
//!
//! The editor has a **read-only** shared memory mapping of the document
//! buffer (hardware-enforced via page table attributes). It reads document
//! content for cursor positioning and context-aware editing. All writes
//! go through IPC to the OS service (sole writer).
//!
//! # IPC protocol
//!
//! Receives: MSG_KEY_EVENT (keycode + ascii) from OS service
//!           MSG_EDITOR_CONFIG (layout dimensions for cursor navigation)
//! Sends:    MSG_WRITE_INSERT (position + byte to insert at cursor)
//!           MSG_WRITE_DELETE (position to delete before cursor)
//!           MSG_WRITE_DELETE_RANGE (delete selected byte range)
//!           MSG_CURSOR_MOVE (update cursor position)
//!           MSG_SELECTION_UPDATE (update selection anchor + cursor)
//!
//! # Shared memory
//!
//! Document buffer (read-only mapping from init):
//!   [0..8):   content_len (u64, written by compositor)
//!   [8..16):  cursor_pos  (u64, written by compositor)
//!   [64..N):  content bytes

#![no_std]
#![no_main]

const CHANNEL_SHM_BASE: usize = 0x4000_0000;
const DOC_HEADER_SIZE: usize = 64;
// Message types — shared with compositor.
const MSG_KEY_EVENT: u32 = 10;
const MSG_WRITE_INSERT: u32 = 30;
const MSG_WRITE_DELETE: u32 = 31;
const MSG_CURSOR_MOVE: u32 = 32;
const MSG_SELECTION_UPDATE: u32 = 33;
const MSG_WRITE_DELETE_RANGE: u32 = 34;
/// Compositor → editor: set cursor position from click-to-position.
const MSG_SET_CURSOR: u32 = 35;
const MSG_EDITOR_CONFIG: u32 = 4;
// Keycodes (Linux evdev).
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_HOME: u16 = 102;
const KEY_END: u16 = 107;
const KEY_LSHIFT: u16 = 42;
const KEY_RSHIFT: u16 = 54;

#[repr(C)]
#[derive(Clone, Copy)]
struct CursorMove {
    position: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct EditorConfig {
    doc_va: u64,
    doc_capacity: u32,
    _pad: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct KeyEvent {
    keycode: u16,
    pressed: u8,
    ascii: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct SelectionUpdate {
    sel_start: u32,
    sel_end: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteDelete {
    position: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteDeleteRange {
    start: u32,
    end: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteInsert {
    position: u32,
    byte: u8,
}

fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}
/// Read the document length from the shared buffer header.
fn doc_len(doc_buf: *const u8) -> usize {
    unsafe { core::ptr::read_volatile(doc_buf as *const u64) as usize }
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

    // Handle 1: OS service (compositor) — bidirectional.
    // SHM slot 1, endpoint 1: we receive input events, send write requests.
    let os_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    const OS_HANDLE: u8 = 1;

    // Editor-local cursor position (byte offset in document).
    let mut cursor: usize = 0;

    // Selection state: anchor is the fixed end of a selection. When
    // has_selection is true, the selection range is [min(anchor, cursor),
    // max(anchor, cursor)). Shift+arrow keys set/extend the selection;
    // regular movement or editing clears it.
    let mut has_selection: bool = false;
    let mut anchor: usize = 0;

    // Shift key tracking (left shift = keycode 42, right shift = keycode 54).
    let mut shift_held: bool = false;

    sys::print(b"     entering event loop\n");

    /// Send a selection update message to the compositor.
    fn send_selection(
        ch: &ipc::Channel,
        has_sel: bool,
        anchor: usize,
        cursor: usize,
    ) {
        let (sel_start, sel_end) = if has_sel {
            let lo = if anchor < cursor { anchor } else { cursor };
            let hi = if anchor < cursor { cursor } else { anchor };
            (lo as u32, hi as u32)
        } else {
            (0u32, 0u32)
        };

        let su = SelectionUpdate { sel_start, sel_end };
        let su_msg = unsafe { ipc::Message::from_payload(MSG_SELECTION_UPDATE, &su) };

        ch.send(&su_msg);
    }

    loop {
        let _ = sys::wait(&[OS_HANDLE], u64::MAX);

        while os_ch.try_recv(&mut msg) {
            // Handle click-to-position: compositor sets cursor directly.
            if msg.msg_type == MSG_SET_CURSOR {
                let cm: CursorMove = unsafe { msg.payload_as() };
                cursor = cm.position as usize;
                // Clear any active selection on click.
                has_selection = false;
                anchor = 0;

                continue;
            }

            if msg.msg_type != MSG_KEY_EVENT {
                continue;
            }

            let key: KeyEvent = unsafe { msg.payload_as() };

            // Track shift key state (press and release).
            if key.keycode == KEY_LSHIFT || key.keycode == KEY_RSHIFT {
                shift_held = key.pressed == 1;
                continue;
            }

            if key.pressed != 1 {
                continue;
            }

            // Read current document length from shared buffer.
            let len = doc_len(doc_buf);

            match key.keycode {
                KEY_LEFT => {
                    if shift_held {
                        // Shift+Left: extend/create selection.
                        if !has_selection {
                            anchor = cursor;
                            has_selection = true;
                        }

                        if cursor > 0 {
                            cursor -= 1;
                        }

                        let cm = CursorMove {
                            position: cursor as u32,
                        };
                        let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                        os_ch.send(&cm_msg);
                        send_selection(&os_ch, has_selection, anchor, cursor);

                        // Collapse selection if anchor == cursor.
                        if anchor == cursor {
                            has_selection = false;
                            send_selection(&os_ch, false, 0, 0);
                        }

                        let _ = sys::channel_signal(OS_HANDLE);
                    } else {
                        // Regular Left: clear selection, move cursor.
                        if has_selection {
                            // Move cursor to selection start (leftmost).
                            let sel_lo = if anchor < cursor { anchor } else { cursor };
                            cursor = sel_lo;
                            has_selection = false;

                            let cm = CursorMove {
                                position: cursor as u32,
                            };
                            let cm_msg =
                                unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                            os_ch.send(&cm_msg);
                            send_selection(&os_ch, false, 0, 0);

                            let _ = sys::channel_signal(OS_HANDLE);
                        } else if cursor > 0 {
                            cursor -= 1;

                            let cm = CursorMove {
                                position: cursor as u32,
                            };
                            let cm_msg =
                                unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                            os_ch.send(&cm_msg);

                            let _ = sys::channel_signal(OS_HANDLE);
                        }
                    }
                }
                KEY_RIGHT => {
                    if shift_held {
                        // Shift+Right: extend/create selection.
                        if !has_selection {
                            anchor = cursor;
                            has_selection = true;
                        }

                        if cursor < len {
                            cursor += 1;
                        }

                        let cm = CursorMove {
                            position: cursor as u32,
                        };
                        let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                        os_ch.send(&cm_msg);
                        send_selection(&os_ch, has_selection, anchor, cursor);

                        // Collapse selection if anchor == cursor.
                        if anchor == cursor {
                            has_selection = false;
                            send_selection(&os_ch, false, 0, 0);
                        }

                        let _ = sys::channel_signal(OS_HANDLE);
                    } else {
                        // Regular Right: clear selection, move cursor.
                        if has_selection {
                            // Move cursor to selection end (rightmost).
                            let sel_hi = if anchor > cursor { anchor } else { cursor };
                            cursor = sel_hi;
                            has_selection = false;

                            let cm = CursorMove {
                                position: cursor as u32,
                            };
                            let cm_msg =
                                unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                            os_ch.send(&cm_msg);
                            send_selection(&os_ch, false, 0, 0);

                            let _ = sys::channel_signal(OS_HANDLE);
                        } else if cursor < len {
                            cursor += 1;

                            let cm = CursorMove {
                                position: cursor as u32,
                            };
                            let cm_msg =
                                unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };
                            os_ch.send(&cm_msg);

                            let _ = sys::channel_signal(OS_HANDLE);
                        }
                    }
                }
                KEY_HOME => {
                    // Clear selection on Home.
                    if has_selection {
                        has_selection = false;
                        send_selection(&os_ch, false, 0, 0);
                    }

                    cursor = 0;

                    let cm = CursorMove {
                        position: cursor as u32,
                    };
                    let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };

                    os_ch.send(&cm_msg);

                    let _ = sys::channel_signal(OS_HANDLE);
                }
                KEY_END => {
                    // Clear selection on End.
                    if has_selection {
                        has_selection = false;
                        send_selection(&os_ch, false, 0, 0);
                    }

                    cursor = len;

                    let cm = CursorMove {
                        position: cursor as u32,
                    };
                    let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };

                    os_ch.send(&cm_msg);

                    let _ = sys::channel_signal(OS_HANDLE);
                }
                _ => {
                    if key.ascii == 0x08 {
                        // Backspace.
                        if has_selection {
                            // Delete entire selection range.
                            let sel_lo = if anchor < cursor { anchor } else { cursor };
                            let sel_hi = if anchor < cursor { cursor } else { anchor };

                            let del_range = WriteDeleteRange {
                                start: sel_lo as u32,
                                end: sel_hi as u32,
                            };
                            let del_msg = unsafe {
                                ipc::Message::from_payload(MSG_WRITE_DELETE_RANGE, &del_range)
                            };

                            os_ch.send(&del_msg);

                            cursor = sel_lo;
                            has_selection = false;
                            send_selection(&os_ch, false, 0, 0);

                            let _ = sys::channel_signal(OS_HANDLE);
                        } else if cursor > 0 {
                            // Delete character before cursor.
                            cursor -= 1;

                            let del = WriteDelete {
                                position: cursor as u32,
                            };
                            let del_msg =
                                unsafe { ipc::Message::from_payload(MSG_WRITE_DELETE, &del) };

                            os_ch.send(&del_msg);

                            let _ = sys::channel_signal(OS_HANDLE);
                        }
                    } else if key.ascii != 0 {
                        // Printable character.
                        if has_selection {
                            // Replace selection: delete range, then insert.
                            let sel_lo = if anchor < cursor { anchor } else { cursor };
                            let sel_hi = if anchor < cursor { cursor } else { anchor };

                            let del_range = WriteDeleteRange {
                                start: sel_lo as u32,
                                end: sel_hi as u32,
                            };
                            let del_msg = unsafe {
                                ipc::Message::from_payload(MSG_WRITE_DELETE_RANGE, &del_range)
                            };

                            os_ch.send(&del_msg);

                            cursor = sel_lo;
                            has_selection = false;

                            // Insert the typed character at the selection start.
                            let insert = WriteInsert {
                                position: cursor as u32,
                                byte: key.ascii,
                            };
                            let ins_msg =
                                unsafe { ipc::Message::from_payload(MSG_WRITE_INSERT, &insert) };

                            os_ch.send(&ins_msg);
                            cursor += 1;
                            send_selection(&os_ch, false, 0, 0);

                            let _ = sys::channel_signal(OS_HANDLE);
                        } else if len < doc_capacity {
                            // Insert at cursor (no selection).
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
}
