//! Userspace virtio-input driver (keyboard + tablet).
//!
//! Receives device config (MMIO PA, IRQ) from init via IPC ring buffer.
//! Reads input events from the virtio event queue and forwards them
//! to the compositor via a direct IPC channel.
//!
//! Handles three event types:
//! - EV_KEY (type 1): keyboard key press/release → MSG_KEY_EVENT
//!   Also handles mouse button events: BTN_LEFT (0x110), BTN_RIGHT (0x111)
//!   → MSG_POINTER_BUTTON
//! - EV_ABS (type 3): absolute pointer coordinates from virtio-tablet
//!   ABS_X (code 0x00) and ABS_Y (code 0x01) in [0, 32767]
//!   → MSG_POINTER_ABS
//!
//! # virtio-input protocol
//!
//! The event virtqueue (queue 0) is device-to-driver: the driver pre-posts
//! device-writable buffers, and the device fills them with 8-byte events
//! when input occurs. Each event is a Linux evdev struct:
//!
//! ```text
//! le16 type   — EV_KEY (1), EV_REL (2), EV_ABS (3), EV_SYN (0)
//! le16 code   — Linux keycode (e.g. KEY_A = 30)
//! le32 value  — 1 = press, 0 = release, 2 = repeat (for EV_KEY)
//!               absolute coordinate (for EV_ABS)
//! ```
//!
//! # Architecture note
//!
//! In the real OS, input events flow: input driver → OS service (input
//! router) → active editor. The editor modifies document state via the
//! edit protocol; the OS service re-renders. For this demo, the compositor
//! plays both roles (OS service + editor), so we send directly to it.

#![no_std]
#![no_main]

use protocol::{
    device::{DeviceConfig, MSG_DEVICE_CONFIG},
    input::{
        KeyEvent, PointerAbs, PointerButton, MSG_KEY_EVENT, MSG_POINTER_ABS, MSG_POINTER_BUTTON,
    },
};

/// Linux evdev event type for key press/release.
const EV_KEY: u16 = 1;
/// Linux evdev event type for absolute axis events (touch/tablet).
const EV_ABS: u16 = 3;
/// Absolute axis codes (Linux input.h).
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
/// Mouse button codes (Linux input.h). These arrive as EV_KEY events.
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
/// Size of a virtio_input_event struct (8 bytes).
const EVENT_SIZE: u32 = 8;
/// Event virtqueue index.
const VIRTQ_EVENT: u32 = 0;

/// A virtio-input event (matches Linux's input_event without timeval).
#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioInputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

