//! Protocol definitions for all IPC boundaries in the system.
//!
//! Single source of truth for message types and payload structs. Every
//! component that sends or receives IPC messages imports from here.
//!
//! # Organization
//!
//! One module per protocol boundary:
//!
//! - `device`  — init -> all drivers (device config)
//! - `gpu`     — init <-> GPU driver, compositor -> GPU driver
//! - `input`   — input driver -> compositor
//! - `edit`    — compositor <-> text editor
//! - `compose` — init -> compositor (compositor config)
//! - `editor`  — init -> text editor (editor config)
//! - `fs`      — init <-> 9p driver (filesystem requests)
//! - `present` — compositor -> GPU driver (frame presentation)
//!
//! # Conventions
//!
//! - All payload structs are `#[repr(C)]` and fit within the 60-byte IPC
//!   message payload.
//! - `CHANNEL_SHM_BASE` and `channel_shm_va()` are defined once here.
//! - `DirtyRect` is defined here; the drawing library re-exports it.

#![no_std]

/// Base virtual address where channel shared memory pages are mapped.
/// The kernel's channel is at page 0. Channels created by init start
/// at subsequent 2-page pairs. Must match `kernel/paging.rs`.
pub const CHANNEL_SHM_BASE: usize = 0x4000_0000;

/// Compute the base VA of channel N's shared pages.
/// Each channel occupies 2 consecutive pages (one per direction).
#[inline]
pub fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}

/// A dirty rectangle for the present protocol wire format.
///
/// Layout-compatible with `drawing::DirtyRect`. Both are `repr(C)` with
/// identical fields, so they can be safely transmuted across the IPC
/// boundary. The drawing library owns the full type with methods;
/// this is the protocol's wire representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

// ── device: init -> all drivers ─────────────────────────────────────

pub mod device {
    pub const MSG_DEVICE_CONFIG: u32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct DeviceConfig {
        pub mmio_pa: u64,
        pub irq: u32,
        pub _pad: u32,
    }
}

// ── gpu: init <-> GPU driver ────────────────────────────────────────

pub mod gpu {
    pub const MSG_GPU_CONFIG: u32 = 2;
    pub const MSG_DISPLAY_INFO: u32 = 5;
    pub const MSG_GPU_READY: u32 = 8;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct GpuConfig {
        pub mmio_pa: u64,
        pub irq: u32,
        pub _pad: u32,
        pub fb_pa: u64,
        pub fb_pa2: u64,
        pub fb_width: u32,
        pub fb_height: u32,
        pub fb_size: u32,
        pub _pad2: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct DisplayInfoMsg {
        pub width: u32,
        pub height: u32,
    }
}

// ── input: input driver -> compositor ───────────────────────────────

pub mod input {
    pub const MSG_KEY_EVENT: u32 = 10;
    pub const MSG_POINTER_ABS: u32 = 11;
    pub const MSG_POINTER_BUTTON: u32 = 12;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct KeyEvent {
        pub keycode: u16,
        pub pressed: u8,
        pub ascii: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PointerAbs {
        pub x: u32,
        pub y: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PointerButton {
        pub button: u8,
        pub pressed: u8,
        pub _pad: [u8; 2],
    }
}

// ── edit: compositor <-> text editor ────────────────────────────────

pub mod edit {
    pub const MSG_WRITE_INSERT: u32 = 30;
    pub const MSG_WRITE_DELETE: u32 = 31;
    pub const MSG_CURSOR_MOVE: u32 = 32;
    pub const MSG_SELECTION_UPDATE: u32 = 33;
    pub const MSG_WRITE_DELETE_RANGE: u32 = 34;
    pub const MSG_SET_CURSOR: u32 = 35;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WriteInsert {
        pub position: u32,
        pub byte: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WriteDelete {
        pub position: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WriteDeleteRange {
        pub start: u32,
        pub end: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CursorMove {
        pub position: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SelectionUpdate {
        pub sel_start: u32,
        pub sel_end: u32,
    }
}

// ── compose: init -> compositor ─────────────────────────────────────

pub mod compose {
    pub const MSG_COMPOSITOR_CONFIG: u32 = 3;
    pub const MSG_IMAGE_CONFIG: u32 = 6;
    pub const MSG_ICON_CONFIG: u32 = 7;
    pub const MSG_IMG_ICON_CONFIG: u32 = 9;
    pub const MSG_RTC_CONFIG: u32 = 15;

    /// Compositor configuration. Layout: u64 fields first, then u32
    /// fields, so `size_of::<CompositorConfig>() == 56` (no trailing
    /// alignment padding) and fits within the 60-byte IPC payload.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CompositorConfig {
        pub fb_va: u64,
        pub fb_va2: u64,
        pub doc_va: u64,
        pub mono_font_va: u64,
        pub fb_width: u32,
        pub fb_height: u32,
        pub fb_stride: u32,
        pub doc_capacity: u32,
        pub mono_font_len: u32,
        pub prop_font_len: u32,
    }

    // Guard: must fit within the 60-byte IPC payload.
    const _: () = assert!(core::mem::size_of::<CompositorConfig>() <= 60);

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ImageConfig {
        pub image_va: u64,
        pub image_len: u32,
        pub _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct IconConfig {
        pub icon_va: u64,
        pub icon_len: u32,
        pub _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct RtcConfig {
        pub mmio_pa: u64,
    }
}

// ── editor: init -> text editor ─────────────────────────────────────

pub mod editor {
    pub const MSG_EDITOR_CONFIG: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct EditorConfig {
        pub doc_va: u64,
        pub doc_capacity: u32,
        pub _pad: u32,
    }
}

// ── present: compositor -> GPU driver ───────────────────────────────

pub mod present {
    use crate::DirtyRect;

    pub const MSG_PRESENT: u32 = 20;

    /// Present payload with double-buffering and damage tracking.
    ///
    /// When `rect_count == 0`: full-screen transfer.
    /// When `rect_count > 0`: transfer only the specified dirty rects.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PresentPayload {
        pub buffer_index: u32,
        pub rect_count: u32,
        pub rects: [DirtyRect; 6],
        pub _pad: [u8; 4],
    }
}

// ── fs: init <-> 9p driver ──────────────────────────────────────────

pub mod fs {
    pub const MSG_FS_READ_REQUEST: u32 = 40;
    pub const MSG_FS_READ_RESPONSE: u32 = 41;
}
