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
//! Sends:    MSG_WRITE_INSERT (position + byte to insert at cursor)
//!           MSG_WRITE_DELETE (position to delete before cursor)
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
const MSG_EDITOR_CONFIG: u32 = 4;
// Keycodes (Linux evdev).
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_HOME: u16 = 102;
const KEY_END: u16 = 107;

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
struct WriteInsert {
    position: u32,
    byte: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteDelete {
    position: u32,
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

    sys::print(b"     entering event loop\n");

    loop {
        let _ = sys::wait(&[OS_HANDLE], u64::MAX);

        while os_ch.try_recv(&mut msg) {
            if msg.msg_type != MSG_KEY_EVENT {
                continue;
            }

            let key: KeyEvent = unsafe { msg.payload_as() };

            if key.pressed != 1 {
                continue;
            }

            // Read current document length from shared buffer.
            let len = doc_len(doc_buf);

            match key.keycode {
                KEY_LEFT => {
                    if cursor > 0 {
                        cursor -= 1;

                        let cm = CursorMove {
                            position: cursor as u32,
                        };
                        let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };

                        os_ch.send(&cm_msg);

                        let _ = sys::channel_signal(OS_HANDLE);
                    }
                }
                KEY_RIGHT => {
                    if cursor < len {
                        cursor += 1;

                        let cm = CursorMove {
                            position: cursor as u32,
                        };
                        let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };

                        os_ch.send(&cm_msg);

                        let _ = sys::channel_signal(OS_HANDLE);
                    }
                }
                KEY_HOME => {
                    cursor = 0;

                    let cm = CursorMove {
                        position: cursor as u32,
                    };
                    let cm_msg = unsafe { ipc::Message::from_payload(MSG_CURSOR_MOVE, &cm) };

                    os_ch.send(&cm_msg);

                    let _ = sys::channel_signal(OS_HANDLE);
                }
                KEY_END => {
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
                        // Backspace: delete character before cursor.
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
                    } else if key.ascii != 0 {
                        // Printable character: insert at cursor.
                        if len < doc_capacity {
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
