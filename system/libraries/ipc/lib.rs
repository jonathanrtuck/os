//! IPC ring buffer library — lock-free SPSC message transport.
//!
//! Provides structured messaging over kernel-allocated shared memory pages.
//! Each channel has two pages (one per direction), each containing a SPSC
//! ring buffer of fixed 64-byte messages.
//!
//! # Ring buffer page layout (one direction, 4 KiB)
//!
//! ```text
//! [0..63]      Producer header — head: u32 (monotonic counter) + padding
//! [64..127]    Consumer header — tail: u32 (monotonic counter) + padding
//! [128..4095]  62 message slots × 64 bytes
//! ```
//!
//! Head and tail occupy separate cache lines (64 bytes on AArch64) to
//! avoid false sharing between producer and consumer cores.
//!
//! # Message format (64 bytes = one cache line)
//!
//! ```text
//! [0..3]   msg_type: u32  — protocol-defined message type
//! [4..63]  payload: [u8; 60] — protocol-defined payload
//! ```
//!
//! # Memory ordering
//!
//! Producer: write payload (relaxed), then increment head (release).
//! Consumer: read head (acquire), read payload, then increment tail (release).
//! This is the standard SPSC acquire/release protocol — no CAS needed.
//!
//! # Usage
//!
//! The kernel allocates two shared pages per channel and maps them into both
//! processes. This library provides the ring buffer data structure on top of
//! those pages. The kernel remains ignorant of message format.
//!
//! ```text
//! // Endpoint 0: page 0 = send ring, page 1 = recv ring
//! // Endpoint 1: page 0 = recv ring, page 1 = send ring
//! let ch = Channel::init(base_va, PAGE_SIZE, endpoint);
//! ch.send(&msg);
//! ch.try_recv(&mut buf);
//! ```

#![no_std]

#[cfg(target_os = "none")]
extern crate sys;

use core::sync::atomic::{AtomicU32, Ordering};

/// Byte offset of the first message slot.
const DATA_OFFSET: usize = HEADER_SIZE;
/// Byte offset of the head counter within the page (producer writes).
const HEAD_OFFSET: usize = 0;
/// Header size: two cache lines (head + tail, separated for false-sharing avoidance).
const HEADER_SIZE: usize = 128;
/// Size of a single message slot in bytes (one AArch64 cache line).
const SLOT_SIZE: usize = 64;
/// Byte offset of the tail counter within the page (consumer writes).
const TAIL_OFFSET: usize = 64;

mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

/// Page size (from system_config.rs SSOT).
pub const PAGE_SIZE: usize = system_config::PAGE_SIZE as usize;
/// Maximum payload size within a message (64 - 4 byte type tag).
pub const PAYLOAD_SIZE: usize = 60;
/// Number of message slots per ring buffer page.
pub const SLOT_COUNT: usize = (PAGE_SIZE - HEADER_SIZE) / SLOT_SIZE;

/// A bidirectional IPC channel (send ring + recv ring).
///
/// Wraps two `RingBuf` instances — one for each direction. The endpoint
/// index (0 or 1) determines which page is send vs recv:
///
/// - Endpoint 0: page 0 = send, page 1 = recv
/// - Endpoint 1: page 0 = recv, page 1 = send
pub struct Channel {
    pub send: RingBuf,
    pub recv: RingBuf,
}
/// A 64-byte IPC message (one cache line).
///
/// `msg_type` is a protocol-defined discriminant. The `payload` bytes are
/// interpreted according to `msg_type` — typically by casting to a `#[repr(C)]`
/// struct defined by the protocol crate.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct Message {
    pub msg_type: u32,
    pub payload: [u8; PAYLOAD_SIZE],
}
/// One direction of a ring buffer, backed by a single 4 KiB shared page.
///
/// The page is shared between two processes. Only the producer calls `send`,
/// only the consumer calls `try_recv`. The SPSC protocol ensures safety
/// without locks or CAS.
pub struct RingBuf {
    base: *mut u8,
}

