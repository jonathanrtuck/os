//! Virtio-input driver — keyboard + tablet.
//!
//! Bootstrap handles (from init via thread_create_in):
//!   Handle 2: name service endpoint
//!   Handle 3: virtio MMIO VMO (device, identity-mapped)
//!   Handle 4: init endpoint (for DMA allocation)
//!
//! Probes the virtio MMIO region for input devices (device ID 18).
//! Requests DMA VMOs from init for virtqueue and event buffers.
//! Reads virtio-input events and registers with the name service.

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

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let virtio_va = match abi::vmo::map(HANDLE_VIRTIO_VMO, 0, rw) {
        Ok(va) => va,
        Err(_) => abi::thread::exit(1),
    };
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

    let own_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(8),
    };

    name::register(HANDLE_NS_EP, b"input", own_ep);

    let mut _modifiers: u8 = 0;

    loop {
        let _ = abi::event::wait(&[(irq_event, 0x1)]);
        let _ = abi::event::clear(irq_event, 0x1);

        device.ack_interrupt();

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

            if event.event_type == EV_KEY && event.value <= 1 {
                let pressed = event.value == 1;

                if event.code == BTN_LEFT || event.code == BTN_RIGHT {
                    let _button = if event.code == BTN_LEFT {
                        input::BUTTON_LEFT
                    } else {
                        input::BUTTON_RIGHT
                    };
                } else {
                    let mod_bit = modifier_bit(event.code);

                    if mod_bit != 0 {
                        if pressed {
                            _modifiers |= mod_bit;
                        } else {
                            _modifiers &= !mod_bit;
                        }
                    }

                    if event.code == KEY_CAPSLOCK {
                        if pressed {
                            _modifiers |= input::MOD_CAPS_LOCK;
                        } else {
                            _modifiers &= !input::MOD_CAPS_LOCK;
                        }
                    }
                }
            } else if event.event_type == EV_ABS {
                match event.code {
                    ABS_X | ABS_Y => {}
                    _ => {}
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
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
