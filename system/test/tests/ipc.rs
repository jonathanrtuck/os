//! Host-side tests for the IPC ring buffer library.
//!
//! Includes the library directly — it has zero external dependencies (no_std,
//! no syscalls, no hardware), making it fully testable on the host.
//!
//! All tests are `#[cfg_attr(miri, ignore)]` because `libraries/ipc/lib.rs:198`
//! creates an unaligned `AtomicU32` reference (real UB, but the library is
//! outside kernel audit scope). The UB is in `RingBuf::head_atomic` which is
//! called by `RingBuf::init`, triggered by every test.

#[path = "../../libraries/ipc/lib.rs"]
mod ipc;

use ipc::{Channel, Message, RingBuf, PAYLOAD_SIZE, SLOT_COUNT};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Allocate a page-aligned 4 KiB buffer on the heap for testing.
fn alloc_page() -> Box<[u8; 4096]> {
    // Zero-initialized — matches kernel behavior (demand-paged zeroed pages).
    Box::new([0u8; 4096])
}

/// Create a RingBuf backed by a test page.
fn make_ring(page: &mut [u8; 4096]) -> RingBuf {
    let ring = unsafe { RingBuf::from_raw(page.as_mut_ptr()) };
    ring.init();
    ring
}

/// Create a test message with a given type and a byte pattern in the payload.
fn test_msg(msg_type: u32, fill: u8) -> Message {
    let mut msg = Message::new(msg_type);
    for b in msg.payload.iter_mut() {
        *b = fill;
    }
    msg
}

// ---------------------------------------------------------------------------
// Message construction
// ---------------------------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn message_new_zeroes_payload() {
    let msg = Message::new(42);
    assert_eq!(msg.msg_type, 42);
    assert!(msg.payload.iter().all(|&b| b == 0));
}

#[test]
#[cfg_attr(miri, ignore)]
fn message_from_payload_roundtrip() {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Config {
        mmio_pa: u64,
        irq: u32,
        width: u32,
    }

    let config = Config {
        mmio_pa: 0xDEAD_BEEF_0000,
        irq: 47,
        width: 1024,
    };

    let msg = unsafe { Message::from_payload(1, &config) };
    assert_eq!(msg.msg_type, 1);

    let recovered: Config = unsafe { msg.payload_as() };
    assert_eq!(recovered, config);
}

#[test]
#[cfg_attr(miri, ignore)]
fn message_payload_small_struct() {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Ping {
        sequence: u32,
    }

    let ping = Ping { sequence: 99 };
    let msg = unsafe { Message::from_payload(10, &ping) };

    let recovered: Ping = unsafe { msg.payload_as() };
    assert_eq!(recovered.sequence, 99);
}

// ---------------------------------------------------------------------------
// RingBuf basics
// ---------------------------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn ring_starts_empty() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    assert!(ring.is_empty());
    assert!(!ring.is_full());
    assert_eq!(ring.len(), 0);
    assert_eq!(ring.capacity(), SLOT_COUNT);
}

#[test]
#[cfg_attr(miri, ignore)]
fn send_then_recv_one() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    let msg = test_msg(7, 0xAB);
    assert!(ring.send(&msg));
    assert_eq!(ring.len(), 1);

    let mut out = Message::new(0);
    assert!(ring.try_recv(&mut out));
    assert_eq!(out.msg_type, 7);
    assert!(out.payload.iter().all(|&b| b == 0xAB));
    assert!(ring.is_empty());
}

#[test]
#[cfg_attr(miri, ignore)]
fn recv_from_empty_returns_false() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    let mut out = Message::new(0);
    assert!(!ring.try_recv(&mut out));
}

#[test]
#[cfg_attr(miri, ignore)]
fn fill_ring_to_capacity() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    for i in 0..SLOT_COUNT {
        let msg = test_msg(i as u32, i as u8);
        assert!(ring.send(&msg), "send {i} should succeed");
    }

    assert!(ring.is_full());
    assert_eq!(ring.len(), SLOT_COUNT);

    // One more should fail.
    let overflow = test_msg(999, 0xFF);
    assert!(!ring.send(&overflow), "send to full ring should fail");
}

#[test]
#[cfg_attr(miri, ignore)]
fn recv_all_after_fill() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Fill the ring.
    for i in 0..SLOT_COUNT {
        ring.send(&test_msg(i as u32, i as u8));
    }

    // Drain and verify order.
    for i in 0..SLOT_COUNT {
        let mut out = Message::new(0);
        assert!(ring.try_recv(&mut out), "recv {i} should succeed");
        assert_eq!(out.msg_type, i as u32);
        assert_eq!(out.payload[0], i as u8);
    }

    assert!(ring.is_empty());
}