impl Channel {
    /// Create a channel from two shared memory pages.
    ///
    /// `page0` and `page1` are the base addresses of the two shared pages
    /// (mapped by the kernel into both processes). `endpoint` is 0 or 1
    /// (which side of the channel this process owns).
    ///
    /// # Safety
    ///
    /// Both pages must be valid 4 KiB shared mappings. `endpoint` must be 0 or 1.
    pub unsafe fn from_pages(page0: *mut u8, page1: *mut u8, endpoint: u8) -> Self {
        debug_assert!(endpoint <= 1);

        let (send_page, recv_page) = if endpoint == 0 {
            (page0, page1)
        } else {
            (page1, page0)
        };

        Self {
            send: unsafe { RingBuf::from_raw(send_page) },
            recv: unsafe { RingBuf::from_raw(recv_page) },
        }
    }
    /// Create a channel from a base VA and page size.
    ///
    /// Assumes the two pages are at consecutive addresses:
    /// `base_va` and `base_va + page_size`.
    ///
    /// # Safety
    ///
    /// The two pages must be valid shared mappings. `endpoint` must be 0 or 1.
    pub unsafe fn from_base(base_va: usize, page_size: usize, endpoint: u8) -> Self {
        let page0 = base_va as *mut u8;
        let page1 = (base_va + page_size) as *mut u8;

        unsafe { Self::from_pages(page0, page1, endpoint) }
    }
    /// Initialize both ring buffers (zero head/tail counters).
    ///
    /// Call once before the peer process starts. Typically done by the
    /// process that creates the channel (e.g., init).
    pub fn init(&self) {
        self.send.init();
        self.recv.init();
    }
    /// Send a message on this channel. Returns `true` if sent.
    pub fn send(&self, msg: &Message) -> bool {
        self.send.send(msg)
    }
    /// Try to receive a message. Returns `true` if a message was read.
    pub fn try_recv(&self, out: &mut Message) -> bool {
        self.recv.try_recv(out)
    }

    /// Block until a message arrives on this channel.
    ///
    /// Loops `sys::wait` + `try_recv` to handle spurious wakeups correctly.
    /// `handle` is the handle index passed to `sys::wait` (must correspond
    /// to this channel). Returns `true` on success, `false` on syscall error.
    ///
    /// This is the correct way to do synchronous RPC over IPC. Never use
    /// a single `wait` + `try_recv` — signals are level-triggered booleans,
    /// not message counters, and can arrive before the wait call.
    #[cfg(target_os = "none")]
    pub fn recv_blocking(&self, handle: u8, out: &mut Message) -> bool {
        loop {
            if self.try_recv(out) {
                return true;
            }
            if sys::wait(&[handle], u64::MAX).is_err() {
                return false;
            }
        }
    }
}

impl Message {
    /// Create a message with the given type and zeroed payload.
    pub const fn new(msg_type: u32) -> Self {
        Self {
            msg_type,
            payload: [0u8; PAYLOAD_SIZE],
        }
    }

    /// Create a message with type and a payload copied from a `#[repr(C)]` struct.
    ///
    /// # Safety
    ///
    /// `T` must be `#[repr(C)]` and `size_of::<T>() <= PAYLOAD_SIZE`.
    pub unsafe fn from_payload<T: Copy>(msg_type: u32, value: &T) -> Self {
        const { assert!(core::mem::size_of::<T>() <= PAYLOAD_SIZE) }
        let mut msg = Self::new(msg_type);
        let size = core::mem::size_of::<T>();

        let src = value as *const T as *const u8;

        unsafe {
            core::ptr::copy_nonoverlapping(src, msg.payload.as_mut_ptr(), size);
        }

        msg
    }
    /// Interpret the payload as a `#[repr(C)]` struct reference.
    ///
    /// # Safety
    ///
    /// `T` must be `#[repr(C)]`, `size_of::<T>() <= PAYLOAD_SIZE`, and the
    /// payload must contain a valid `T` (written via `from_payload` with the
    /// same type).
    pub unsafe fn payload_as<T: Copy>(&self) -> T {
        const { assert!(core::mem::size_of::<T>() <= PAYLOAD_SIZE) }

        unsafe { core::ptr::read_unaligned(self.payload.as_ptr() as *const T) }
    }
}
impl RingBuf {
    fn head_atomic(&self) -> &AtomicU32 {
        unsafe { &*(self.base.add(HEAD_OFFSET) as *const AtomicU32) }
    }
    fn tail_atomic(&self) -> &AtomicU32 {
        unsafe { &*(self.base.add(TAIL_OFFSET) as *const AtomicU32) }
    }

