//! Virtio-input driver — keyboard + tablet.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!
//! Probes the virtio MMIO region for input devices (device ID 18).
//! Requests DMA VMOs from init for virtqueue and event buffers.
//! On key press, translates evdev codes to characters and forwards
//! to the presenter via sync IPC.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_VIRTIO_VMO: Handle = Handle(3);
const HANDLE_INIT_EP: Handle = Handle(4);

const PAGE_SIZE: usize = virtio::PAGE_SIZE;

const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const EV_TEXT: u16 = 0x10;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;

const KEY_LSHIFT: u16 = 42;
const KEY_RSHIFT: u16 = 54;
const KEY_LCTRL: u16 = 29;
const KEY_RCTRL: u16 = 97;
const KEY_LALT: u16 = 56;
const KEY_RALT: u16 = 100;
const KEY_LMETA: u16 = 125;
const KEY_RMETA: u16 = 126;
const KEY_CAPSLOCK: u16 = 58;

const EVENT_VIRTQ: u32 = 0;
const VIRTIO_EVENT_SIZE: u32 = 8;
const NUM_EVENT_BUFS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioInputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

fn modifier_bit(code: u16) -> u8 {
    match code {
        KEY_LSHIFT | KEY_RSHIFT => input::MOD_SHIFT,
        KEY_LCTRL | KEY_RCTRL => input::MOD_CONTROL,
        KEY_LALT | KEY_RALT => input::MOD_ALT,
        KEY_LMETA | KEY_RMETA => input::MOD_SUPER,
        _ => 0,
    }
}

// ── Evdev → character keymap (US QWERTY) ────────────────────────────

const HID_RETURN: u16 = 0x28;
const HID_BACKSPACE: u16 = 0x2A;
const HID_TAB: u16 = 0x2B;
const HID_DELETE: u16 = 0x4C;
const HID_RIGHT: u16 = 0x4F;
const HID_LEFT: u16 = 0x50;
const HID_DOWN: u16 = 0x51;
const HID_UP: u16 = 0x52;
const HID_HOME: u16 = 0x4A;
const HID_PAGE_UP: u16 = 0x4B;
const HID_END: u16 = 0x4D;
const HID_PAGE_DOWN: u16 = 0x4E;

fn evdev_to_key(code: u16, shift: bool) -> (u16, u8) {
    // (hid_key_code, ascii_char) — for printable: hid=0, char=ascii
    match code {
        // Row 1: digits
        2 => (0, if shift { b'!' } else { b'1' }),
        3 => (0, if shift { b'@' } else { b'2' }),
        4 => (0, if shift { b'#' } else { b'3' }),
        5 => (0, if shift { b'$' } else { b'4' }),
        6 => (0, if shift { b'%' } else { b'5' }),
        7 => (0, if shift { b'^' } else { b'6' }),
        8 => (0, if shift { b'&' } else { b'7' }),
        9 => (0, if shift { b'*' } else { b'8' }),
        10 => (0, if shift { b'(' } else { b'9' }),
        11 => (0, if shift { b')' } else { b'0' }),
        12 => (0, if shift { b'_' } else { b'-' }),
        13 => (0, if shift { b'+' } else { b'=' }),
        // Row 2: qwertyuiop
        16 => (0, if shift { b'Q' } else { b'q' }),
        17 => (0, if shift { b'W' } else { b'w' }),
        18 => (0, if shift { b'E' } else { b'e' }),
        19 => (0, if shift { b'R' } else { b'r' }),
        20 => (0, if shift { b'T' } else { b't' }),
        21 => (0, if shift { b'Y' } else { b'y' }),
        22 => (0, if shift { b'U' } else { b'u' }),
        23 => (0, if shift { b'I' } else { b'i' }),
        24 => (0, if shift { b'O' } else { b'o' }),
        25 => (0, if shift { b'P' } else { b'p' }),
        26 => (0, if shift { b'{' } else { b'[' }),
        27 => (0, if shift { b'}' } else { b']' }),
        // Row 3: asdfghjkl
        30 => (0, if shift { b'A' } else { b'a' }),
        31 => (0, if shift { b'S' } else { b's' }),
        32 => (0, if shift { b'D' } else { b'd' }),
        33 => (0, if shift { b'F' } else { b'f' }),
        34 => (0, if shift { b'G' } else { b'g' }),
        35 => (0, if shift { b'H' } else { b'h' }),
        36 => (0, if shift { b'J' } else { b'j' }),
        37 => (0, if shift { b'K' } else { b'k' }),
        38 => (0, if shift { b'L' } else { b'l' }),
        39 => (0, if shift { b':' } else { b';' }),
        40 => (0, if shift { b'"' } else { b'\'' }),
        41 => (0, if shift { b'~' } else { b'`' }),
        43 => (0, if shift { b'|' } else { b'\\' }),
        // Row 4: zxcvbnm
        44 => (0, if shift { b'Z' } else { b'z' }),
        45 => (0, if shift { b'X' } else { b'x' }),
        46 => (0, if shift { b'C' } else { b'c' }),
        47 => (0, if shift { b'V' } else { b'v' }),
        48 => (0, if shift { b'B' } else { b'b' }),
        49 => (0, if shift { b'N' } else { b'n' }),
        50 => (0, if shift { b'M' } else { b'm' }),
        51 => (0, if shift { b'<' } else { b',' }),
        52 => (0, if shift { b'>' } else { b'.' }),
        53 => (0, if shift { b'?' } else { b'/' }),
        // Special keys
        14 => (HID_BACKSPACE, 0),
        15 => (HID_TAB, 0),
        28 => (HID_RETURN, 0),
        57 => (0, b' '),
        111 => (HID_DELETE, 0),
        // Navigation keys
        102 => (HID_HOME, 0),
        103 => (HID_UP, 0),
        104 => (HID_PAGE_UP, 0),
        105 => (HID_LEFT, 0),
        106 => (HID_RIGHT, 0),
        107 => (HID_END, 0),
        108 => (HID_DOWN, 0),
        109 => (HID_PAGE_DOWN, 0),
        _ => (0, 0),
    }
}

