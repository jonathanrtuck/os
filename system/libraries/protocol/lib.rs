//! Protocol definitions for all IPC boundaries in the system.
//!
//! Single source of truth for message types and payload structs. Every
//! component that sends or receives IPC messages imports from here.
//!
//! # Organization
//!
//! One module per protocol boundary:
//!
//! - `device`      — init -> all drivers (device config)
//! - `gpu`         — init <-> render service (display info, config)
//! - `input`       — input driver -> core
//! - `edit`        — core <-> text editor
//! - `core_config` — init -> core (core config, scene update signal)
//! - `compose`     — init -> render service (render config)
//! - `editor`      — init -> text editor (editor config)
//! - `fs`          — init <-> 9p driver (filesystem requests)
//! - `present`     — core -> render service (scene update signal)
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
            w: (x1 - x0 as u32) as u16,
            h: (y1 - y0 as u32) as u16,
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
        pub _pad: u32,
    }
    const _: () = assert!(core::mem::size_of::<DeviceConfig>() <= 60);
}

// ── gpu: init <-> GPU driver ────────────────────────────────────────

pub mod gpu {
    pub const MSG_GPU_CONFIG: u32 = 2;
    pub const MSG_DISPLAY_INFO: u32 = 5;
    pub const MSG_GPU_READY: u32 = 8;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct GpuConfig {
        pub mmio_pa: u64,
        pub irq: u32,
        pub _pad: u32,
        pub fb_width: u32,
        pub fb_height: u32,
        pub fb_size: u32,
        /// Number of chunks per buffer (total entries = chunks_per_buf * 2).
        pub chunks_per_buf: u16,
        /// Each chunk is 2^chunk_order pages.
        pub chunk_order: u8,
        pub _pad2: u8,
    }
    const _: () = assert!(core::mem::size_of::<GpuConfig>() <= 60);

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct DisplayInfoMsg {
        pub width: u32,
        pub height: u32,
    }
    const _: () = assert!(core::mem::size_of::<DisplayInfoMsg>() <= 60);
}

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
}

// ── edit: core <-> text editor ──────────────────────────────────────

pub mod edit {
    pub const MSG_WRITE_INSERT: u32 = 30;
    pub const MSG_WRITE_DELETE: u32 = 31;
    pub const MSG_CURSOR_MOVE: u32 = 32;
    pub const MSG_SELECTION_UPDATE: u32 = 33;
    pub const MSG_WRITE_DELETE_RANGE: u32 = 34;
    pub const MSG_SET_CURSOR: u32 = 35;

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
    const _: () = assert!(core::mem::size_of::<SelectionUpdate>() <= 60);
}

// ── core: init -> core (OS service) ─────────────────────────────────

pub mod core_config {
    pub const MSG_CORE_CONFIG: u32 = 50;
    /// Signal-only message (no payload). Core sends this after publishing a
    /// new scene graph frame to notify the render service to read it.
    pub const MSG_SCENE_UPDATED: u32 = 51;

    /// Core process configuration. The core owns documents, layout, input
    /// routing, and scene graph building. It writes to the scene graph in
    /// shared memory and signals the render service when a new frame is ready.
    ///
    /// `fb_width` / `fb_height` are dimensions in points (physical / scale).
    /// The core lays out in point coordinates; the render service scales to pixels.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct CoreConfig {
        pub doc_va: u64,
        pub scene_va: u64,
        pub font_buf_va: u64,
        /// VA of the shared PointerState register (input driver → core).
        /// 0 if no input device is present.
        pub input_state_va: u64,
        pub fb_width: u32,
        pub fb_height: u32,
        pub doc_capacity: u32,
        pub mono_font_len: u32,
        pub sans_font_len: u32,
        pub serif_font_len: u32,
    }

    // Guard: must fit within the 60-byte IPC payload (56 bytes used).
    const _: () = assert!(core::mem::size_of::<CoreConfig>() <= 60);
}

// ── compose: init -> render service ─────────────────────────────────

pub mod compose {
    pub const MSG_COMPOSITOR_CONFIG: u32 = 3;
    pub const MSG_IMAGE_CONFIG: u32 = 6;
    pub const MSG_RTC_CONFIG: u32 = 15;

