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
//!   → shared PointerState register (atomic u64, no IPC ring)
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
    device::MSG_DEVICE_CONFIG,
    input::{
        KeyEvent, PointerButton, MOD_ALT, MOD_CAPS_LOCK, MOD_CTRL, MOD_SHIFT, MOD_SUPER,
        MSG_KEY_EVENT, MSG_POINTER_BUTTON,
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
/// Modifier keycodes (Linux evdev).
const KEY_LSHIFT: u16 = 42;
const KEY_RSHIFT: u16 = 54;
const KEY_LCTRL: u16 = 29;
const KEY_RCTRL: u16 = 97;
const KEY_LALT: u16 = 56;
const KEY_RALT: u16 = 100;
const KEY_LMETA: u16 = 125;
const KEY_RMETA: u16 = 126;
const KEY_CAPSLOCK: u16 = 58;

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
/// Translate a Linux evdev keycode to ASCII, applying shift if held.
///
/// Returns 0 for unmapped keys. Covers the US keyboard layout
/// (letters, digits, punctuation, space, enter, backspace, tab).
fn keycode_to_ascii(code: u16, shifted: bool) -> u8 {
    // Unshifted: index = Linux keycode, value = ASCII character (0 = unmapped).
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

    // Shifted: same indices, uppercase letters and shifted punctuation.
    static SHIFT_MAP: [u8; 58] = [
        0, 0, // 0: reserved, 1: ESC
        b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', // 2–11
        b'_', b'+',  // 12–13
        0x08,  // 14: backspace (unchanged)
        b'\t', // 15: tab (unchanged)
        b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', // 16–25
        b'{', b'}',  // 26–27
        b'\n', // 28: enter (unchanged)
        0,     // 29: left ctrl
        b'A', b'S', b'D', b'F', b'G', b'H', b'J', b'K', b'L', // 30–38
        b':', b'"', // 39–40
        b'~', // 41: grave → tilde
        0,    // 42: left shift
        b'|', // 43: backslash → pipe
        b'Z', b'X', b'C', b'V', b'B', b'N', b'M', // 44–50
        b'<', b'>', b'?', // 51–53
        0, 0, 0,    // 54: rshift, 55: kp*, 56: lalt
        b' ', // 57: space (unchanged)
    ];

    let idx = code as usize;
    if idx >= MAP.len() {
        return 0;
    }

    let base = MAP[idx];
    if base == 0 {
        return 0;
    }

    if shifted {
        SHIFT_MAP[idx]
    } else {
        base
    }
}

/// Update modifier bitmask for a key press/release event.
/// Returns the modifier bit if the keycode is a modifier key, 0 otherwise.
fn modifier_bit(code: u16) -> u8 {
    match code {
        KEY_LSHIFT | KEY_RSHIFT => MOD_SHIFT,
        KEY_LCTRL | KEY_RCTRL => MOD_CTRL,
        KEY_LALT | KEY_RALT => MOD_ALT,
        KEY_LMETA | KEY_RMETA => MOD_SUPER,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Channel 0: init config (endpoint 1).
    // SAFETY: addr is the base of channel SHM region mapped by kernel at page-aligned boundaries.
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_DEVICE_CONFIG {
        sys::print(b"virtio-input: no config message\n");
        sys::exit();
    }

    let config = if let Some(protocol::device::Message::DeviceConfig(c)) =
        protocol::device::decode(msg.msg_type, &msg.payload)
    {
        c
    } else {
        sys::print(b"virtio-input: bad device config\n");
        sys::exit();
    };
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
    let irq_handle: sys::InterruptHandle =
        sys::interrupt_register(config.irq).unwrap_or_else(|_| {
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
    let vq_bytes = (1usize << vq_order) * ipc::PAGE_SIZE;

    // SAFETY: vq_va is a valid DMA allocation of vq_bytes; zeroing virtqueue memory before use.
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

    // SAFETY: event_va is a valid DMA page allocation; zeroing event buffer memory before use.
    unsafe { core::ptr::write_bytes(event_va as *mut u8, 0, ipc::PAGE_SIZE) };

    // Pre-post all event buffers (each 8 bytes, device-writable).
    for i in 0..NUM_EVENT_BUFS {
        let buf_pa = event_pa + (i as u64 * EVENT_SIZE as u64);

        vq.push(buf_pa, EVENT_SIZE, true);
    }

    device.notify(VIRTQ_EVENT);

    // Channel 1: compositor events (endpoint 0 = send direction).
    // SAFETY: same as above — channel SHM region mapped by kernel at page-aligned boundaries.
    let comp_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 0) };

    // Receive pointer state register VA from init (optional — 0 if not present).
    let pointer_state_va: usize = if init_ch.try_recv(&mut msg)
        && msg.msg_type == protocol::input::MSG_POINTER_STATE_CONFIG
    {
        if let Some(protocol::input::Message::PointerStateConfig(cfg)) =
            protocol::input::decode(msg.msg_type, &msg.payload)
        {
            cfg.state_va as usize
        } else {
            0
        }
    } else {
        0
    };

    sys::print(b"  \xE2\x8C\xA8\xEF\xB8\x8F  virtio-input ready\n");

    // Track absolute pointer state. EV_ABS events for X and Y arrive as
    // separate events before an EV_SYN. We accumulate them and write to
    // the shared state register (atomic u64, init-allocated).
    let mut pointer_x: u32 = 0;
    let mut pointer_y: u32 = 0;

    // Modifier key state (packed bitmask).
    let mut modifiers: u8 = 0;

    // -----------------------------------------------------------------------
    // Event loop: wait for IRQ → read events → forward to compositor
    // -----------------------------------------------------------------------
    loop {
        let _ = sys::wait(&[irq_handle.0], u64::MAX);

        device.ack_interrupt();

        let mut repost_count = 0u32;
        let mut pointer_moved = false;

        while let Some(used) = vq.pop_used() {
            let idx = used.id as usize;
            if idx >= NUM_EVENT_BUFS {
                continue; // malformed completion from device
            }
            // Compute buffer VA from descriptor index (each buffer = 8 bytes).
            let buf_offset = idx * EVENT_SIZE as usize;
            let buf_va = event_va + buf_offset;
            let buf_pa = event_pa + buf_offset as u64;
            // Read the event from the DMA buffer.
            // SAFETY: buf_va points to DMA buffer written by device; volatile required for device visibility.
            let event: VirtioInputEvent =
                unsafe { core::ptr::read_volatile(buf_va as *const VirtioInputEvent) };

            if event.event_type == EV_KEY && event.value <= 1 {
                let pressed = event.value == 1;

                // Check if this is a mouse button event.
                if event.code == BTN_LEFT || event.code == BTN_RIGHT {
                    let button = if event.code == BTN_LEFT { 0u8 } else { 1u8 };
                    let ptr_btn = PointerButton {
                        button,
                        pressed: event.value as u8,
                        _pad: [0; 2],
                    };
                    // SAFETY: PointerButton fits within 60-byte IPC payload.
                    let msg = unsafe { ipc::Message::from_payload(MSG_POINTER_BUTTON, &ptr_btn) };

                    if !comp_ch.send(&msg) {
                        sys::print(b"virtio-input: ring full, event dropped\n");
                    }

                    let _ = sys::channel_signal(sys::ChannelHandle(1));
                } else {
                    // Update modifier state.
                    let mod_bit = modifier_bit(event.code);
                    if mod_bit != 0 {
                        if pressed {
                            modifiers |= mod_bit;
                        } else {
                            modifiers &= !mod_bit;
                        }
                    }
                    // Caps Lock: macOS sends press when capsLock flag turns ON,
                    // release when it turns OFF. Map directly to modifier state
                    // (set on press, clear on release) rather than toggling.
                    if event.code == KEY_CAPSLOCK {
                        if pressed {
                            modifiers |= MOD_CAPS_LOCK;
                        } else {
                            modifiers &= !MOD_CAPS_LOCK;
                        }
                    }

                    // Compute ASCII with shift/caps applied.
                    // Letters are affected by both Shift and Caps Lock (XOR behavior).
                    // Punctuation is affected by Shift only.
                    let shift_held = modifiers & MOD_SHIFT != 0;
                    let caps_on = modifiers & MOD_CAPS_LOCK != 0;
                    let base_ascii = keycode_to_ascii(event.code, false);
                    let is_letter = base_ascii.is_ascii_lowercase();
                    let effective_shift = if is_letter {
                        shift_held ^ caps_on // XOR: shift+caps = lowercase
                    } else {
                        shift_held
                    };
                    let ascii = keycode_to_ascii(event.code, effective_shift);

                    let key_event = KeyEvent {
                        keycode: event.code,
                        pressed: event.value as u8,
                        ascii,
                        modifiers,
                        _pad: 0,
                    };
                    // SAFETY: KeyEvent fits within 60-byte IPC payload.
                    let msg = unsafe { ipc::Message::from_payload(MSG_KEY_EVENT, &key_event) };

                    if !comp_ch.send(&msg) {
                        sys::print(b"virtio-input: ring full, event dropped\n");
                    }

                    let _ = sys::channel_signal(sys::ChannelHandle(1));
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
            // SAFETY: buf_va points to DMA buffer of EVENT_SIZE bytes; zeroing before repost.
            unsafe {
                core::ptr::write_bytes(buf_va as *mut u8, 0, EVENT_SIZE as usize);
            };

            vq.push(buf_pa, EVENT_SIZE, true);

            repost_count += 1;
        }

        // Write accumulated pointer position to shared state register.
        // Atomic u64 store — no ring, no overflow, always latest.
        if pointer_moved {
            let packed = protocol::input::PointerState::pack(pointer_x, pointer_y);
            // SAFETY: pointer_state_va points to a shared PointerState page
            // mapped by init. Atomic store-release for cross-core visibility.
            unsafe {
                let atom = &*(pointer_state_va as *const core::sync::atomic::AtomicU64);
                atom.store(packed, core::sync::atomic::Ordering::Release);
            }
            // Signal core to wake and read the new state.
            let _ = sys::channel_signal(sys::ChannelHandle(1));
        }

        // Batch-notify after reposting all consumed buffers.
        if repost_count > 0 {
            device.notify(VIRTQ_EVENT);
        }

        let _ = sys::interrupt_ack(irq_handle);
    }
}