// ── Forward events to presenter ─────────────────────────────────────

fn forward_key(presenter_ep: Handle, hid_code: u16, modifiers: u8, character: u8) {
    let mut payload = [0u8; 4];

    payload[0..2].copy_from_slice(&hid_code.to_le_bytes());
    payload[2] = modifiers;
    payload[3] = character;

    let _ = ipc::client::call_simple(presenter_ep, presenter_service::KEY_EVENT, &payload);
}

fn forward_pointer(presenter_ep: Handle, abs_x: u32, abs_y: u32) {
    let event = presenter_service::PointerEvent { abs_x, abs_y };
    let mut payload = [0u8; presenter_service::PointerEvent::SIZE];

    event.write_to(&mut payload);

    let _ = ipc::client::call_simple(presenter_ep, presenter_service::POINTER_EVENT, &payload);
}

fn forward_button(presenter_ep: Handle, abs_x: u32, abs_y: u32, button: u8, pressed: u8) {
    let event = presenter_service::PointerButton {
        abs_x,
        abs_y,
        button,
        pressed,
    };
    let mut payload = [0u8; presenter_service::PointerButton::SIZE];

    event.write_to(&mut payload);

    let _ = ipc::client::call_simple(presenter_ep, presenter_service::POINTER_BUTTON, &payload);
}