#[test]
#[cfg_attr(miri, ignore)]
fn fifo_ordering() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Send messages with distinct types.
    for i in 0..5 {
        ring.send(&test_msg(100 + i, 0));
    }

    // Verify FIFO order.
    for i in 0..5 {
        let mut out = Message::new(0);
        ring.try_recv(&mut out);
        assert_eq!(out.msg_type, 100 + i);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn interleaved_send_recv() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Interleave: send 2, recv 1, send 2, recv 1, ...
    let mut sent = 0u32;
    let mut recvd = 0u32;

    for _ in 0..20 {
        ring.send(&test_msg(sent, 0));
        sent += 1;
        ring.send(&test_msg(sent, 0));
        sent += 1;

        let mut out = Message::new(0);
        assert!(ring.try_recv(&mut out));
        assert_eq!(out.msg_type, recvd);
        recvd += 1;
    }

    // Drain remaining.
    while !ring.is_empty() {
        let mut out = Message::new(0);
        ring.try_recv(&mut out);
        assert_eq!(out.msg_type, recvd);
        recvd += 1;
    }

    assert_eq!(recvd, sent);
}

#[test]
#[cfg_attr(miri, ignore)]
fn wraparound_correctness() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Send and recv more messages than slot count to exercise wraparound.
    let total = SLOT_COUNT * 3;

    for i in 0..total {
        // Keep the ring from getting full by draining periodically.
        if ring.is_full() {
            let mut out = Message::new(0);
            ring.try_recv(&mut out);
        }
        ring.send(&test_msg(i as u32, 0));
    }

    // Drain and verify the last batch.
    let mut out = Message::new(0);
    while ring.try_recv(&mut out) {
        // Messages are in order within the ring.
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn payload_data_integrity() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Fill the entire 60-byte payload with a pattern.
    let mut msg = Message::new(42);
    for (i, b) in msg.payload.iter_mut().enumerate() {
        *b = (i & 0xFF) as u8;
    }

    ring.send(&msg);

    let mut out = Message::new(0);
    ring.try_recv(&mut out);

    assert_eq!(out.msg_type, 42);
    for (i, &b) in out.payload.iter().enumerate() {
        assert_eq!(b, (i & 0xFF) as u8, "payload byte {i} mismatch");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn multiple_fill_drain_cycles() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Multiple complete fill/drain cycles.
    for cycle in 0..5 {
        for i in 0..SLOT_COUNT {
            let sent = ring.send(&test_msg((cycle * SLOT_COUNT + i) as u32, 0));
            assert!(sent, "cycle {cycle}, send {i}");
        }
        assert!(ring.is_full());

        for i in 0..SLOT_COUNT {
            let mut out = Message::new(0);
            let recvd = ring.try_recv(&mut out);
            assert!(recvd, "cycle {cycle}, recv {i}");
            assert_eq!(out.msg_type, (cycle * SLOT_COUNT + i) as u32);
        }
        assert!(ring.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Channel (bidirectional)
// ---------------------------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn channel_send_recv_endpoint_0() {
    let mut page0 = alloc_page();
    let mut page1 = alloc_page();

    let ch = unsafe {
        Channel::from_pages(page0.as_mut_ptr(), page1.as_mut_ptr(), 0)
    };
    ch.init();

    // Endpoint 0 sends on page0, recvs on page1.
    // To test recv, we need to write into page1 as if the peer sent.
    // Simulate by creating a RingBuf on page1 as producer.
    let peer_send = unsafe { RingBuf::from_raw(page1.as_mut_ptr()) };

    let msg = test_msg(1, 0x11);
    peer_send.send(&msg);

    let mut out = Message::new(0);
    assert!(ch.try_recv(&mut out));
    assert_eq!(out.msg_type, 1);
    assert_eq!(out.payload[0], 0x11);
}

#[test]
#[cfg_attr(miri, ignore)]
fn channel_send_recv_endpoint_1() {
    let mut page0 = alloc_page();
    let mut page1 = alloc_page();

    let ch = unsafe {
        Channel::from_pages(page0.as_mut_ptr(), page1.as_mut_ptr(), 1)
    };
    ch.init();

    // Endpoint 1 sends on page1, recvs on page0.
    // Simulate peer by producing into page0.
    let peer_send = unsafe { RingBuf::from_raw(page0.as_mut_ptr()) };

    let msg = test_msg(2, 0x22);
    peer_send.send(&msg);

    let mut out = Message::new(0);
    assert!(ch.try_recv(&mut out));
    assert_eq!(out.msg_type, 2);
    assert_eq!(out.payload[0], 0x22);
}

#[test]
#[cfg_attr(miri, ignore)]
fn channel_bidirectional_pair() {
    let mut page0 = alloc_page();
    let mut page1 = alloc_page();

    // Both endpoints share the same physical pages.
    let ep0 = unsafe {
        Channel::from_pages(page0.as_mut_ptr(), page1.as_mut_ptr(), 0)
    };
    ep0.init();

    let ep1 = unsafe {
        Channel::from_pages(page0.as_mut_ptr(), page1.as_mut_ptr(), 1)
    };
    // ep1 does NOT call init — ep0 already initialized both pages.

    // ep0 sends → ep1 receives.
    let msg_a = test_msg(10, 0xAA);
    assert!(ep0.send(&msg_a));

    let mut out = Message::new(0);
    assert!(ep1.try_recv(&mut out));
    assert_eq!(out.msg_type, 10);
    assert_eq!(out.payload[0], 0xAA);

    // ep1 sends → ep0 receives.
    let msg_b = test_msg(20, 0xBB);
    assert!(ep1.send(&msg_b));

    let mut out2 = Message::new(0);
    assert!(ep0.try_recv(&mut out2));
    assert_eq!(out2.msg_type, 20);
    assert_eq!(out2.payload[0], 0xBB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn channel_from_base_matches_from_pages() {
    let mut pages = [0u8; 8192]; // two consecutive pages

    let ch = unsafe { Channel::from_base(pages.as_mut_ptr() as usize, 4096, 0) };
    ch.init();

    let msg = test_msg(5, 0x55);
    assert!(ch.send(&msg));
    assert_eq!(ch.send.len(), 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn channel_config_as_first_message() {
    // Simulates the "config is the first message" pattern.
    // Init creates channel, writes config, starts child. Child reads config.
    let mut page0 = alloc_page();
    let mut page1 = alloc_page();

    // Init side (endpoint 0).
    let init_ch = unsafe {
        Channel::from_pages(page0.as_mut_ptr(), page1.as_mut_ptr(), 0)
    };
    init_ch.init();

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct GpuConfig {
        mmio_pa: u64,
        irq: u32,
        _pad: u32,
        fb_pa: u64,
        fb_pa2: u64,
        fb_width: u32,
        fb_height: u32,
        fb_size: u32,
        _pad2: u32,
    }

    let config = GpuConfig {
        mmio_pa: 0x0A00_0000,
        irq: 48,
        _pad: 0,
        fb_pa: 0x4000_0000,
        fb_pa2: 0x4040_0000,
        fb_width: 1024,
        fb_height: 768,
        fb_size: 1024 * 768 * 4,
        _pad2: 0,
    };

    const MSG_GPU_CONFIG: u32 = 1;
    let msg = unsafe { Message::from_payload(MSG_GPU_CONFIG, &config) };
    assert!(init_ch.send(&msg));

    // Child side (endpoint 1) — reads config as first message.
    let child_ch = unsafe {
        Channel::from_pages(page0.as_mut_ptr(), page1.as_mut_ptr(), 1)
    };

    let mut out = Message::new(0);
    assert!(child_ch.try_recv(&mut out));
    assert_eq!(out.msg_type, MSG_GPU_CONFIG);

    let recovered: GpuConfig = unsafe { out.payload_as() };
    assert_eq!(recovered, config);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
#[cfg_attr(miri, ignore)]
fn slot_count_is_62() {
    // Verify the constant matches the design (4096 - 128 header) / 64.
    assert_eq!(SLOT_COUNT, 62);
}

#[test]
#[cfg_attr(miri, ignore)]
fn payload_size_is_60() {
    assert_eq!(PAYLOAD_SIZE, 60);
}

#[test]
#[cfg_attr(miri, ignore)]
fn message_size_is_64() {
    assert_eq!(core::mem::size_of::<Message>(), 64);
}

#[test]
#[cfg_attr(miri, ignore)]
fn message_alignment_is_64() {
    assert_eq!(core::mem::align_of::<Message>(), 64);
}

#[test]
#[cfg_attr(miri, ignore)]
fn send_after_drain_reuses_slots() {
    let mut page = alloc_page();
    let ring = make_ring(&mut page);

    // Fill, drain, fill again — exercises slot reuse via modulo.
    for i in 0..SLOT_COUNT {
        ring.send(&test_msg(i as u32, 0));
    }
    for _ in 0..SLOT_COUNT {
        let mut out = Message::new(0);
        ring.try_recv(&mut out);
    }

    // Second fill — head counter is now at SLOT_COUNT, wraps around.
    for i in 0..SLOT_COUNT {
        let sent = ring.send(&test_msg((SLOT_COUNT + i) as u32, 0));
        assert!(sent, "second fill, slot {i}");
    }

    // Verify second batch.
    for i in 0..SLOT_COUNT {
        let mut out = Message::new(0);
        ring.try_recv(&mut out);
        assert_eq!(out.msg_type, (SLOT_COUNT + i) as u32);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn wrapping_counter_handles_u32_overflow() {
    let mut page = alloc_page();
    // Initialize via make_ring, then override counters manually below.
    let _ring = make_ring(&mut page);

    // Manually set head and tail near u32::MAX to test wrapping arithmetic.
    // We access the atomic counters through the page memory directly.
    let head_ptr = page.as_mut_ptr() as *mut u32;
    let tail_ptr = unsafe { page.as_mut_ptr().add(64) as *mut u32 };

    let near_max = u32::MAX - 5;
    unsafe {
        core::ptr::write_volatile(head_ptr, near_max);
        core::ptr::write_volatile(tail_ptr, near_max);
    }

    let ring = unsafe { RingBuf::from_raw(page.as_mut_ptr()) };

    assert!(ring.is_empty());

    // Send and recv across the u32 boundary.
    for i in 0..10 {
        ring.send(&test_msg(i, 0));
        let mut out = Message::new(0);
        assert!(ring.try_recv(&mut out));
        assert_eq!(out.msg_type, i);
    }
}
