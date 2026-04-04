//! Protocol definitions for all IPC boundaries in the system.
//!
//! Single source of truth for message types and payload structs. Every
//! component that sends or receives IPC messages imports from here.
//!
//! # Organization
//!
//! One module per protocol boundary (10 modules):
//!
//! - `init`     — init → any service (config messages)
//! - `device`   — init → drivers (device config)
//! - `input`    — input driver → presenter
//! - `edit`     — editor ↔ document, editor ↔ presenter
//! - `layout`   — presenter ↔ layout
//! - `view`     — presenter → compositor (cursor state, A↔C notifications)
//! - `store`    — document ↔ store service
//! - `decode`   — document ↔ decoders
//! - `content`  — shared memory layout (Content Region)
//! - `metal`    — compositor → hypervisor (includes legacy virgl submodule)
//!
//! # Conventions
//!
//! - All payload structs are `#[repr(C)]` and fit within the 60-byte IPC
//!   message payload.
//! - `CHANNEL_SHM_BASE` and `channel_shm_va()` are defined once here.
//! - `DirtyRect` is defined here; the drawing library re-exports it.

#![no_std]

/// IPC payload size in bytes. Must match `ipc::PAYLOAD_SIZE`.
const PAYLOAD_SIZE: usize = 60;

/// Decode a `#[repr(C)]` payload from raw bytes.
///
/// # Safety
///
/// `T` must be `#[repr(C)]` and `size_of::<T>() <= PAYLOAD_SIZE`. Both are
/// enforced by const assertions on every payload struct in this crate.
#[inline]
unsafe fn decode_payload<T: Copy>(payload: &[u8; PAYLOAD_SIZE]) -> T {
    unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const T) }
}

/// System-wide constants (PAGE_SIZE, BOOTSTRAP_PAGE_VA, BootstrapLayout, etc.).
mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

/// Read the bootstrap layout from the kernel-mapped page.
///
/// # Safety
///
/// The kernel maps the bootstrap page at `BOOTSTRAP_PAGE_VA` before starting
/// any userspace process. This function must only be called from userspace
/// after the process has been started (always true — `_start` is the earliest
/// userspace entry point, and the page is mapped before the thread runs).
#[inline]
unsafe fn bootstrap() -> &'static system_config::BootstrapLayout {
    let ptr = system_config::BOOTSTRAP_PAGE_VA as *const system_config::BootstrapLayout;
    // SAFETY: The kernel maps a single physical page at BOOTSTRAP_PAGE_VA with
    // user-RO permissions before starting the process. The page contains a
    // valid BootstrapLayout (72 bytes, well within one 16 KiB page). The
    // pointer is aligned (page-aligned VA, repr(C) struct). The page is
    // read-only and never deallocated, so the reference is valid for 'static.
    unsafe { &*ptr }
}

/// Base VA where channel shared memory pages are mapped for this process.
#[inline]
pub fn channel_shm_base() -> usize {
    // SAFETY: bootstrap page is always mapped before userspace runs.
    unsafe { bootstrap().channel_shm_base as usize }
}

/// Base VA of the service pack (init only; 0 for other processes).
#[inline]
pub fn service_pack_base() -> usize {
    // SAFETY: bootstrap page is always mapped before userspace runs.
    unsafe { bootstrap().service_pack_base as usize }
}

/// Base VA of the shared memory region for this process.
#[inline]
pub fn shared_memory_base() -> usize {
    // SAFETY: bootstrap page is always mapped before userspace runs.
    unsafe { bootstrap().shared_base as usize }
}

/// Compute the base VA of channel N's shared pages.
/// Each channel occupies 2 consecutive pages (one per direction).
#[inline]
pub fn channel_shm_va(idx: usize) -> usize {
    channel_shm_base() + idx * 2 * system_config::PAGE_SIZE as usize
}

/// A rectangular region of pixels that has been modified.
///
/// Used by the drawing library (damage tracking) and the present protocol
/// (dirty rects in MSG_PRESENT payloads). Defined here as the single source
/// of truth; drawing re-exports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl DirtyRect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub fn union(self, other: DirtyRect) -> DirtyRect {
        if self.w == 0 || self.h == 0 {
            return other;
        }
        if other.w == 0 || other.h == 0 {
            return self;
        }

        let x0 = if self.x < other.x { self.x } else { other.x };
        let y0 = if self.y < other.y { self.y } else { other.y };
        let self_x1 = self.x as u32 + self.w as u32;
        let other_x1 = other.x as u32 + other.w as u32;
        let x1 = if self_x1 > other_x1 {
            self_x1
        } else {
            other_x1
        };
        let self_y1 = self.y as u32 + self.h as u32;
        let other_y1 = other.y as u32 + other.h as u32;
        let y1 = if self_y1 > other_y1 {
            self_y1
        } else {
            other_y1
        };

        DirtyRect {
            x: x0,
            y: y0,
            w: (x1 - x0 as u32).min(u16::MAX as u32) as u16,
            h: (y1 - y0 as u32).min(u16::MAX as u32) as u16,
        }
    }

    pub fn union_all(rects: &[DirtyRect]) -> DirtyRect {
        let mut result = DirtyRect::new(0, 0, 0, 0);
        for &r in rects {
            result = result.union(r);
        }
        result
    }
}