// ── Entry point ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };

    // Probe for two input devices: keyboard (first) and tablet (second).
    let (device, input_slot) = match virtio::find_device(virtio_va, virtio::DEVICE_INPUT) {
        Some(d) => d,
        None => abi::thread::exit(2),
    };

    if !device.negotiate() {
        abi::thread::exit(3);
    }

    let queue_size = device
        .queue_max_size(EVENT_VIRTQ)
        .min(virtio::DEFAULT_QUEUE_SIZE);
    let vq_bytes = virtio::Virtqueue::total_bytes(queue_size);
    let vq_alloc = vq_bytes.next_multiple_of(PAGE_SIZE);
    let vq_dma = match init::request_dma(HANDLE_INIT_EP, vq_alloc) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(4),
    };
    let vq_va = vq_dma.va;

    // SAFETY: vq_va is a valid DMA allocation; zeroing before virtqueue init.
    unsafe { core::ptr::write_bytes(vq_va as *mut u8, 0, vq_alloc) };

    let vq_pa = vq_va as u64;
    let mut vq = virtio::Virtqueue::new(queue_size, vq_va, vq_pa);

    device.setup_queue(
        EVENT_VIRTQ,
        queue_size,
        vq.desc_pa(),
        vq.avail_pa(),
        vq.used_pa(),
    );

    let event_alloc = PAGE_SIZE;
    let evt_dma = match init::request_dma(HANDLE_INIT_EP, event_alloc) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(5),
    };
    let event_va = evt_dma.va;

    // SAFETY: event_va is a valid DMA allocation; zeroing event buffer memory.
    unsafe { core::ptr::write_bytes(event_va as *mut u8, 0, event_alloc) };

    let event_pa = event_va as u64;

    for i in 0..NUM_EVENT_BUFS {
        let buf_pa = event_pa + (i as u64 * VIRTIO_EVENT_SIZE as u64);

        vq.push(buf_pa, VIRTIO_EVENT_SIZE, true);
    }

    device.driver_ok();
    device.notify(EVENT_VIRTQ);

    let irq_event = match abi::event::create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(6),
    };
    let irq_num = virtio::SPI_BASE_INTID + input_slot;

    if abi::event::bind_irq(irq_event, irq_num, 0x1).is_err() {
        abi::thread::exit(7);
    }

    // Tablet device (second input device) — optional.
    let tablet = virtio::find_device_from(virtio_va, virtio::DEVICE_INPUT, input_slot as usize + 1);
    let mut tab_vq: Option<virtio::Virtqueue> = None;
    let mut tab_device: Option<virtio::Device> = None;
    let mut tab_event_va: usize = 0;
    let mut tab_event_pa: u64 = 0;
    let tab_irq_event: Option<Handle>;

    if let Some((tab_dev, tab_slot)) = tablet {
        if tab_dev.negotiate() {
            let tq_size = tab_dev
                .queue_max_size(EVENT_VIRTQ)
                .min(virtio::DEFAULT_QUEUE_SIZE);
            let tq_alloc = virtio::Virtqueue::total_bytes(tq_size).next_multiple_of(PAGE_SIZE);

            if let Ok(tq_dma) = init::request_dma(HANDLE_INIT_EP, tq_alloc) {
                // SAFETY: tq_dma.va is a valid DMA allocation.
                unsafe { core::ptr::write_bytes(tq_dma.va as *mut u8, 0, tq_alloc) };

                let mut tvq = virtio::Virtqueue::new(tq_size, tq_dma.va, tq_dma.va as u64);

                tab_dev.setup_queue(
                    EVENT_VIRTQ,
                    tq_size,
                    tvq.desc_pa(),
                    tvq.avail_pa(),
                    tvq.used_pa(),
                );

                if let Ok(te_dma) = init::request_dma(HANDLE_INIT_EP, event_alloc) {
                    // SAFETY: te_dma.va is a valid DMA allocation.
                    unsafe { core::ptr::write_bytes(te_dma.va as *mut u8, 0, event_alloc) };

                    tab_event_va = te_dma.va;
                    tab_event_pa = te_dma.va as u64;

                    for i in 0..NUM_EVENT_BUFS {
                        let buf_pa = tab_event_pa + (i as u64 * VIRTIO_EVENT_SIZE as u64);

                        tvq.push(buf_pa, VIRTIO_EVENT_SIZE, true);
                    }

                    tab_dev.driver_ok();
                    tab_dev.notify(EVENT_VIRTQ);

                    if let Ok(te) = abi::event::create() {
                        let tab_irq = virtio::SPI_BASE_INTID + tab_slot;

                        if abi::event::bind_irq(te, tab_irq, 0x1).is_ok() {
                            tab_irq_event = Some(te);
                            tab_vq = Some(tvq);
                            tab_device = Some(tab_dev);
                        } else {
                            tab_irq_event = None;
                        }
                    } else {
                        tab_irq_event = None;
                    }
                } else {
                    tab_irq_event = None;
                }
            } else {
                tab_irq_event = None;
            }
        } else {
            tab_irq_event = None;
        }
    } else {
        tab_irq_event = None;
    }

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    name::register(HANDLE_NS_EP, b"input", own_ep);

    let console_ep = name::lookup(HANDLE_NS_EP, b"console").ok();
    let mut presenter_ep: Option<Handle> = None;
    let mut modifiers: u8 = 0;
    let mut pointer_x: u32 = 0;
    let mut pointer_y: u32 = 0;
    let mut pointer_dirty = false;
    let mut wait_entries: [_; 2] = [(irq_event, 0x1), (Handle(0), 0)];
    let wait_count = if let Some(te) = tab_irq_event {
        wait_entries[1] = (te, 0x1);
        2
    } else {
        1
    };

    loop {
        let _ = abi::event::wait(&wait_entries[..wait_count]);

        // ── Keyboard device events ──
        device.ack_interrupt();

        let _ = abi::event::clear(irq_event, 0x1);
        let mut repost_count = 0u32;

        while let Some(used) = vq.pop_used() {
            let idx = used.id as usize;

            if idx >= NUM_EVENT_BUFS {
                continue;
            }

            let buf_offset = idx * VIRTIO_EVENT_SIZE as usize;
            let buf_va = event_va + buf_offset;
            let buf_pa = event_pa + buf_offset as u64;
            // SAFETY: buf_va points to DMA buffer written by device.
            let event: VirtioInputEvent =
                unsafe { core::ptr::read_volatile(buf_va as *const VirtioInputEvent) };

            if event.event_type == EV_TEXT && event.value > 0 {
                let codepoint = event.value;

                if codepoint < 128 {
                    if presenter_ep.is_none() {
                        presenter_ep = name::lookup(HANDLE_NS_EP, b"presenter").ok();
                    }

                    if let Some(ep) = presenter_ep {
                        forward_key(ep, 0, modifiers, codepoint as u8);
                    }
                }
            } else if event.event_type == EV_KEY && event.value <= 1 {
                let pressed = event.value == 1;

                if event.code == BTN_LEFT || event.code == BTN_RIGHT {
                    if presenter_ep.is_none() {
                        presenter_ep = name::lookup(HANDLE_NS_EP, b"presenter").ok();
                    }

                    if let Some(ep) = presenter_ep {
                        let btn_id = if event.code == BTN_LEFT { 0 } else { 1 };

                        forward_button(ep, pointer_x, pointer_y, btn_id, pressed as u8);
                    }
                } else {
                    let mod_bit = modifier_bit(event.code);

                    if mod_bit != 0 {
                        if pressed {
                            modifiers |= mod_bit;
                        } else {
                            modifiers &= !mod_bit;
                        }
                    }

                    if event.code == KEY_CAPSLOCK && pressed {
                        modifiers ^= input::MOD_CAPS_LOCK;
                    }

                    if pressed && mod_bit == 0 {
                        let shift = modifiers & input::MOD_SHIFT != 0;
                        let (hid, ch) = evdev_to_key(event.code, shift);

                        if hid != 0 || ch != 0 {
                            if presenter_ep.is_none() {
                                presenter_ep = name::lookup(HANDLE_NS_EP, b"presenter").ok();

                                if let (Some(ep), Some(con)) = (presenter_ep, console_ep) {
                                    let _ = ep;

                                    console::write(con, b"input: presenter connected\n");
                                }
                            }

                            if let Some(ep) = presenter_ep {
                                forward_key(ep, hid, modifiers, ch);
                            }
                        }
                    }
                }
            }

            // SAFETY: buf_va is within DMA allocation; zeroing before repost.
            unsafe {
                core::ptr::write_bytes(buf_va as *mut u8, 0, VIRTIO_EVENT_SIZE as usize);
            };

            vq.push(buf_pa, VIRTIO_EVENT_SIZE, true);

            repost_count += 1;
        }

        if repost_count > 0 {
            device.notify(EVENT_VIRTQ);
        }

        // ── Tablet device events (pointer) ──
        if let (Some(td), Some(tvq), Some(te)) = (&tab_device, &mut tab_vq, tab_irq_event) {
            td.ack_interrupt();

            let _ = abi::event::clear(te, 0x1);
            let mut tab_repost = 0u32;

            while let Some(used) = tvq.pop_used() {
                let idx = used.id as usize;

                if idx >= NUM_EVENT_BUFS {
                    continue;
                }

                let buf_offset = idx * VIRTIO_EVENT_SIZE as usize;
                let buf_va = tab_event_va + buf_offset;
                let buf_pa = tab_event_pa + buf_offset as u64;
                // SAFETY: buf_va points to tablet DMA buffer written by device.
                let event: VirtioInputEvent =
                    unsafe { core::ptr::read_volatile(buf_va as *const VirtioInputEvent) };

                if event.event_type == EV_ABS {
                    match event.code {
                        ABS_X => {
                            pointer_x = event.value;
                            pointer_dirty = true;
                        }
                        ABS_Y => {
                            pointer_y = event.value;
                            pointer_dirty = true;
                        }
                        _ => {}
                    }
                } else if event.event_type == EV_KEY
                    && (event.code == BTN_LEFT || event.code == BTN_RIGHT)
                    && event.value <= 1
                {
                    if presenter_ep.is_none() {
                        presenter_ep = name::lookup(HANDLE_NS_EP, b"presenter").ok();
                    }

                    if let Some(ep) = presenter_ep {
                        let btn_id = if event.code == BTN_LEFT { 0 } else { 1 };

                        forward_button(ep, pointer_x, pointer_y, btn_id, event.value as u8);
                    }
                }

                // SAFETY: buf_va is within DMA allocation.
                unsafe {
                    core::ptr::write_bytes(buf_va as *mut u8, 0, VIRTIO_EVENT_SIZE as usize);
                };

                tvq.push(buf_pa, VIRTIO_EVENT_SIZE, true);

                tab_repost += 1;
            }

            if tab_repost > 0 {
                td.notify(EVENT_VIRTQ);
            }
        }

        if pointer_dirty {
            pointer_dirty = false;

            if presenter_ep.is_none() {
                presenter_ep = name::lookup(HANDLE_NS_EP, b"presenter").ok();
            }

            if let Some(ep) = presenter_ep {
                forward_pointer(ep, pointer_x, pointer_y);
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