    /// Number of message slots in this ring buffer.
    pub const fn capacity(&self) -> usize {
        SLOT_COUNT
    }
    /// Wrap an existing shared memory page as a ring buffer.
    ///
    /// # Safety
    ///
    /// `base` must point to a 4 KiB page mapped read/write in this process.
    /// The page must be shared with exactly one other process (the peer).
    pub const unsafe fn from_raw(base: *mut u8) -> Self {
        Self { base }
    }
    /// Initialize a ring buffer page (zeroes head and tail).
    ///
    /// Call this once before any process starts using the ring. Typically
    /// done by init (the process that creates the channel) before starting
    /// the child.
    pub fn init(&self) {
        self.head_atomic().store(0, Ordering::Relaxed);
        self.tail_atomic().store(0, Ordering::Relaxed);
    }
    /// True if no messages are available.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// True if the ring is full (producer cannot send).
    pub fn is_full(&self) -> bool {
        self.len() >= SLOT_COUNT
    }
    /// Number of messages available to read.
    pub fn len(&self) -> usize {
        let head = self.head_atomic().load(Ordering::Acquire);
        let tail = self.tail_atomic().load(Ordering::Relaxed);

        head.wrapping_sub(tail) as usize
    }
    /// Try to send a message. Returns `true` if sent, `false` if ring is full.
    ///
    /// Only the producer should call this.
    pub fn send(&self, msg: &Message) -> bool {
        let head = self.head_atomic().load(Ordering::Relaxed);
        let tail = self.tail_atomic().load(Ordering::Acquire);

        if (head.wrapping_sub(tail)) as usize >= SLOT_COUNT {
            return false; // full
        }

        let slot_index = (head as usize) % SLOT_COUNT;
        let slot_ptr = unsafe { self.base.add(DATA_OFFSET + slot_index * SLOT_SIZE) };

        // Write the message into the slot.
        unsafe {
            core::ptr::copy_nonoverlapping(msg as *const Message as *const u8, slot_ptr, SLOT_SIZE);
        }

        // Publish: increment head with release ordering so the payload
        // is visible to the consumer before the head update.
        self.head_atomic()
            .store(head.wrapping_add(1), Ordering::Release);

        true
    }
    /// Try to receive a message. Returns `true` if a message was read into
    /// `out`, `false` if the ring is empty.
    ///
    /// Only the consumer should call this.
    pub fn try_recv(&self, out: &mut Message) -> bool {
        let tail = self.tail_atomic().load(Ordering::Relaxed);
        let head = self.head_atomic().load(Ordering::Acquire);

        if head == tail {
            return false; // empty
        }

        let slot_index = (tail as usize) % SLOT_COUNT;
        let slot_ptr = unsafe { self.base.add(DATA_OFFSET + slot_index * SLOT_SIZE) };

        // Read the message from the slot.
        unsafe {
            core::ptr::copy_nonoverlapping(slot_ptr, out as *mut Message as *mut u8, SLOT_SIZE);
        }

        // Advance tail with release ordering so the producer sees the
        // freed slot only after we've finished reading.
        self.tail_atomic()
            .store(tail.wrapping_add(1), Ordering::Release);

        true
    }
}
// SAFETY: RingBuf can be transferred between threads (Send) — the new thread
// becomes the sole producer or consumer. Sync is intentionally NOT implemented:
// SPSC correctness requires exactly one producer and one consumer. If RingBuf
// were Sync, multiple threads could concurrently call send() or try_recv(),
// racing on the head/tail counters and corrupting the ring.
unsafe impl Send for RingBuf {}
