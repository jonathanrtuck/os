//! Text editor — a simple editor process demonstrating the edit protocol.
//!
//! Receives input events from the OS service (compositor) via IPC channel,
//! processes them into document write requests, and sends those requests
//! back to the OS service. The editor never writes to the document directly
//! — "never make the wrong path the happy path."
//!
//! # IPC protocol
//!
//! Receives: MSG_KEY_EVENT (keycode + ascii) from OS service
//! Sends:    MSG_WRITE_INSERT (byte to insert at cursor)
//!           MSG_WRITE_DELETE (delete character before cursor)
//!
//! # Architecture
//!
//! This is the first process in the system that demonstrates the settled
//! editor architecture: editors are read-only consumers of documents,
//! all writes go through the OS service. The editor receives input,
//! decides what the input means (e.g., 'a' key = insert 'a'), and sends
//! a write request. The OS service applies the write and re-renders.

#![no_std]
#![no_main]

const CHANNEL_SHM_BASE: usize = 0x4000_0000;
// Message types — shared with compositor.
const MSG_KEY_EVENT: u32 = 10;
const MSG_WRITE_INSERT: u32 = 30;
const MSG_WRITE_DELETE: u32 = 31;

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
    byte: u8,
}

fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x93\x9D text-editor starting\n");

    // Handle 0: init config channel (unused by editor).
    // Handle 1: OS service (compositor) — bidirectional.
    // SHM slot 1, endpoint 1: we receive input events, send write requests.
    let os_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };

    const OS_HANDLE: u8 = 1;

    sys::print(b"     entering event loop\n");

    let mut msg = ipc::Message::new(0);

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

            if key.ascii == 0x08 {
                // Backspace → delete request.
                let del_msg = ipc::Message::new(MSG_WRITE_DELETE);

                os_ch.send(&del_msg);

                let _ = sys::channel_signal(OS_HANDLE);
            } else if key.ascii != 0 {
                // Printable character → insert request.
                let insert = WriteInsert { byte: key.ascii };
                let ins_msg = unsafe { ipc::Message::from_payload(MSG_WRITE_INSERT, &insert) };

                os_ch.send(&ins_msg);

                let _ = sys::channel_signal(OS_HANDLE);
            }
        }
    }
}