    /// Render service configuration. The render service delegates rendering
    /// to its backend (CpuBackend or Virgl3D), which owns glyph caches and
    /// rasterization. It reads the scene graph from shared memory and
    /// produces pixels for the display.
    ///
    /// `fb_width` / `fb_height` are physical framebuffer dimensions in pixels.
    /// `fb_stride` is always `fb_width * 4` (BGRA8888) — derived by
    /// the render service, not stored in the config.
    /// `scale_factor` is the fractional display scale (1.0, 1.25, 1.5, 2.0).
    /// f32 represents all common scale factors exactly and fits within
    /// the 60-byte IPC payload. The scene graph is in point coordinates
    /// (physical / scale); the render service multiplies by scale_factor
    /// during rendering.
    /// `font_size` is the font size in points (e.g. 18).
    /// `screen_dpi` is the display DPI (e.g. 96).
    /// `frame_rate` is the target frames per second (e.g. 60).
    ///
    /// Framebuffer VAs are not included — both render services self-allocate
    /// framebuffers via `dma_alloc`.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct CompositorConfig {
        pub scene_va: u64,
        pub font_buf_va: u64,
        pub fb_width: u32,
        pub fb_height: u32,
        pub mono_font_len: u32,
        pub sans_font_len: u32,
        pub serif_font_len: u32,
        pub scale_factor: f32,
        pub frame_rate: u16,
        pub font_size: u16,
        pub screen_dpi: u16,
        pub _pad: u16,
        /// Pointer state register VA (atomic u64, read-only). Metal-render
        /// reads cursor position directly from here for cursor plane commands,
        /// independent of the scene graph.
        pub pointer_state_va: u64,
    }

    // Guard: must fit within the 60-byte IPC payload.
    const _: () = assert!(core::mem::size_of::<CompositorConfig>() <= 60);

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct ImageConfig {
        pub image_va: u64,
        pub image_len: u32,
        pub _pad: u32,
    }
    const _: () = assert!(core::mem::size_of::<ImageConfig>() <= 60);

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct RtcConfig {
        pub mmio_pa: u64,
    }
    const _: () = assert!(core::mem::size_of::<RtcConfig>() <= 60);
}

// ── editor: init -> text editor ─────────────────────────────────────

pub mod editor {
    pub const MSG_EDITOR_CONFIG: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct EditorConfig {
        pub doc_va: u64,
        pub doc_capacity: u32,
        pub _pad: u32,
    }
    const _: () = assert!(core::mem::size_of::<EditorConfig>() <= 60);
}

// ── present: render service internal (legacy, unused) ──────────────

pub mod present {
    use crate::DirtyRect;

    pub const MSG_PRESENT: u32 = 20;
    /// GPU → compositor: the transfer+flush for the last present is done.
    /// The compositor can now reuse the framebuffer that was in-flight.
    pub const MSG_PRESENT_DONE: u32 = 21;

    /// Present payload with double-buffering and damage tracking.
    ///
    /// When `rect_count == 0`: full-screen transfer.
    /// When `rect_count > 0`: transfer only the specified dirty rects.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PresentPayload {
        pub buffer_index: u32,
        pub rect_count: u32,
        pub rects: [DirtyRect; 6],
        pub _pad: [u8; 4],
    }
    const _: () = assert!(core::mem::size_of::<PresentPayload>() <= 60);
}

// ── fs: init <-> 9p driver ──────────────────────────────────────────

pub mod fs {
    /// FS read request. Sent by init to the 9p driver.
    ///
    /// Payload layout (60 bytes, written via raw pointer arithmetic):
    ///   [0..8]   u64  file offset
    ///   [8..12]  u32  byte count to read
    ///   [12..16] u32  path length (bytes, excluding null)
    ///   [16..60] [u8] path (null-terminated, max 43 chars + null)
    ///
    /// Note: a `#[repr(C)]` struct with a `u64` first field requires 8-byte
    /// alignment, causing 4 bytes of end-padding (64 bytes total). Callers
    /// use `write_unaligned`/`read_unaligned` at known offsets instead.
    pub const MSG_FS_READ_REQUEST: u32 = 40;

    /// FS read response. Sent by the 9p driver back to init.
    ///
    /// Payload layout (60 bytes):
    ///   [0..8]   u64  file offset (echoed from request)
    ///   [8..12]  u32  actual bytes read (may be less than requested)
    ///   [12..60] [u8] data (up to 48 bytes per message)
    pub const MSG_FS_READ_RESPONSE: u32 = 41;
}

// ── virgl: Virgl3D protocol constants and command encoding ───────────

pub mod virgl;

// ── metal: Metal-over-virtio command protocol ───────────────────────

pub mod metal;