// ── device: init -> all drivers ─────────────────────────────────────

pub mod device {
    pub const MSG_DEVICE_CONFIG: u32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct DeviceConfig {
        pub mmio_pa: u64,
        pub irq: u32,
        /// Kernel channel handle for signaling init.
        pub init_handle: u16,
        /// Kernel channel handle for signaling the connected service (e.g. core).
        /// 0xFFFF if this driver has no service channel.
        pub service_handle: u16,
    }
    const _: () = assert!(core::mem::size_of::<DeviceConfig>() <= 60);

    /// Typed message for the device protocol boundary.
    #[derive(Clone, Copy, Debug)]
    pub enum Message {
        DeviceConfig(DeviceConfig),
    }

    /// Decode a device protocol message. Returns `None` for unknown msg_type.
    pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
        match msg_type {
            MSG_DEVICE_CONFIG => Some(Message::DeviceConfig(unsafe {
                crate::decode_payload(payload)
            })),
            _ => None,
        }
    }
}

// ── input: input driver -> presenter ────────────────────────────────

// ── (remaining inline modules: input, edit; external modules below) ─

// ── input: input driver -> core ─────────────────────────────────────

pub mod input {
    pub const MSG_KEY_EVENT: u32 = 10;
    pub const MSG_POINTER_ABS: u32 = 11;
    pub const MSG_POINTER_BUTTON: u32 = 12;
    /// Config message: VA of the shared PointerState register.
    pub const MSG_POINTER_STATE_CONFIG: u32 = 13;