/// Compute the base VA of channel N's shared pages.
fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}
/// Translate a Linux evdev keycode to ASCII.
///
/// Returns 0 for unmapped keys. Only covers the basic US keyboard layout
/// (lowercase letters, digits, punctuation, space, enter, backspace, tab).
fn keycode_to_ascii(code: u16) -> u8 {
    // Index = Linux keycode, value = ASCII character (0 = unmapped).
    // Covers keycodes 0–57 (KEY_RESERVED through KEY_SPACE).
    static MAP: [u8; 58] = [
        0, 0, // 0: reserved, 1: ESC
        b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', // 2–11
        b'-', b'=',  // 12–13
        0x08,  // 14: backspace (BS)
        b'\t', // 15: tab
        b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', // 16–25
        b'[', b']',  // 26–27
        b'\n', // 28: enter
        0,     // 29: left ctrl
        b'a', b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l', // 30–38
        b';', b'\'', // 39–40
        b'`',  // 41: grave
        0,     // 42: left shift
        b'\\', // 43: backslash
        b'z', b'x', b'c', b'v', b'b', b'n', b'm', // 44–50
        b',', b'.', b'/', // 51–53
        0, 0, 0,    // 54: rshift, 55: kp*, 56: lalt
        b' ', // 57: space
    ];

    if (code as usize) < MAP.len() {
        MAP[code as usize]
    } else {
        0
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Channel 0: init config (endpoint 1).
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virtio-input: no config message\n");
        sys::exit();
    }

    let config: DeviceConfig = unsafe { msg.payload_as() };
    // Map MMIO region (sub-page offset for virtio-mmio slots).
    let page_offset = config.mmio_pa & 0xFFF;
    let page_pa = config.mmio_pa & !0xFFF;
    let page_va = sys::device_map(page_pa, 0x1000).unwrap_or_else(|_| {
        sys::print(b"virtio-input: device_map failed\n");
        sys::exit();
    });
    let device = virtio::Device::new(page_va + page_offset as usize);

    if !device.negotiate() {
        sys::print(b"virtio-input: negotiate failed\n");
        sys::exit();
    }

    // IRQ handle goes into slot 2 (after init channel=0, compositor channel=1).
    let irq_handle = sys::interrupt_register(config.irq).unwrap_or_else(|_| {
        sys::print(b"virtio-input: interrupt_register failed\n");
        sys::exit();
    });
    // Setup event virtqueue.
    let queue_size = core::cmp::min(
        device.queue_max_size(VIRTQ_EVENT),
        virtio::DEFAULT_QUEUE_SIZE,
    );
    let vq_order = virtio::Virtqueue::allocation_order(queue_size);
    let mut vq_pa: u64 = 0;
    let vq_va = sys::dma_alloc(vq_order, &mut vq_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-input: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let vq_bytes = (1usize << vq_order) * 4096;

    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_bytes) };

    let mut vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        VIRTQ_EVENT,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );
    device.driver_ok();

    // Allocate a DMA page for event buffers. Each keypress generates at
    // least 2 events (EV_KEY + EV_SYN), so we need multiple buffers posted.
    // Use 64 buffers of 8 bytes each (512 bytes total, fits in one page).
    const NUM_EVENT_BUFS: usize = 64;

    let mut event_pa: u64 = 0;
    let event_va = sys::dma_alloc(0, &mut event_pa).unwrap_or_else(|_| {
        sys::print(b"virtio-input: dma_alloc (event) failed\n");
        sys::exit();
    });

    unsafe { core::ptr::write_bytes(event_va as *mut u8, 0, 4096) };

    // Pre-post all event buffers (each 8 bytes, device-writable).
    for i in 0..NUM_EVENT_BUFS {
        let buf_pa = event_pa + (i as u64 * EVENT_SIZE as u64);

        vq.push(buf_pa, EVENT_SIZE, true);
    }

    device.notify(VIRTQ_EVENT);

    // Channel 1: compositor events (endpoint 0 = send direction).
    let comp_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 0) };

    sys::print(b"  \xE2\x8C\xA8\xEF\xB8\x8F  virtio-input ready\n");

    // Track absolute pointer state. EV_ABS events for X and Y arrive as
    // separate events before an EV_SYN. We accumulate them and send a
    // single MSG_POINTER_ABS when either axis updates.
    let mut pointer_x: u32 = 0;
    let mut pointer_y: u32 = 0;

    // -----------------------------------------------------------------------
    // Event loop: wait for IRQ → read events → forward to compositor
    // -----------------------------------------------------------------------
    loop {
        let _ = sys::wait(&[irq_handle], u64::MAX);

        device.ack_interrupt();

        let mut repost_count = 0u32;
        let mut pointer_moved = false;

        while let Some(used) = vq.pop_used() {
            // Compute buffer VA from descriptor index (each buffer = 8 bytes).
            let buf_offset = used.id as usize * EVENT_SIZE as usize;
            let buf_va = event_va + buf_offset;
            let buf_pa = event_pa + buf_offset as u64;
            // Read the event from the DMA buffer.
            let event: VirtioInputEvent =
                unsafe { core::ptr::read_volatile(buf_va as *const VirtioInputEvent) };

            if event.event_type == EV_KEY && event.value <= 1 {
                // Check if this is a mouse button event.
                if event.code == BTN_LEFT || event.code == BTN_RIGHT {
                    let button = if event.code == BTN_LEFT { 0u8 } else { 1u8 };
                    let ptr_btn = PointerButton {
                        button,
                        pressed: event.value as u8,
                        _pad: [0; 2],
                    };
                    let msg = unsafe { ipc::Message::from_payload(MSG_POINTER_BUTTON, &ptr_btn) };

                    if !comp_ch.send(&msg) {
                        // Ring buffer full — event dropped.
                    }

                    let _ = sys::channel_signal(1);
                } else {
                    // Regular keyboard key press/release.
                    let ascii = keycode_to_ascii(event.code);
                    let key_event = KeyEvent {
                        keycode: event.code,
                        pressed: event.value as u8,
                        ascii,
                    };
                    let msg = unsafe { ipc::Message::from_payload(MSG_KEY_EVENT, &key_event) };

                    if !comp_ch.send(&msg) {
                        // Ring buffer full — event dropped.
                    }

                    let _ = sys::channel_signal(1);
                }
            } else if event.event_type == EV_ABS {
                // Absolute pointer axis event from virtio-tablet.
                match event.code {
                    ABS_X => {
                        pointer_x = event.value;
                        pointer_moved = true;
                    }
                    ABS_Y => {
                        pointer_y = event.value;
                        pointer_moved = true;
                    }
                    _ => {} // Ignore other axes.
                }
            }

            // Re-post this buffer for reuse.
            unsafe {
                core::ptr::write_bytes(buf_va as *mut u8, 0, EVENT_SIZE as usize);
            };

            vq.push(buf_pa, EVENT_SIZE, true);

            repost_count += 1;
        }

        // Send accumulated pointer position after processing all events
        // in this IRQ batch (avoids sending redundant intermediate positions).
        if pointer_moved {
            let ptr_abs = PointerAbs {
                x: pointer_x,
                y: pointer_y,
            };
            let msg = unsafe { ipc::Message::from_payload(MSG_POINTER_ABS, &ptr_abs) };

            if !comp_ch.send(&msg) {
                // Ring buffer full — event dropped.
            }

            let _ = sys::channel_signal(1);
        }

        // Batch-notify after reposting all consumed buffers.
        if repost_count > 0 {
            device.notify(VIRTQ_EVENT);
        }

        let _ = sys::interrupt_ack(irq_handle);
    }
}