    /// Modifier key bitmask flags, packed into `KeyEvent.modifiers`.
    pub const MOD_SHIFT: u8 = 1 << 0;
    pub const MOD_CTRL: u8 = 1 << 1;
    pub const MOD_ALT: u8 = 1 << 2;
    pub const MOD_SUPER: u8 = 1 << 3;
    pub const MOD_CAPS_LOCK: u8 = 1 << 4;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct KeyEvent {
        pub keycode: u16,
        pub pressed: u8,
        pub ascii: u8,
        /// Active modifier keys at the time of this event.
        pub modifiers: u8,
        pub _pad: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PointerAbs {
        pub x: u32,
        pub y: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PointerButton {
        pub button: u8,
        pub pressed: u8,
        pub _pad: [u8; 2],
    }

    /// Config message payload: VA of the shared pointer state register.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PointerStateConfig {
        pub state_va: u64,
    }

    /// Shared pointer state register. Lives in init-allocated shared memory.
    ///
    /// The input driver atomically writes `pointer_xy` (packed `(x << 32) | y`)
    /// using a store-release. Core atomically reads it with a load-acquire.
    /// Single atomic u64 — no torn reads, no generation counter needed.
    ///
    /// Coordinates are absolute [0, 32767] from the virtio tablet device.
    #[repr(C)]
    pub struct PointerState {
        /// Packed pointer position: `(x << 32) | y`.
        /// Accessed via AtomicU64 semantics (store-release / load-acquire).
        pub pointer_xy: u64,
    }

    impl PointerState {
        pub const fn pack(x: u32, y: u32) -> u64 {
            ((x as u64) << 32) | (y as u64)
        }
        pub const fn unpack_x(packed: u64) -> u32 {
            (packed >> 32) as u32
        }
        pub const fn unpack_y(packed: u64) -> u32 {
            packed as u32
        }
    }

    const _: () = assert!(core::mem::size_of::<KeyEvent>() <= 60);
    const _: () = assert!(core::mem::size_of::<PointerAbs>() <= 60);
    const _: () = assert!(core::mem::size_of::<PointerButton>() <= 60);
    const _: () = assert!(core::mem::size_of::<PointerStateConfig>() <= 60);
    const _: () = assert!(core::mem::size_of::<PointerState>() == 8);

    #[derive(Clone, Copy, Debug)]
    pub enum Message {
        KeyEvent(KeyEvent),
        PointerAbs(PointerAbs),
        PointerButton(PointerButton),
        PointerStateConfig(PointerStateConfig),
    }

    pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
        match msg_type {
            MSG_KEY_EVENT => Some(Message::KeyEvent(unsafe { crate::decode_payload(payload) })),
            MSG_POINTER_ABS => Some(Message::PointerAbs(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_POINTER_BUTTON => Some(Message::PointerButton(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_POINTER_STATE_CONFIG => Some(Message::PointerStateConfig(unsafe {
                crate::decode_payload(payload)
            })),
            _ => None,
        }
    }
}

// ── edit: editor <-> document, editor <-> presenter ─────────────────

pub mod edit {
    /// Document content format. Used by document service and presenter to
    /// dispatch on text/plain vs text/rich code paths.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DocumentFormat {
        /// text/plain — flat UTF-8 buffer.
        Plain,
        /// text/rich — piece table in shared memory.
        Rich,
    }

    pub const MSG_WRITE_INSERT: u32 = 30;
    pub const MSG_WRITE_DELETE: u32 = 31;
    pub const MSG_CURSOR_MOVE: u32 = 32;
    pub const MSG_SELECTION_UPDATE: u32 = 33;
    pub const MSG_WRITE_DELETE_RANGE: u32 = 34;
    pub const MSG_SET_CURSOR: u32 = 35;
    pub const MSG_STYLE_APPLY: u32 = 36;
    pub const MSG_STYLE_SET_CURRENT: u32 = 37;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct WriteInsert {
        pub position: u32,
        pub byte: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct WriteDelete {
        pub position: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct WriteDeleteRange {
        pub start: u32,
        pub end: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct CursorMove {
        pub position: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct SelectionUpdate {
        pub sel_start: u32,
        pub sel_end: u32,
    }
    const _: () = assert!(core::mem::size_of::<WriteInsert>() <= 60);
    const _: () = assert!(core::mem::size_of::<WriteDelete>() <= 60);
    const _: () = assert!(core::mem::size_of::<WriteDeleteRange>() <= 60);
    const _: () = assert!(core::mem::size_of::<CursorMove>() <= 60);
    /// Apply a style to a byte range in the active document.
    /// Sent by the rich text editor to core.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct StyleApply {
        pub start: u32,
        pub end: u32,
        pub style_id: u8,
        pub _pad: [u8; 3],
    }

    /// Set the active insertion style.
    /// Sent by the rich text editor to core. New text inherits this style.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct StyleSetCurrent {
        pub style_id: u8,
        pub _pad: [u8; 3],
    }

    const _: () = assert!(core::mem::size_of::<SelectionUpdate>() <= 60);
    const _: () = assert!(core::mem::size_of::<StyleApply>() <= 60);
    const _: () = assert!(core::mem::size_of::<StyleSetCurrent>() <= 60);

    #[derive(Clone, Copy, Debug)]
    pub enum Message {
        WriteInsert(WriteInsert),
        WriteDelete(WriteDelete),
        WriteDeleteRange(WriteDeleteRange),
        CursorMove(CursorMove),
        SelectionUpdate(SelectionUpdate),
        SetCursor(CursorMove),
        StyleApply(StyleApply),
        StyleSetCurrent(StyleSetCurrent),
    }

    pub fn decode(msg_type: u32, payload: &[u8; crate::PAYLOAD_SIZE]) -> Option<Message> {
        match msg_type {
            MSG_WRITE_INSERT => Some(Message::WriteInsert(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_WRITE_DELETE => Some(Message::WriteDelete(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_WRITE_DELETE_RANGE => Some(Message::WriteDeleteRange(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_CURSOR_MOVE => Some(Message::CursorMove(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_SELECTION_UPDATE => Some(Message::SelectionUpdate(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_SET_CURSOR => Some(Message::SetCursor(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_STYLE_APPLY => Some(Message::StyleApply(unsafe {
                crate::decode_payload(payload)
            })),
            MSG_STYLE_SET_CURRENT => Some(Message::StyleSetCurrent(unsafe {
                crate::decode_payload(payload)
            })),
            _ => None,
        }
    }
}

// ── External modules (one file per boundary) ────────────────────────

/// Init → service configuration (gpu, core, compositor, editor, doc-model).
pub mod init;

/// View protocol — cursor state (C → compositor), A ↔ C notifications.
pub mod view;

/// Metal-over-virtio command protocol (includes legacy `virgl` submodule).
pub mod metal;

/// Content Region shared memory layout (font data, decoded images).
pub mod content;

/// Store service protocol (document ↔ store service, legacy filesystem).
pub mod store;

/// Decode protocol for content decoder services (PNG, JPEG, etc.).
pub mod decode;

/// Layout protocol — shared memory format and IPC signals.
pub mod layout;
