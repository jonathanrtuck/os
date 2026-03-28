//! Core — the system's central process.
//!
//! Owns document state, understands content types, performs layout,
//! routes input, and builds the scene graph. The compositor is a
//! downstream consumer that renders the scene graph to pixels.
//!
//! # Responsibilities
//!
//! - Document buffer (sole writer)
//! - Text layout (line breaking, cursor/selection positioning)
//! - Scene graph building (writes to shared memory)
//! - Input routing (keyboard → editor, pointer → hit testing)
//! - Editor communication (receives write requests, sends input events)
//! - Clock / RTC
//! - Scroll management
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: input driver → core (keyboard events)
//! Handle 2: core → compositor (scene update signal)
//! Handle 3: core ↔ editor (input events out, write requests in)
//! Handle 4: second input device (tablet) → core (optional)

#![no_std]
#![no_main]

extern crate alloc;
extern crate animation;
extern crate drawing;
extern crate fonts;
extern crate layout as layout_lib;
extern crate piecetable;
extern crate render;
extern crate scene;

#[path = "blink.rs"]
mod blink;
#[path = "documents.rs"]
mod documents;
#[path = "fallback.rs"]
mod fallback;
#[path = "icons.rs"]
mod icons;
#[path = "input.rs"]
mod input_handling;
#[path = "layout/mod.rs"]
mod layout;
#[path = "scene_state.rs"]
mod scene_state;
#[path = "typography.rs"]
mod typography;

use protocol::{
    compose::{self, ImageConfig, RtcConfig, MSG_IMAGE_CONFIG, MSG_RTC_CONFIG},
    core_config::{
        self, CoreConfig, FrameRateMsg, MSG_CORE_CONFIG, MSG_FRAME_RATE, MSG_SCENE_UPDATED,
    },
    document::{
        DocCommit, DocCreate, DocCreateResult, DocDeleteSnapshot, DocQuery, DocQueryResult,
        DocRead, DocReadDone, DocRestore, DocSnapshot, DocSnapshotResult, MSG_DOC_COMMIT,
        MSG_DOC_CREATE, MSG_DOC_CREATE_RESULT, MSG_DOC_DELETE_SNAPSHOT, MSG_DOC_QUERY,
        MSG_DOC_QUERY_RESULT, MSG_DOC_READ, MSG_DOC_READ_DONE, MSG_DOC_RESTORE,
        MSG_DOC_RESTORE_RESULT, MSG_DOC_SNAPSHOT, MSG_DOC_SNAPSHOT_RESULT,
    },
    edit::{
        self, CursorMove, SelectionUpdate, WriteDelete, WriteDeleteRange, WriteInsert,
        MSG_CURSOR_MOVE, MSG_SELECTION_UPDATE, MSG_SET_CURSOR, MSG_STYLE_APPLY,
        MSG_STYLE_SET_CURRENT, MSG_WRITE_DELETE, MSG_WRITE_DELETE_RANGE, MSG_WRITE_INSERT,
    },
    input::{self, KeyEvent, PointerButton, MSG_KEY_EVENT, MSG_POINTER_BUTTON},
};

/// Clamp a float to [min, max]. Manual implementation for `no_std`.
#[inline]
fn clamp_f32(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Delegates to the canonical implementation in `drawing`.
#[inline]
fn round_f32(x: f32) -> i32 {
    drawing::round_f32(x)
}

/// Document format discriminant — determines which code path core uses
/// for document operations, layout, and scene building.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentFormat {
    /// text/plain — flat UTF-8 buffer (existing path, unchanged).
    Plain,
    /// text/rich — piece table in shared memory.
    Rich,
}

// ── Boot state machine ─────────────────────────────────────────────
//
// Core boots event-driven: publishes a loading scene immediately,
// then handles async init replies (document queries, PNG decode,
// undo snapshot) as events while animating a spinner. Transitions
// to the full scene when all init completes.

/// PNG decode sub-state (two-phase: header query → full decode).
#[derive(Clone, Copy)]
enum DecodePhase {
    /// No image to decode (no image config received).
    None,
    /// Header-only query sent, awaiting dimensions.
    AwaitingHeader,
    /// Full decode request sent, awaiting pixel data.
    AwaitingDecode {
        alloc_offset: u32,
        pixel_bytes: u32,
    },
    /// Decode complete (success or failure).
    Done,
}

/// Document loading sub-state.
#[derive(Clone, Copy)]
enum DocPhase {
    /// Query for text/rich sent, awaiting result.
    QueryRich,
    /// Query for text/plain sent (text/rich not found).
    QueryPlain,
    /// Document found, read request sent.
    Reading {
        file_id: u64,
        detected_format: DocumentFormat,
    },
    /// No document found, create request sent.
    Creating,
    /// Document loaded, undo snapshot request sent.
    AwaitingUndo,
    /// Complete.
    Done,
}

/// Boot-time async init state. Tracks which operations have completed.
struct BootState {
    spinner_angle: f32,
    decode_phase: DecodePhase,
    doc_phase: DocPhase,
    image_ready: bool,
    doc_ready: bool,
    undo_ready: bool,
    /// Image config saved from init channel for decode requests.
    img_file_store_offset: u32,
    img_file_store_length: u32,
}

impl BootState {
    fn all_ready(&self) -> bool {
        self.image_ready && self.doc_ready && self.undo_ready
    }
}

/// Boot timeout: force-transition after 5 seconds regardless of pending replies.
const BOOT_TIMEOUT_NS: u64 = 5_000_000_000;

/// Spinner rotation increment per frame (~5° per tick at 60fps → ~1.2 sec/rev).
const SPINNER_ANGLE_DELTA: f32 = 0.0873;

const COMPOSITOR_HANDLE: sys::ChannelHandle = sys::ChannelHandle(2);
const DECODER_HANDLE: sys::ChannelHandle = sys::ChannelHandle(4);
pub(crate) const DOC_HEADER_SIZE: usize = 64;
const EDITOR_HANDLE: sys::ChannelHandle = sys::ChannelHandle(3);
const FONT_SIZE: u32 = 18;
const FS_HANDLE: sys::ChannelHandle = sys::ChannelHandle(5);
const INPUT_HANDLE: sys::ChannelHandle = sys::ChannelHandle(1);
const INPUT2_HANDLE: sys::ChannelHandle = sys::ChannelHandle(6);
// Keycodes (Linux evdev).
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_A: u16 = 30;
const KEY_B: u16 = 48;
const KEY_I: u16 = 23;
const KEY_LEFTCTRL: u16 = 29;
const KEY_UP: u16 = 103;
const KEY_DOWN: u16 = 108;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_HOME: u16 = 102;
const KEY_END: u16 = 107;
const KEY_PAGEUP: u16 = 104;
const KEY_PAGEDOWN: u16 = 109;
const KEY_DELETE: u16 = 111;
const KEY_Z: u16 = 44;
const KEY_1: u16 = 2;
const KEY_2: u16 = 3;
const MAX_UNDO: usize = 64;
const SHADOW_DEPTH: u32 = 0;
const TEXT_INSET_BOTTOM: u32 = 8;
const TEXT_INSET_TOP: u32 = TITLE_BAR_H + SHADOW_DEPTH + 8;
const TEXT_INSET_X: u32 = 12;
const TITLE_BAR_H: u32 = 36;

/// Undo/redo state: fixed-size ring of COW snapshot IDs.
///
/// `snapshots[0..count]` are valid snapshot IDs in chronological order.
/// `position` is the index of the snapshot matching the current document state.
/// Undo decrements position and restores; redo increments and restores.
/// Editing after undo truncates redo history, then pushes a new snapshot.
struct UndoState {
    snapshots: [u64; MAX_UNDO],
    count: usize,
    position: usize,
}

impl UndoState {
    const fn new() -> Self {
        Self {
            snapshots: [0; MAX_UNDO],
            count: 0,
            position: 0,
        }
    }

    /// Record the initial document state (called once at boot).
    fn set_initial(&mut self, snap_id: u64) {
        self.snapshots[0] = snap_id;
        self.count = 1;
        self.position = 0;
    }

    /// Push a new snapshot after an edit. Truncates any redo history.
    /// Returns the number of discarded snapshot IDs written to `discarded`.
    fn push(&mut self, snap_id: u64, discarded: &mut [u64; MAX_UNDO]) -> usize {
        let mut n = 0;

        // Collect redo history being truncated.
        for i in (self.position + 1)..self.count {
            discarded[n] = self.snapshots[i];
            n += 1;
        }
        self.count = self.position + 1;

        if self.count >= MAX_UNDO {
            // Array full — oldest snapshot evicted.
            discarded[n] = self.snapshots[0];
            n += 1;
            let dst = &mut self.snapshots;
            for i in 0..MAX_UNDO - 1 {
                dst[i] = dst[i + 1];
            }
            self.count -= 1;
            self.position -= 1;
        }

        self.snapshots[self.count] = snap_id;
        self.count += 1;
        self.position = self.count - 1;
        n
    }

    /// Undo: return the snapshot ID to restore, or None if at oldest.
    fn undo(&mut self) -> Option<u64> {
        if self.position > 0 {
            self.position -= 1;
            Some(self.snapshots[self.position])
        } else {
            None
        }
    }

    /// Redo: return the snapshot ID to restore, or None if at newest.
    fn redo(&mut self) -> Option<u64> {
        if self.count > 0 && self.position < self.count - 1 {
            self.position += 1;
            Some(self.snapshots[self.position])
        } else {
            None
        }
    }
}

pub(crate) struct CoreState {
    pub(crate) blink_phase: blink::BlinkPhase,
    pub(crate) blink_phase_start_ms: u64,
    pub(crate) boot_counter: u64,
    /// Character advance in 16.16 fixed-point points.
    /// Single source of truth — same precision as scene ShapedGlyph advances.
    pub(crate) char_w_fx: i32,
    pub(crate) counter_freq: u64,
    pub(crate) cursor_blink_id: Option<animation::AnimationId>,
    pub(crate) cursor_opacity: u8,
    pub(crate) cursor_pos: usize,
    pub(crate) doc_buf: *mut u8,
    pub(crate) doc_capacity: usize,
    /// FileId of the active text document in the store.
    pub(crate) doc_file_id: u64,
    /// Document format (Plain for text/plain, Rich for text/rich).
    pub(crate) doc_format: DocumentFormat,
    pub(crate) doc_len: usize,
    pub(crate) font_data_ptr: *const u8,
    pub(crate) font_data_len: usize,
    pub(crate) font_upem: u16,
    /// Mono font typographic ascent (font units, positive above baseline).
    pub(crate) font_ascender: i16,
    /// Mono font typographic descent (font units, negative below baseline).
    pub(crate) font_descender: i16,
    /// Mono font line gap (font units).
    pub(crate) font_line_gap: i16,
    /// Mono font cap height (font units, from OS/2 sCapHeight). 0 if unavailable.
    pub(crate) font_cap_height: i16,
    pub(crate) sans_font_data_ptr: *const u8,
    pub(crate) sans_font_data_len: usize,
    pub(crate) sans_font_upem: u16,
    /// Sans font typographic ascent (font units, positive above baseline).
    pub(crate) sans_font_ascender: i16,
    /// Sans font typographic descent (font units, negative below baseline).
    pub(crate) sans_font_descender: i16,
    /// Sans font line gap (font units).
    pub(crate) sans_font_line_gap: i16,
    /// Sans font cap height (font units). 0 if unavailable.
    pub(crate) sans_font_cap_height: i16,
    pub(crate) serif_font_data_ptr: *const u8,
    pub(crate) serif_font_data_len: usize,
    pub(crate) serif_font_upem: u16,
    /// Serif font typographic ascent (font units, positive above baseline).
    pub(crate) serif_font_ascender: i16,
    /// Serif font typographic descent (font units, negative below baseline).
    pub(crate) serif_font_descender: i16,
    /// Serif font line gap (font units).
    pub(crate) serif_font_line_gap: i16,
    /// Serif font cap height (font units). 0 if unavailable.
    pub(crate) serif_font_cap_height: i16,
    // Mono italic font.
    pub(crate) mono_italic_font_data_ptr: *const u8,
    pub(crate) mono_italic_font_data_len: usize,
    pub(crate) mono_italic_font_upem: u16,
    pub(crate) mono_italic_font_ascender: i16,
    pub(crate) mono_italic_font_descender: i16,
    pub(crate) mono_italic_font_line_gap: i16,
    pub(crate) mono_italic_font_cap_height: i16,
    // Sans italic font.
    pub(crate) sans_italic_font_data_ptr: *const u8,
    pub(crate) sans_italic_font_data_len: usize,
    pub(crate) sans_italic_font_upem: u16,
    pub(crate) sans_italic_font_ascender: i16,
    pub(crate) sans_italic_font_descender: i16,
    pub(crate) sans_italic_font_line_gap: i16,
    pub(crate) sans_italic_font_cap_height: i16,
    // Serif italic font.
    pub(crate) serif_italic_font_data_ptr: *const u8,
    pub(crate) serif_italic_font_data_len: usize,
    pub(crate) serif_italic_font_upem: u16,
    pub(crate) serif_italic_font_ascender: i16,
    pub(crate) serif_italic_font_descender: i16,
    pub(crate) serif_italic_font_line_gap: i16,
    pub(crate) serif_italic_font_cap_height: i16,
    /// Content Region base VA and size (for PNG decode output).
    pub(crate) content_va: usize,
    pub(crate) content_size: usize,
    /// Free-list allocator for the Content Region data area.
    pub(crate) content_alloc: protocol::content::ContentAllocator,
    /// Current scene graph generation (cached for content entry stamping).
    pub(crate) scene_generation: u32,
    /// Decoded image in the Content Region (set after PNG decode).
    pub(crate) image_content_id: u32,
    pub(crate) image_width: u16,
    pub(crate) image_height: u16,
    /// Which document space receives input (0=text, 1=image).
    pub(crate) active_space: u8,
    /// True when the slide spring is animating between spaces.
    pub(crate) slide_animating: bool,
    /// True on the first frame of a slide animation (clamp dt to frame interval).
    pub(crate) slide_first_frame: bool,
    /// Current slide offset in millipoints (0 = space 0, fb_width*MPT = space 1).
    pub(crate) slide_offset: scene::Mpt,
    /// Spring physics for slide animation.
    pub(crate) slide_spring: animation::Spring,
    /// Target slide offset in millipoints.
    pub(crate) slide_target: scene::Mpt,
    pub(crate) line_h: u32,
    pub(crate) mouse_x: u32,
    pub(crate) mouse_y: u32,
    /// Animation ID for the pointer fade-out (255→0, 300ms EaseOut).
    pub(crate) pointer_fade_id: Option<animation::AnimationId>,
    /// Timestamp (ms) of the last pointer movement event.
    pub(crate) pointer_last_event_ms: u64,
    /// Current pointer cursor opacity (0 = hidden, 255 = fully visible).
    pub(crate) pointer_opacity: u8,
    /// True when the pointer cursor is currently shown (recently moved).
    pub(crate) pointer_visible: bool,
    /// VA of the shared PointerState register (input driver writes, core reads).
    pub(crate) input_state_va: usize,
    /// Last-seen packed pointer_xy value (for change detection).
    pub(crate) last_pointer_xy: u64,
    pub(crate) rtc_mmio_va: usize,
    /// PL031 epoch captured once at boot — never re-read.
    pub(crate) rtc_epoch_at_boot: u64,
    /// CNTVCT value at the moment PL031 was read (for elapsed computation).
    pub(crate) boot_counter_at_rtc_read: u64,
    pub(crate) scroll_animating: bool,
    pub(crate) scroll_offset: scene::Mpt,
    pub(crate) scroll_spring: animation::Spring,
    pub(crate) scroll_target: scene::Mpt,
    pub(crate) sel_end: usize,
    /// Selection anchor: the fixed end of a selection range. When
    /// `has_selection` is true, the visible range is
    /// `[min(anchor, cursor_pos), max(anchor, cursor_pos))`.
    pub(crate) anchor: usize,
    /// True when a selection is active.
    pub(crate) has_selection: bool,
    /// Sticky goal column for Up/Down navigation. Preserved across
    /// consecutive vertical moves, cleared on any horizontal move.
    pub(crate) goal_column: Option<usize>,
    /// Sticky goal x-position (points) for Up/Down navigation in rich text.
    /// Preserved across consecutive vertical moves, cleared on horizontal moves.
    pub(crate) goal_x: Option<f32>,
    /// Cached rich text line layout from the last scene build.
    /// Valid until text changes (next scene build updates it).
    pub(crate) rich_lines: alloc::vec::Vec<layout::RichLine>,
    /// Click state for double/triple-click detection.
    pub(crate) last_click_ms: u64,
    pub(crate) last_click_x: u32,
    pub(crate) last_click_y: u32,
    pub(crate) click_count: u8,
    /// Animation ID for the selection highlight fade-in (0→255).
    pub(crate) selection_fade_id: Option<animation::AnimationId>,
    /// Current selection highlight opacity (animated on selection change).
    pub(crate) selection_opacity: u8,
    pub(crate) sel_start: usize,
    pub(crate) timeline: animation::Timeline,
    pub(crate) timer_active: bool,
    pub(crate) timer_handle: sys::TimerHandle,
}

impl CoreState {
    const fn new() -> Self {
        Self {
            blink_phase: blink::BlinkPhase::VisibleHold,
            blink_phase_start_ms: 0,
            boot_counter: 0,
            char_w_fx: 8 * 65536,
            counter_freq: 0,
            cursor_blink_id: None,
            cursor_opacity: 255,
            cursor_pos: 0,
            doc_buf: core::ptr::null_mut(),
            doc_capacity: 0,
            doc_file_id: 0,
            doc_format: DocumentFormat::Plain,
            doc_len: 0,
            font_data_ptr: core::ptr::null(),
            font_data_len: 0,
            font_upem: 1000,
            font_ascender: 800,
            font_descender: -200,
            font_line_gap: 0,
            font_cap_height: 0,
            sans_font_data_ptr: core::ptr::null(),
            sans_font_data_len: 0,
            sans_font_upem: 1000,
            sans_font_ascender: 800,
            sans_font_descender: -200,
            sans_font_line_gap: 0,
            sans_font_cap_height: 0,
            serif_font_data_ptr: core::ptr::null(),
            serif_font_data_len: 0,
            serif_font_upem: 1000,
            serif_font_ascender: 800,
            serif_font_descender: -200,
            serif_font_line_gap: 0,
            serif_font_cap_height: 0,
            mono_italic_font_data_ptr: core::ptr::null(),
            mono_italic_font_data_len: 0,
            mono_italic_font_upem: 0,
            mono_italic_font_ascender: 0,
            mono_italic_font_descender: 0,
            mono_italic_font_line_gap: 0,
            mono_italic_font_cap_height: 0,
            sans_italic_font_data_ptr: core::ptr::null(),
            sans_italic_font_data_len: 0,
            sans_italic_font_upem: 0,
            sans_italic_font_ascender: 0,
            sans_italic_font_descender: 0,
            sans_italic_font_line_gap: 0,
            sans_italic_font_cap_height: 0,
            serif_italic_font_data_ptr: core::ptr::null(),
            serif_italic_font_data_len: 0,
            serif_italic_font_upem: 0,
            serif_italic_font_ascender: 0,
            serif_italic_font_descender: 0,
            serif_italic_font_line_gap: 0,
            serif_italic_font_cap_height: 0,
            content_va: 0,
            content_size: 0,
            content_alloc: protocol::content::ContentAllocator::empty(),
            scene_generation: 0,
            image_content_id: 0,
            image_width: 0,
            image_height: 0,
            active_space: 0,
            slide_animating: false,
            slide_first_frame: false,
            slide_offset: 0,
            slide_spring: {
                let mut s = animation::Spring::new(0.0, 600.0, 49.0, 1.0);
                s.set_settle_threshold(0.5);
                s
            },
            slide_target: 0,
            line_h: 20,
            mouse_x: 0,
            mouse_y: 0,
            pointer_fade_id: None,
            pointer_last_event_ms: 0,
            pointer_opacity: 0,
            pointer_visible: false,
            input_state_va: 0,
            last_pointer_xy: 0,
            rtc_mmio_va: 0,
            rtc_epoch_at_boot: 0,
            boot_counter_at_rtc_read: 0,
            scroll_animating: false,
            scroll_offset: 0,
            scroll_spring: animation::Spring::snappy(0.0),
            scroll_target: 0,
            sel_end: 0,
            anchor: 0,
            has_selection: false,
            goal_column: None,
            goal_x: None,
            rich_lines: alloc::vec::Vec::new(),
            last_click_ms: 0,
            last_click_x: 0,
            last_click_y: 0,
            click_count: 0,
            selection_fade_id: None,
            selection_opacity: 255,
            sel_start: 0,
            timeline: animation::Timeline::new(),
            timer_active: false,
            timer_handle: sys::TimerHandle(0),
        }
    }
}

struct SyncState(core::cell::UnsafeCell<CoreState>);
// SAFETY: Single-threaded userspace process.
unsafe impl Sync for SyncState {}
static STATE: SyncState = SyncState(core::cell::UnsafeCell::new(CoreState::new()));

pub(crate) fn state() -> &'static mut CoreState {
    // SAFETY: Single-threaded userspace process. No concurrent access.
    unsafe { &mut *STATE.0.get() }
}

/// Access the mono font data slice from shared memory.
fn font_data() -> &'static [u8] {
    let s = state();
    if s.font_data_ptr.is_null() || s.font_data_len == 0 {
        &[]
    } else {
        // SAFETY: font_data_ptr points to font_data_len bytes of shared memory.
        unsafe { core::slice::from_raw_parts(s.font_data_ptr, s.font_data_len) }
    }
}

/// Access the sans font data slice (Inter) from shared memory.
fn sans_font_data() -> &'static [u8] {
    let s = state();
    if s.sans_font_data_ptr.is_null() || s.sans_font_data_len == 0 {
        // Fallback to mono when sans font not available.
        font_data()
    } else {
        // SAFETY: sans_font_data_ptr points to sans_font_data_len bytes of shared memory.
        unsafe { core::slice::from_raw_parts(s.sans_font_data_ptr, s.sans_font_data_len) }
    }
}

/// Access the serif font data slice (Source Serif) from shared memory.
fn serif_font_data() -> &'static [u8] {
    let s = state();
    if s.serif_font_data_ptr.is_null() || s.serif_font_data_len == 0 {
        // Fallback to sans when serif font not available.
        sans_font_data()
    } else {
        // SAFETY: serif_font_data_ptr points to serif_font_data_len bytes of shared memory.
        unsafe { core::slice::from_raw_parts(s.serif_font_data_ptr, s.serif_font_data_len) }
    }
}

/// Access the mono italic font data slice from shared memory.
fn mono_italic_font_data() -> &'static [u8] {
    let s = state();
    if s.mono_italic_font_data_ptr.is_null() || s.mono_italic_font_data_len == 0 {
        &[]
    } else {
        // SAFETY: mono_italic_font_data_ptr points to mapped shared memory.
        unsafe {
            core::slice::from_raw_parts(s.mono_italic_font_data_ptr, s.mono_italic_font_data_len)
        }
    }
}

/// Access the sans italic font data slice (Inter Italic) from shared memory.
fn sans_italic_font_data() -> &'static [u8] {
    let s = state();
    if s.sans_italic_font_data_ptr.is_null() || s.sans_italic_font_data_len == 0 {
        &[]
    } else {
        // SAFETY: sans_italic_font_data_ptr points to mapped shared memory.
        unsafe {
            core::slice::from_raw_parts(s.sans_italic_font_data_ptr, s.sans_italic_font_data_len)
        }
    }
}

/// Access the serif italic font data slice (Source Serif 4 Italic) from shared memory.
fn serif_italic_font_data() -> &'static [u8] {
    let s = state();
    if s.serif_italic_font_data_ptr.is_null() || s.serif_italic_font_data_len == 0 {
        &[]
    } else {
        // SAFETY: serif_italic_font_data_ptr points to mapped shared memory.
        unsafe {
            core::slice::from_raw_parts(s.serif_italic_font_data_ptr, s.serif_italic_font_data_len)
        }
    }
}

/// Construct `RichFonts` from current CoreState font pointers.
/// Used by both scene building (main.rs) and navigation (input.rs).
pub(crate) fn make_rich_fonts() -> scene_state::RichFonts<'static> {
    let s = state();
    scene_state::RichFonts {
        mono_data: font_data(),
        mono_upem: s.font_upem,
        mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
        mono_ascender: s.font_ascender,
        mono_descender: s.font_descender,
        mono_line_gap: s.font_line_gap,
        mono_cap_height: s.font_cap_height,
        sans_data: sans_font_data(),
        sans_upem: s.sans_font_upem,
        sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
        sans_ascender: s.sans_font_ascender,
        sans_descender: s.sans_font_descender,
        sans_line_gap: s.sans_font_line_gap,
        sans_cap_height: s.sans_font_cap_height,
        serif_data: serif_font_data(),
        serif_upem: s.serif_font_upem,
        serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
        serif_ascender: s.serif_font_ascender,
        serif_descender: s.serif_font_descender,
        serif_line_gap: s.serif_font_line_gap,
        serif_cap_height: s.serif_font_cap_height,
        mono_italic_data: mono_italic_font_data(),
        mono_italic_upem: s.mono_italic_font_upem,
        mono_italic_content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
        mono_italic_ascender: s.mono_italic_font_ascender,
        mono_italic_descender: s.mono_italic_font_descender,
        mono_italic_line_gap: s.mono_italic_font_line_gap,
        mono_italic_cap_height: s.mono_italic_font_cap_height,
        sans_italic_data: sans_italic_font_data(),
        sans_italic_upem: s.sans_italic_font_upem,
        sans_italic_content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
        sans_italic_ascender: s.sans_italic_font_ascender,
        sans_italic_descender: s.sans_italic_font_descender,
        sans_italic_line_gap: s.sans_italic_font_line_gap,
        sans_italic_cap_height: s.sans_italic_font_cap_height,
        serif_italic_data: serif_italic_font_data(),
        serif_italic_upem: s.serif_italic_font_upem,
        serif_italic_content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
        serif_italic_ascender: s.serif_italic_font_ascender,
        serif_italic_descender: s.serif_italic_font_descender,
        serif_italic_line_gap: s.serif_italic_font_line_gap,
        serif_italic_cap_height: s.serif_italic_font_cap_height,
    }
}

fn clock_seconds() -> u64 {
    let s = state();
    let freq = s.counter_freq;
    if freq == 0 {
        return 0;
    }
    let now = sys::counter();
    if s.rtc_epoch_at_boot != 0 {
        // PL031 was read once at boot; derive current time from CNTVCT.
        let elapsed_ticks = now - s.boot_counter_at_rtc_read;
        s.rtc_epoch_at_boot + elapsed_ticks / freq
    } else {
        // No RTC — show uptime.
        let boot = s.boot_counter;
        (now - boot) / freq
    }
}
/// Fixed-pitch text layout engine.
///
/// Computes line breaks (hard newlines + soft wrap at max width), cursor
/// mapping (byte offset to/from pixel coordinates), and scroll management.
/// Pure computation — no allocations, no side effects.
struct TextLayout {
    /// Character advance in 16.16 fixed-point points.
    /// This is the single source of truth for character width — derived
    /// from the same formula as scene graph ShapedGlyph advances.
    char_width_fx: i32,
    line_height: u32,
    max_width: u32,
}

impl TextLayout {
    fn cols(&self) -> usize {
        if self.char_width_fx == 0 {
            return 0;
        }
        // chars_per_line = floor(max_width / char_width) using 16.16 math.
        ((self.max_width as i64 * 65536) / self.char_width_fx as i64) as usize
    }

    /// Character advance as f32 points (for APIs that need it).
    fn char_width_pt(&self) -> f32 {
        self.char_width_fx as f32 / 65536.0
    }

    /// Return the visual line number (0-based) for a given byte offset.
    /// Delegates to `byte_to_line_col` for a single wrapping implementation.
    fn byte_to_visual_line(&self, text: &[u8], offset: usize) -> u32 {
        let cols = self.cols();
        if cols == 0 || text.is_empty() {
            return 0;
        }
        let target = if offset > text.len() {
            text.len()
        } else {
            offset
        };
        let (line, _col) = scene_state::byte_to_line_col(text, target, cols);
        line as u32
    }

    /// Compute the scroll offset (in pixels) needed to keep the cursor visible.
    fn scroll_for_cursor(
        &self,
        text: &[u8],
        cursor_offset: usize,
        current_scroll: f32,
        viewport_lines: u32,
    ) -> f32 {
        if viewport_lines == 0 || self.line_height == 0 {
            return 0.0;
        }

        let cursor_line = self.byte_to_visual_line(text, cursor_offset);
        let cursor_pt = cursor_line as f32 * self.line_height as f32;
        let viewport_pt = viewport_lines as f32 * self.line_height as f32;

        if cursor_pt < current_scroll {
            return cursor_pt;
        }

        let last_visible_top = current_scroll + viewport_pt - self.line_height as f32;

        if cursor_pt > last_visible_top {
            return cursor_pt - viewport_pt + self.line_height as f32;
        }

        current_scroll
    }

    /// Map pixel coordinates to a byte offset (hit testing).
    fn xy_to_byte(&self, text: &[u8], x: u32, y: u32) -> usize {
        let cols = self.cols();

        if cols == 0 || text.is_empty() {
            return 0;
        }

        let target_row = y / self.line_height;
        // Use fractional char width for click-to-column mapping.
        let cw_pt = self.char_width_pt();
        let target_col = if cw_pt > 0.0 {
            ((x as f32 + cw_pt * 0.5) / cw_pt) as u32
        } else {
            0
        };
        let mut col = 0usize;
        let mut row = 0u32;

        for (i, &byte) in text.iter().enumerate() {
            if byte == b'\n' {
                if row == target_row {
                    return i;
                }

                row += 1;
                col = 0;

                continue;
            }

            if col >= cols {
                row += 1;
                col = 0;
            }

            if row == target_row && col >= target_col as usize {
                return i;
            }
            if row > target_row {
                return i;
            }

            col += 1;
        }

        text.len()
    }
}

fn content_text_layout(page_w: u32, page_pad: u32) -> TextLayout {
    let s = state();
    TextLayout {
        char_width_fx: s.char_w_fx,
        line_height: s.line_h,
        max_width: page_w.saturating_sub(2 * page_pad),
    }
}
fn create_clock_timer() -> bool {
    let s = state();
    let freq = s.counter_freq;
    let timeout_ns = if freq > 0 {
        let now = sys::counter();
        // Align to the same epoch as clock_seconds() so the timer fires
        // exactly when the displayed second ticks over.
        let epoch = if s.boot_counter_at_rtc_read != 0 {
            s.boot_counter_at_rtc_read
        } else {
            s.boot_counter
        };
        let elapsed_ticks = now - epoch;
        let ticks_this_second = elapsed_ticks % freq;
        let remaining_ticks = freq - ticks_this_second;

        (remaining_ticks as u128 * 1_000_000_000 / freq as u128) as u64
    } else {
        1_000_000_000
    };
    let timeout_ns = if timeout_ns < 10_000_000 {
        1_000_000_000
    } else {
        timeout_ns
    };

    match sys::timer_create(timeout_ns) {
        Ok(handle) => {
            let s = state();
            s.timer_handle = handle;
            s.timer_active = true;
            true
        }
        Err(_) => {
            state().timer_active = false;
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    {
        let s = state();
        s.boot_counter = sys::counter();
        s.counter_freq = sys::counter_freq();
    }

    sys::print(b"  \xF0\x9F\xA7\xA0 core - starting\n");

    // Read core config from init channel.
    // SAFETY: channel_shm_va(0) is the base of the init channel SHM region mapped by the kernel;
    // alignment guaranteed by page-boundary allocation.
    let init_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_CORE_CONFIG {
        sys::print(b"core: no config message\n");
        sys::exit();
    }

    let Some(core_config::Message::CoreConfig(config)) =
        core_config::decode(msg.msg_type, &msg.payload)
    else {
        sys::print(b"core: bad config payload\n");
        sys::exit();
    };
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;

    // Read frame rate from separate message (CoreConfig is full at 56 bytes).
    // Init sends FrameRateMsg immediately after CoreConfig on the same channel.
    let _ = sys::wait(&[0], 100_000_000); // 100ms timeout on init channel
    let frame_rate: u64 = if let Some(core_config::Message::FrameRate(fr)) = init_ch
        .try_recv(&mut msg)
        .then(|| core_config::decode(msg.msg_type, &msg.payload))
        .flatten()
    {
        if fr.frame_rate > 0 {
            fr.frame_rate as u64
        } else {
            60
        }
    } else {
        60
    };
    let frame_interval_ns: u64 = 1_000_000_000 / frame_rate;

    if config.doc_va == 0 || config.scene_va == 0 {
        sys::print(b"core: bad config\n");
        sys::exit();
    }

    {
        let s = state();
        s.doc_buf = config.doc_va as *mut u8;
        s.doc_capacity = config.doc_capacity as usize;
        s.doc_len = 0;
        s.input_state_va = config.input_state_va as usize;
    }
    documents::doc_write_header();

    // Parse fonts from Content Region via registry lookup.
    // Core needs metrics for layout and font data pointers for shaping.
    if config.content_va != 0 && config.content_size > 0 {
        // SAFETY: content_va..+content_size is mapped read-write by init. Header is repr(C).
        let header =
            unsafe { &*(config.content_va as *const protocol::content::ContentRegionHeader) };

        // Store Content Region info and initialize the free-list allocator.
        // Init bump-allocated fonts into [CONTENT_HEADER_SIZE, next_alloc).
        // The allocator manages [next_alloc, content_size) for core's use.
        {
            let s = state();
            s.content_va = config.content_va as usize;
            s.content_size = config.content_size as usize;
            s.content_alloc = protocol::content::ContentAllocator::new(
                header.next_alloc,
                config.content_size as u32,
            );
        }

        // Mono font.
        if let Some(entry) =
            protocol::content::find_entry(header, protocol::content::CONTENT_ID_FONT_MONO)
        {
            let font_ptr = (config.content_va as usize + entry.offset as usize) as *const u8;
            // SAFETY: entry bounds validated by init; content_va region is mapped.
            let font_data = unsafe { core::slice::from_raw_parts(font_ptr, entry.length as usize) };
            {
                let s = state();
                s.font_data_ptr = font_ptr;
                s.font_data_len = entry.length as usize;
            }
            if let Some(fm) = fonts::rasterize::font_metrics(font_data) {
                let upem = fm.units_per_em;
                state().font_upem = upem;
                let asc = fm.ascent as i32;
                let desc = fm.descent as i32;
                let gap = fm.line_gap as i32;
                let size = FONT_SIZE;
                let ascent_pt = ((asc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
                let descent_pt = ((-desc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
                let gap_pt = if gap > 0 {
                    (gap * size as i32 / upem as i32) as u32
                } else {
                    0
                };
                let line_h = ascent_pt + descent_pt + gap_pt;
                let space_gid = fonts::rasterize::glyph_id_for_char(font_data, ' ').unwrap_or(0);
                let (advance_fu, _) =
                    fonts::rasterize::glyph_h_metrics(font_data, space_gid).unwrap_or((0, 0));
                let char_w_fx = (advance_fu as i64 * size as i64 * 65536 / upem as i64) as i32;

                {
                    let s = state();
                    s.char_w_fx = if char_w_fx > 0 { char_w_fx } else { 8 * 65536 };
                    s.line_h = if line_h > 0 { line_h } else { 20 };
                    s.font_ascender = fm.ascent;
                    s.font_descender = fm.descent;
                    s.font_line_gap = fm.line_gap;
                    s.font_cap_height = fm.cap_height;
                }

                sys::print(b"     font metrics loaded\n");
            } else {
                sys::print(b"     warning: font parse failed, using defaults\n");
            }
        }

        // Sans font (Inter) for chrome text.
        if let Some(entry) =
            protocol::content::find_entry(header, protocol::content::CONTENT_ID_FONT_SANS)
        {
            let sans_ptr = (config.content_va as usize + entry.offset as usize) as *const u8;
            // SAFETY: entry bounds validated by init; content_va region is mapped.
            let sans_data = unsafe { core::slice::from_raw_parts(sans_ptr, entry.length as usize) };
            if let Some(fm) = fonts::rasterize::font_metrics(sans_data) {
                let s = state();
                s.sans_font_data_ptr = sans_ptr;
                s.sans_font_data_len = entry.length as usize;
                s.sans_font_upem = fm.units_per_em;
                s.sans_font_ascender = fm.ascent;
                s.sans_font_descender = fm.descent;
                s.sans_font_line_gap = fm.line_gap;
                s.sans_font_cap_height = fm.cap_height;
                sys::print(b"     sans font (Inter) loaded\n");
            } else {
                sys::print(b"     warning: sans font parse failed\n");
            }
        }

        // Serif font (Source Serif 4) for rich text serif content.
        if let Some(entry) =
            protocol::content::find_entry(header, protocol::content::CONTENT_ID_FONT_SERIF)
        {
            let serif_ptr = (config.content_va as usize + entry.offset as usize) as *const u8;
            // SAFETY: entry bounds validated by init; content_va region is mapped.
            let serif_data =
                unsafe { core::slice::from_raw_parts(serif_ptr, entry.length as usize) };
            if let Some(fm) = fonts::rasterize::font_metrics(serif_data) {
                let s = state();
                s.serif_font_data_ptr = serif_ptr;
                s.serif_font_data_len = entry.length as usize;
                s.serif_font_upem = fm.units_per_em;
                s.serif_font_ascender = fm.ascent;
                s.serif_font_descender = fm.descent;
                s.serif_font_line_gap = fm.line_gap;
                s.serif_font_cap_height = fm.cap_height;
                sys::print(b"     serif font loaded\n");
            } else {
                sys::print(b"     warning: serif font parse failed\n");
            }
        }

        // Load mono italic font metrics.
        if let Some(entry) =
            protocol::content::find_entry(header, protocol::content::CONTENT_ID_FONT_MONO_ITALIC)
        {
            let ptr = (config.content_va as usize + entry.offset as usize) as *const u8;
            // SAFETY: entry bounds validated by init; content_va region is mapped.
            let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
            if let Some(fm) = fonts::rasterize::font_metrics(data) {
                let s = state();
                s.mono_italic_font_data_ptr = ptr;
                s.mono_italic_font_data_len = entry.length as usize;
                s.mono_italic_font_upem = fm.units_per_em;
                s.mono_italic_font_ascender = fm.ascent;
                s.mono_italic_font_descender = fm.descent;
                s.mono_italic_font_line_gap = fm.line_gap;
                s.mono_italic_font_cap_height = fm.cap_height;
            }
        }

        // Load sans italic font metrics.
        if let Some(entry) =
            protocol::content::find_entry(header, protocol::content::CONTENT_ID_FONT_SANS_ITALIC)
        {
            let ptr = (config.content_va as usize + entry.offset as usize) as *const u8;
            // SAFETY: entry bounds validated by init; content_va region is mapped.
            let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
            if let Some(fm) = fonts::rasterize::font_metrics(data) {
                let s = state();
                s.sans_italic_font_data_ptr = ptr;
                s.sans_italic_font_data_len = entry.length as usize;
                s.sans_italic_font_upem = fm.units_per_em;
                s.sans_italic_font_ascender = fm.ascent;
                s.sans_italic_font_descender = fm.descent;
                s.sans_italic_font_line_gap = fm.line_gap;
                s.sans_italic_font_cap_height = fm.cap_height;
            }
        }

        // Load serif italic font metrics.
        if let Some(entry) =
            protocol::content::find_entry(header, protocol::content::CONTENT_ID_FONT_SERIF_ITALIC)
        {
            let ptr = (config.content_va as usize + entry.offset as usize) as *const u8;
            // SAFETY: entry bounds validated by init; content_va region is mapped.
            let data = unsafe { core::slice::from_raw_parts(ptr, entry.length as usize) };
            if let Some(fm) = fonts::rasterize::font_metrics(data) {
                let s = state();
                s.serif_italic_font_data_ptr = ptr;
                s.serif_italic_font_data_len = entry.length as usize;
                s.serif_italic_font_upem = fm.units_per_em;
                s.serif_italic_font_ascender = fm.ascent;
                s.serif_italic_font_descender = fm.descent;
                s.serif_italic_font_line_gap = fm.line_gap;
                s.serif_italic_font_cap_height = fm.cap_height;
            }
        }
    }

    // ── Set up IPC channels (needed before async sends) ─────────────
    // SAFETY: channel_shm_va(1..N) are bases of channel SHM regions mapped by the kernel;
    // alignment guaranteed by page-boundary allocation.
    let input_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    let compositor_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    let editor_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(3), ipc::PAGE_SIZE, 0) };
    let decoder_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(DECODER_HANDLE.0 as usize),
            ipc::PAGE_SIZE,
            0,
        )
    };
    let fs_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(FS_HANDLE.0 as usize),
            ipc::PAGE_SIZE,
            0,
        )
    };

    // ── Read remaining init channel messages (fast, already queued) ──

    // Image config — save for async decode, don't block.
    let mut boot = BootState {
        spinner_angle: 0.0,
        decode_phase: DecodePhase::None,
        doc_phase: DocPhase::QueryRich,
        image_ready: true, // default: no image to decode
        doc_ready: false,
        undo_ready: false,
        img_file_store_offset: 0,
        img_file_store_length: 0,
    };

    if let Some(compose::Message::ImageConfig(img_config)) = init_ch
        .try_recv(&mut msg)
        .then(|| compose::decode(msg.msg_type, &msg.payload))
        .flatten()
    {
        if img_config.file_store_length > 0 {
            boot.img_file_store_offset = img_config.file_store_offset;
            boot.img_file_store_length = img_config.file_store_length;
            boot.image_ready = false;
        }
    }

    // RTC config — fast, synchronous (already on init channel).
    if let Some(compose::Message::RtcConfig(rtc_config)) = init_ch
        .try_recv(&mut msg)
        .then(|| compose::decode(msg.msg_type, &msg.payload))
        .flatten()
    {
        if rtc_config.mmio_pa != 0 {
            match sys::device_map(rtc_config.mmio_pa, ipc::PAGE_SIZE as u64) {
                Ok(va) => {
                    state().rtc_mmio_va = va;
                    // SAFETY: va points to memory-mapped PL031 RTC data register.
                    let epoch = unsafe { core::ptr::read_volatile(va as *const u32) };
                    state().rtc_epoch_at_boot = epoch as u64;
                    state().boot_counter_at_rtc_read = sys::counter();
                    sys::print(b"     pl031 rtc mapped\n");
                }
                Err(_) => {
                    sys::print(b"     pl031 rtc map failed\n");
                }
            }
        }
    }

    // ── Build loading scene (first visible frame) ───────────────────
    // SAFETY: scene_va..+TRIPLE_SCENE_SIZE is within the scene SHM region mapped by init;
    // alignment is 1 (u8 slice). scene_va validated non-zero above.
    let scene_buf = unsafe {
        core::slice::from_raw_parts_mut(config.scene_va as *mut u8, scene::TRIPLE_SCENE_SIZE)
    };
    let mut scene = scene_state::SceneState::from_buf(scene_buf);

    scene.build_loading(fb_width, fb_height);

    // Signal compositor — loading scene visible almost immediately.
    let scene_msg = ipc::Message::new(MSG_SCENE_UPDATED);
    compositor_ch.send(&scene_msg);
    let _ = sys::channel_signal(COMPOSITOR_HANDLE);
    sys::print(b"     loading scene published\n");

    // ── Send async init requests (non-blocking) ─────────────────────

    // PNG decode: send header-only query.
    if !boot.image_ready {
        let hdr_req = protocol::decode::DecodeRequest {
            file_offset: boot.img_file_store_offset,
            file_length: boot.img_file_store_length,
            content_offset: 0,
            max_output: 0,
            request_id: 1,
            flags: protocol::decode::DECODE_FLAG_HEADER_ONLY,
        };
        // SAFETY: DecodeRequest is repr(C) and fits in 60-byte payload.
        let req_msg = unsafe {
            ipc::Message::from_payload(protocol::decode::MSG_DECODE_REQUEST, &hdr_req)
        };
        decoder_ch.send(&req_msg);
        let _ = sys::channel_signal(DECODER_HANDLE);
        boot.decode_phase = DecodePhase::AwaitingHeader;
    }

    // Document: send query for text/rich.
    {
        let media = b"text/rich";
        let mut query_payload = DocQuery {
            query_type: 0,
            data_len: media.len() as u32,
            data: [0u8; 48],
        };
        query_payload.data[..media.len()].copy_from_slice(media);
        // SAFETY: DocQuery is repr(C) and fits in 60-byte payload.
        let query_msg = unsafe { ipc::Message::from_payload(MSG_DOC_QUERY, &query_payload) };
        fs_ch.send(&query_msg);
        let _ = sys::channel_signal(FS_HANDLE);
        boot.doc_phase = DocPhase::QueryRich;
    }

    // ── Create animation timer ──────────────────────────────────────
    let mut anim_timer = sys::timer_create(frame_interval_ns).unwrap_or(sys::TimerHandle(255));

    // ── Boot animation loop ─────────────────────────────────────────
    //
    // Multiplex timer ticks (spinner animation) with async init replies
    // (decoder, document service). Exits when all init completes or
    // boot timeout expires.
    let mut undo_state = UndoState::new();
    let boot_start = sys::counter();
    let boot_timeout_ticks = if state().counter_freq > 0 {
        BOOT_TIMEOUT_NS as u128 * state().counter_freq as u128 / 1_000_000_000
    } else {
        u128::MAX
    } as u64;

    loop {
        // Wait on animation timer + async reply channels.
        let mut wait_handles = alloc::vec![anim_timer.0, FS_HANDLE.0];
        if !boot.image_ready {
            wait_handles.push(DECODER_HANDLE.0);
        }
        let _ = sys::wait(&wait_handles, frame_interval_ns);

        // ── Timer tick: rotate spinner ──────────────────────────────
        if let Ok(_) = sys::wait(&[anim_timer.0], 0) {
            let _ = sys::handle_close(anim_timer.0);
            boot.spinner_angle += SPINNER_ANGLE_DELTA;
            scene.update_spinner(boot.spinner_angle);
            compositor_ch.send(&scene_msg);
            let _ = sys::channel_signal(COMPOSITOR_HANDLE);
            // Recreate one-shot timer.
            anim_timer = sys::timer_create(frame_interval_ns)
                .unwrap_or(sys::TimerHandle(255));
        }

        // ── Decoder replies ─────────────────────────────────────────
        while !boot.image_ready && decoder_ch.try_recv(&mut msg) {
            if let Some(protocol::decode::Message::Response(resp)) =
                protocol::decode::decode(msg.msg_type, &msg.payload)
            {
                match boot.decode_phase {
                    DecodePhase::AwaitingHeader => {
                        if resp.status == protocol::decode::DecodeStatus::HeaderOk as u8
                            && resp.width > 0
                            && resp.height > 0
                        {
                            let pixel_bytes = resp.width as u32 * resp.height as u32 * 4;
                            let s = state();
                            if let Some(alloc_offset) = s.content_alloc.allocate(pixel_bytes) {
                                // Send full decode request.
                                let dec_req = protocol::decode::DecodeRequest {
                                    file_offset: boot.img_file_store_offset,
                                    file_length: boot.img_file_store_length,
                                    content_offset: alloc_offset,
                                    max_output: pixel_bytes,
                                    request_id: 2,
                                    flags: 0,
                                };
                                let req_msg = unsafe {
                                    ipc::Message::from_payload(
                                        protocol::decode::MSG_DECODE_REQUEST,
                                        &dec_req,
                                    )
                                };
                                decoder_ch.send(&req_msg);
                                let _ = sys::channel_signal(DECODER_HANDLE);
                                boot.decode_phase = DecodePhase::AwaitingDecode {
                                    alloc_offset,
                                    pixel_bytes,
                                };
                            } else {
                                boot.image_ready = true;
                                boot.decode_phase = DecodePhase::Done;
                            }
                        } else {
                            boot.image_ready = true;
                            boot.decode_phase = DecodePhase::Done;
                        }
                    }
                    DecodePhase::AwaitingDecode {
                        alloc_offset,
                        pixel_bytes,
                    } => {
                        if resp.status == protocol::decode::DecodeStatus::Ok as u8 {
                            // Register decoded image in Content Region.
                            let s = state();
                            // SAFETY: content_va is mapped read-write.
                            let header = unsafe {
                                &mut *(s.content_va
                                    as *mut protocol::content::ContentRegionHeader)
                            };
                            let entry_idx = header.entry_count as usize;
                            if entry_idx < protocol::content::MAX_CONTENT_ENTRIES {
                                let content_id = protocol::content::CONTENT_ID_DYNAMIC_START;
                                header.entries[entry_idx] = protocol::content::ContentEntry {
                                    content_id,
                                    offset: alloc_offset,
                                    length: resp.bytes_written,
                                    class: protocol::content::ContentClass::Pixels as u8,
                                    _pad: [0; 3],
                                    width: resp.width as u16,
                                    height: resp.height as u16,
                                    generation: s.scene_generation,
                                };
                                header.entry_count += 1;
                                s.image_content_id = content_id;
                                s.image_width = resp.width as u16;
                                s.image_height = resp.height as u16;
                                sys::print(b"     PNG decoded into Content Region\n");
                            }
                        } else {
                            state().content_alloc.free(alloc_offset, pixel_bytes);
                            sys::print(b"     PNG decode failed\n");
                        }
                        boot.image_ready = true;
                        boot.decode_phase = DecodePhase::Done;
                    }
                    _ => {}
                }
            }
        }

        // ── Document service replies ────────────────────────────────
        while fs_ch.try_recv(&mut msg) {
            match boot.doc_phase {
                DocPhase::QueryRich => {
                    if let Some(protocol::document::Message::DocQueryResult(result)) =
                        protocol::document::decode(msg.msg_type, &msg.payload)
                    {
                        if result.count > 0 {
                            // Found text/rich — send read request.
                            let s = state();
                            let read_payload = DocRead {
                                file_id: result.file_ids[0],
                                target_va: 0,
                                capacity: s.doc_capacity as u32,
                                _pad: 0,
                            };
                            let read_msg = unsafe {
                                ipc::Message::from_payload(MSG_DOC_READ, &read_payload)
                            };
                            fs_ch.send(&read_msg);
                            let _ = sys::channel_signal(FS_HANDLE);
                            boot.doc_phase = DocPhase::Reading {
                                file_id: result.file_ids[0],
                                detected_format: DocumentFormat::Rich,
                            };
                        } else {
                            // Not found — try text/plain.
                            let media = b"text/plain";
                            let mut query_payload = DocQuery {
                                query_type: 0,
                                data_len: media.len() as u32,
                                data: [0u8; 48],
                            };
                            query_payload.data[..media.len()].copy_from_slice(media);
                            let query_msg = unsafe {
                                ipc::Message::from_payload(MSG_DOC_QUERY, &query_payload)
                            };
                            fs_ch.send(&query_msg);
                            let _ = sys::channel_signal(FS_HANDLE);
                            boot.doc_phase = DocPhase::QueryPlain;
                        }
                    }
                }
                DocPhase::QueryPlain => {
                    if let Some(protocol::document::Message::DocQueryResult(result)) =
                        protocol::document::decode(msg.msg_type, &msg.payload)
                    {
                        if result.count > 0 {
                            // Found text/plain — send read request.
                            let s = state();
                            let read_payload = DocRead {
                                file_id: result.file_ids[0],
                                target_va: 0,
                                capacity: s.doc_capacity as u32,
                                _pad: 0,
                            };
                            let read_msg = unsafe {
                                ipc::Message::from_payload(MSG_DOC_READ, &read_payload)
                            };
                            fs_ch.send(&read_msg);
                            let _ = sys::channel_signal(FS_HANDLE);
                            boot.doc_phase = DocPhase::Reading {
                                file_id: result.file_ids[0],
                                detected_format: DocumentFormat::Plain,
                            };
                        } else {
                            // Neither found — create text/plain.
                            sys::print(b"     creating new text document\n");
                            let media = b"text/plain";
                            let mut create_payload = DocCreate {
                                media_type_len: media.len() as u32,
                                _pad: 0,
                                media_type: [0u8; 52],
                            };
                            create_payload.media_type[..media.len()].copy_from_slice(media);
                            let create_msg = unsafe {
                                ipc::Message::from_payload(MSG_DOC_CREATE, &create_payload)
                            };
                            fs_ch.send(&create_msg);
                            let _ = sys::channel_signal(FS_HANDLE);
                            boot.doc_phase = DocPhase::Creating;
                        }
                    }
                }
                DocPhase::Reading {
                    file_id,
                    detected_format,
                } => {
                    if let Some(protocol::document::Message::DocReadDone(done)) =
                        protocol::document::decode(msg.msg_type, &msg.payload)
                    {
                        if done.status == 0 && done.len > 0 {
                            let s = state();
                            s.doc_file_id = file_id;
                            // SAFETY: doc_buf valid, done.len <= capacity.
                            let content_start = unsafe {
                                core::slice::from_raw_parts(
                                    s.doc_buf.add(DOC_HEADER_SIZE),
                                    done.len as usize,
                                )
                            };
                            if detected_format == DocumentFormat::Rich
                                && done.len >= 64
                                && piecetable::validate(content_start)
                            {
                                s.doc_format = DocumentFormat::Rich;
                                s.doc_len = done.len as usize;
                                s.cursor_pos =
                                    piecetable::cursor_pos(documents::rich_buf_ref()) as usize;
                                documents::doc_write_header();
                                sys::print(b"     rich text document loaded\n");
                            } else {
                                s.doc_format = DocumentFormat::Plain;
                                s.doc_len = done.len as usize;
                                documents::doc_write_header();
                                sys::print(b"     plain text document loaded\n");
                            }
                        } else {
                            state().doc_file_id = file_id;
                            state().doc_format = detected_format;
                        }
                    }
                    boot.doc_ready = true;
                    // Send undo snapshot request.
                    if state().doc_file_id != 0 {
                        let snap_payload = DocSnapshot {
                            file_count: 1,
                            _pad: 0,
                            file_ids: [state().doc_file_id, 0, 0, 0, 0, 0],
                        };
                        let snap_msg = unsafe {
                            ipc::Message::from_payload(MSG_DOC_SNAPSHOT, &snap_payload)
                        };
                        fs_ch.send(&snap_msg);
                        let _ = sys::channel_signal(FS_HANDLE);
                        boot.doc_phase = DocPhase::AwaitingUndo;
                    } else {
                        boot.undo_ready = true;
                        boot.doc_phase = DocPhase::Done;
                    }
                }
                DocPhase::Creating => {
                    if let Some(protocol::document::Message::DocCreateResult(result)) =
                        protocol::document::decode(msg.msg_type, &msg.payload)
                    {
                        if result.status == 0 {
                            state().doc_file_id = result.file_id;
                            state().doc_format = DocumentFormat::Plain;
                            sys::print(b"     text document created\n");
                        } else {
                            sys::print(b"     warning: document create failed\n");
                        }
                    }
                    boot.doc_ready = true;
                    // Send undo snapshot for newly created document.
                    if state().doc_file_id != 0 {
                        let snap_payload = DocSnapshot {
                            file_count: 1,
                            _pad: 0,
                            file_ids: [state().doc_file_id, 0, 0, 0, 0, 0],
                        };
                        let snap_msg = unsafe {
                            ipc::Message::from_payload(MSG_DOC_SNAPSHOT, &snap_payload)
                        };
                        fs_ch.send(&snap_msg);
                        let _ = sys::channel_signal(FS_HANDLE);
                        boot.doc_phase = DocPhase::AwaitingUndo;
                    } else {
                        boot.undo_ready = true;
                        boot.doc_phase = DocPhase::Done;
                    }
                }
                DocPhase::AwaitingUndo => {
                    if let Some(protocol::document::Message::DocSnapshotResult(result)) =
                        protocol::document::decode(msg.msg_type, &msg.payload)
                    {
                        if result.status == 0 {
                            undo_state.set_initial(result.snapshot_id);
                            sys::print(b"     initial undo snapshot taken\n");
                        }
                    }
                    boot.undo_ready = true;
                    boot.doc_phase = DocPhase::Done;
                }
                DocPhase::Done => {
                    // Spurious message after completion — discard.
                }
            }
        }

        // ── Check completion ────────────────────────────────────────
        if boot.all_ready() {
            sys::print(b"     boot init complete\n");
            break;
        }

        // ── Boot timeout ────────────────────────────────────────────
        if sys::counter() - boot_start > boot_timeout_ticks {
            sys::print(b"     boot timeout - proceeding with available data\n");
            boot.undo_ready = true;
            boot.image_ready = true;
            boot.doc_ready = true;
            break;
        }
    }

    // Clean up animation timer.
    let _ = sys::handle_close(anim_timer.0);

    let has_input2 = match sys::wait(&[INPUT2_HANDLE.0], 0) {
        Ok(_) => true,
        Err(sys::SyscallError::WouldBlock) => true,
        _ => false,
    };
    let input2_ch = if has_input2 {
        sys::print(b"     tablet input channel detected\n");
        // SAFETY: same invariant as channel_shm_va(1..3) from_base above.
        Some(unsafe {
            ipc::Channel::from_base(
                protocol::channel_shm_va(INPUT2_HANDLE.0 as usize),
                ipc::PAGE_SIZE,
                1,
            )
        })
    } else {
        None
    };

    // ── Transition: build full scene ────────────────────────────────
    // Content area dimensions (for layout).
    let content_w = fb_width;
    let content_h = fb_height;
    // Build full scene (replaces loading scene).
    let mut time_buf = [0u8; 8];

    documents::format_time_hms(clock_seconds(), &mut time_buf);

    // Compute page dimensions (A4 proportions, centered in content area).
    let content_y = TITLE_BAR_H + SHADOW_DEPTH;
    let content_h = fb_height.saturating_sub(content_y);
    let page_margin_v: u32 = 16;
    let page_height = content_h.saturating_sub(2 * page_margin_v);
    // A4 ratio: width = height × 210/297.
    let page_width = (page_height as u64 * 210 / 297) as u32;
    let page_padding: u32 = 24;

    let scene_cfg = {
        let s = state();
        scene_state::SceneConfig {
            fb_width,
            fb_height,
            title_bar_h: TITLE_BAR_H,
            shadow_depth: SHADOW_DEPTH,
            text_inset_x: page_padding,
            chrome_bg: drawing::CHROME_BG,
            chrome_border: drawing::CHROME_BORDER,
            chrome_title_color: drawing::CHROME_TITLE,
            chrome_clock_color: drawing::CHROME_CLOCK,
            bg_color: drawing::BG_BASE,
            text_color: drawing::TEXT_PRIMARY,
            cursor_color: drawing::TEXT_CURSOR,
            sel_color: drawing::TEXT_SELECTION,
            page_bg: drawing::PAGE_BG,
            page_width,
            page_height,
            font_size: FONT_SIZE as u16,
            char_width_fx: s.char_w_fx,
            line_height: s.line_h,
            font_data: font_data(),
            upem: s.font_upem,
            axes: &[],
            mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
            mono_ascender: s.font_ascender,
            mono_descender: s.font_descender,
            mono_line_gap: s.font_line_gap,
            sans_font_data: sans_font_data(),
            sans_upem: s.sans_font_upem,
            sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
            sans_ascender: s.sans_font_ascender,
            sans_descender: s.sans_font_descender,
            sans_line_gap: s.sans_font_line_gap,
        }
    };

    {
        let s = state();
        let is_rich_doc = s.doc_format == DocumentFormat::Rich;
        let title_label: &[u8] = if is_rich_doc { b"Rich Text" } else { b"Text" };
        scene.build_editor_scene(
            &scene_cfg,
            if is_rich_doc {
                &[]
            } else {
                documents::doc_content()
            },
            s.cursor_pos as u32,
            s.sel_start as u32,
            s.sel_end as u32,
            title_label,
            &time_buf,
            0,
            s.cursor_opacity,
            s.mouse_x,
            s.mouse_y,
            s.pointer_opacity,
            0,
            0,
        );
        // For rich text, immediately rebuild document content with styled layout.
        if is_rich_doc {
            let rich_fonts = scene_state::RichFonts {
                mono_data: font_data(),
                mono_upem: s.font_upem,
                mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
                mono_ascender: s.font_ascender,
                mono_descender: s.font_descender,
                mono_line_gap: s.font_line_gap,
                mono_cap_height: s.font_cap_height,
                sans_data: sans_font_data(),
                sans_upem: s.sans_font_upem,
                sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
                sans_ascender: s.sans_font_ascender,
                sans_descender: s.sans_font_descender,
                sans_line_gap: s.sans_font_line_gap,
                sans_cap_height: s.sans_font_cap_height,
                serif_data: serif_font_data(),
                serif_upem: s.serif_font_upem,
                serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                serif_ascender: s.serif_font_ascender,
                serif_descender: s.serif_font_descender,
                serif_line_gap: s.serif_font_line_gap,
                serif_cap_height: s.serif_font_cap_height,
                mono_italic_data: mono_italic_font_data(),
                mono_italic_upem: s.mono_italic_font_upem,
                mono_italic_content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
                mono_italic_ascender: s.mono_italic_font_ascender,
                mono_italic_descender: s.mono_italic_font_descender,
                mono_italic_line_gap: s.mono_italic_font_line_gap,
                mono_italic_cap_height: s.mono_italic_font_cap_height,
                sans_italic_data: sans_italic_font_data(),
                sans_italic_upem: s.sans_italic_font_upem,
                sans_italic_content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
                sans_italic_ascender: s.sans_italic_font_ascender,
                sans_italic_descender: s.sans_italic_font_descender,
                sans_italic_line_gap: s.sans_italic_font_line_gap,
                sans_italic_cap_height: s.sans_italic_font_cap_height,
                serif_italic_data: serif_italic_font_data(),
                serif_italic_upem: s.serif_italic_font_upem,
                serif_italic_content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
                serif_italic_ascender: s.serif_italic_font_ascender,
                serif_italic_descender: s.serif_italic_font_descender,
                serif_italic_line_gap: s.serif_italic_font_line_gap,
                serif_italic_cap_height: s.serif_italic_font_cap_height,
            };
            let lines = scene.update_rich_document_content(
                &scene_cfg,
                documents::rich_buf_ref(),
                &rich_fonts,
                s.cursor_pos as u32,
                s.sel_start as u32,
                s.sel_end as u32,
                title_label,
                &time_buf,
                0,
                true,
                s.cursor_opacity,
            );
            state().rich_lines = lines;
        }
    }

    // Signal compositor that first frame is ready.
    let scene_msg = ipc::Message::new(MSG_SCENE_UPDATED);

    compositor_ch.send(&scene_msg);

    let _ = sys::channel_signal(COMPOSITOR_HANDLE);

    create_clock_timer();

    // Track line count for incremental scene updates.
    let mut prev_line_count = scene_state::count_lines(documents::doc_content());

    sys::print(b"     entering event loop\n");

    // ctrl_pressed removed — modifier state now in KeyEvent.modifiers.

    let mut prev_ms: u64 = {
        let s = state();
        let freq = s.counter_freq;
        if freq > 0 {
            sys::counter() * 1000 / freq
        } else {
            0
        }
    };

    // ── Undo coalescing ─────────────────────────────────────────────
    //
    // Commit is always immediate (crash safety). Snapshots are debounced:
    // rapid keystrokes within COALESCE_MS produce one undo step, not one
    // per character. The snapshot fires when a typing pause is detected.
    const COALESCE_MS: u64 = 500;
    let mut snapshot_pending = false;
    let mut last_edit_ms: u64 = 0;

    loop {
        let timer_active = state().timer_active;
        let timer_handle = state().timer_handle;
        // Compute wait timeout from active animations and blink phase.
        //
        // Scroll animation: 16ms (60fps) while active.
        // Blink fade (FadeOut/FadeIn): 16ms for smooth opacity changes.
        // Blink holds (VisibleHold/HiddenHold): sleep until next phase transition.
        let now_ms = {
            let s = state();
            let freq = s.counter_freq;
            if freq > 0 {
                sys::counter() * 1000 / freq
            } else {
                0
            }
        };
        // ── Animation timeout ─────────────────────────────────────
        // Single question: is anything visually animating?
        // If yes, wake at display refresh rate for smooth frames.
        // If no, sleep until the next timer event or IPC wake.
        let any_animating =
            state().scroll_animating || state().slide_animating || state().timeline.any_active();

        let timeout_ns: u64 = if any_animating {
            frame_interval_ns
        } else {
            let s = state();
            let elapsed = now_ms.saturating_sub(s.blink_phase_start_ms);
            let remaining_ms = match s.blink_phase {
                blink::BlinkPhase::VisibleHold => blink::BLINK_VISIBLE_MS.saturating_sub(elapsed),
                blink::BlinkPhase::FadeOut => blink::BLINK_FADE_OUT_MS.saturating_sub(elapsed),
                blink::BlinkPhase::HiddenHold => blink::BLINK_HIDDEN_MS.saturating_sub(elapsed),
                blink::BlinkPhase::FadeIn => blink::BLINK_FADE_IN_MS.saturating_sub(elapsed),
            };
            if remaining_ms == 0 {
                1_000_000 // 1ms — transition imminent
            } else {
                remaining_ms.saturating_mul(1_000_000)
            }
        };
        // If a snapshot is pending, wake up in time to fire it.
        let timeout_ns = if snapshot_pending {
            let snap_deadline_ms = last_edit_ms + COALESCE_MS;
            let snap_remaining_ms = snap_deadline_ms.saturating_sub(now_ms);
            let snap_ns = snap_remaining_ms.saturating_mul(1_000_000).max(1_000_000);
            timeout_ns.min(snap_ns)
        } else {
            timeout_ns
        };
        let _ = match (timer_active, has_input2) {
            (true, true) => sys::wait(
                &[
                    INPUT_HANDLE.0,
                    EDITOR_HANDLE.0,
                    timer_handle.0,
                    INPUT2_HANDLE.0,
                ],
                timeout_ns,
            ),
            (true, false) => sys::wait(
                &[INPUT_HANDLE.0, EDITOR_HANDLE.0, timer_handle.0],
                timeout_ns,
            ),
            (false, true) => sys::wait(
                &[INPUT_HANDLE.0, EDITOR_HANDLE.0, INPUT2_HANDLE.0],
                timeout_ns,
            ),
            (false, false) => sys::wait(&[INPUT_HANDLE.0, EDITOR_HANDLE.0], timeout_ns),
        };
        let mut changed = false;
        let mut text_changed = false;
        let mut selection_changed = false;
        let mut context_switched = false;
        let mut timer_fired = false;
        let mut had_user_input = false;
        let mut pointer_position_changed = false;
        let mut undo_requested = false;
        let mut redo_requested = false;

        // Check timer.
        if timer_active {
            if let Ok(_) = sys::wait(&[timer_handle.0], 0) {
                timer_fired = true;

                let _ = sys::handle_close(timer_handle.0);

                create_clock_timer();
            }
        }

        // Process input events.
        while input_ch.try_recv(&mut msg) {
            if let Some(input::Message::KeyEvent(key)) = input::decode(msg.msg_type, &msg.payload) {
                // Intercept Cmd+Z / Cmd+Shift+Z for undo/redo.
                if key.pressed == 1
                    && key.keycode == KEY_Z
                    && key.modifiers & protocol::input::MOD_SUPER != 0
                {
                    if key.modifiers & protocol::input::MOD_SHIFT != 0 {
                        redo_requested = true;
                    } else {
                        undo_requested = true;
                    }
                    had_user_input = true;
                    continue;
                }

                let action = input_handling::process_key_event(
                    &key,
                    state().image_content_id != 0,
                    &editor_ch,
                    fb_width,
                    page_width,
                    page_height,
                    page_padding,
                );

                if action.changed {
                    changed = true;
                }
                if action.text_changed {
                    text_changed = true;
                }
                if action.selection_changed {
                    selection_changed = true;
                }
                if action.context_switched {
                    context_switched = true;
                }
                had_user_input = true;
            }
        }

        // Drain second input channel.
        if let Some(ref ch2) = input2_ch {
            while ch2.try_recv(&mut msg) {
                match msg.msg_type {
                    MSG_KEY_EVENT => {
                        let Some(input::Message::KeyEvent(key)) =
                            input::decode(msg.msg_type, &msg.payload)
                        else {
                            continue;
                        };

                        // Intercept Cmd+Z / Cmd+Shift+Z for undo/redo.
                        if key.pressed == 1
                            && key.keycode == KEY_Z
                            && key.modifiers & protocol::input::MOD_SUPER != 0
                        {
                            if key.modifiers & protocol::input::MOD_SHIFT != 0 {
                                redo_requested = true;
                            } else {
                                undo_requested = true;
                            }
                            had_user_input = true;
                            continue;
                        }

                        let action = input_handling::process_key_event(
                            &key,
                            state().image_content_id != 0,
                            &editor_ch,
                            fb_width,
                            page_width,
                            page_height,
                            page_padding,
                        );

                        if action.changed {
                            changed = true;
                        }
                        if action.text_changed {
                            text_changed = true;
                        }
                        if action.selection_changed {
                            selection_changed = true;
                        }
                        if action.context_switched {
                            context_switched = true;
                        }
                        had_user_input = true;
                    }
                    MSG_POINTER_BUTTON => {
                        let Some(input::Message::PointerButton(btn)) =
                            input::decode(msg.msg_type, &msg.payload)
                        else {
                            continue;
                        };
                        if btn.button == 0 && btn.pressed == 1 {
                            let s = state();
                            let click_x = s.mouse_x;
                            let click_y = s.mouse_y;

                            if click_y >= TITLE_BAR_H && s.active_space == 0 {
                                // Text origin = page position + padding.
                                let page_x = (fb_width - page_width) / 2;
                                let page_y_abs =
                                    content_y + (content_h.saturating_sub(page_height)) / 2;
                                let text_origin_x = page_x + page_padding;
                                let text_origin_y = page_y_abs + page_padding;
                                let rel_x = click_x.saturating_sub(text_origin_x);
                                let rel_y = click_y.saturating_sub(text_origin_y);
                                let adjusted_y =
                                    rel_y + (s.scroll_offset / scene::MPT_PER_PT) as u32;
                                let is_rich = state().doc_format == DocumentFormat::Rich;
                                let byte_pos = if is_rich {
                                    // Rich text: proportional hit test using styled layout.
                                    let tl = documents::rich_text_len();
                                    let mut scratch = alloc::vec![0u8; tl];
                                    documents::rich_copy_text(&mut scratch);
                                    let pt_buf = documents::rich_buf_ref();
                                    let s2 = state();
                                    let dw = page_width.saturating_sub(2 * page_padding);
                                    let rich_fonts = scene_state::RichFonts {
                                        mono_data: font_data(),
                                        mono_upem: s2.font_upem,
                                        mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
                                        mono_ascender: s2.font_ascender,
                                        mono_descender: s2.font_descender,
                                        mono_line_gap: s2.font_line_gap,
                                        mono_cap_height: s2.font_cap_height,
                                        sans_data: sans_font_data(),
                                        sans_upem: s2.sans_font_upem,
                                        sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
                                        sans_ascender: s2.sans_font_ascender,
                                        sans_descender: s2.sans_font_descender,
                                        sans_line_gap: s2.sans_font_line_gap,
                                        sans_cap_height: s2.sans_font_cap_height,
                                        serif_data: serif_font_data(),
                                        serif_upem: s2.serif_font_upem,
                                        serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                                        serif_ascender: s2.serif_font_ascender,
                                        serif_descender: s2.serif_font_descender,
                                        serif_line_gap: s2.serif_font_line_gap,
                                        serif_cap_height: s2.serif_font_cap_height,
                                        mono_italic_data: mono_italic_font_data(),
                                        mono_italic_upem: s2.mono_italic_font_upem,
                                        mono_italic_content_id:
                                            protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
                                        mono_italic_ascender: s2.mono_italic_font_ascender,
                                        mono_italic_descender: s2.mono_italic_font_descender,
                                        mono_italic_line_gap: s2.mono_italic_font_line_gap,
                                        mono_italic_cap_height: s2.mono_italic_font_cap_height,
                                        sans_italic_data: sans_italic_font_data(),
                                        sans_italic_upem: s2.sans_italic_font_upem,
                                        sans_italic_content_id:
                                            protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
                                        sans_italic_ascender: s2.sans_italic_font_ascender,
                                        sans_italic_descender: s2.sans_italic_font_descender,
                                        sans_italic_line_gap: s2.sans_italic_font_line_gap,
                                        sans_italic_cap_height: s2.sans_italic_font_cap_height,
                                        serif_italic_data: serif_italic_font_data(),
                                        serif_italic_upem: s2.serif_italic_font_upem,
                                        serif_italic_content_id:
                                            protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
                                        serif_italic_ascender: s2.serif_italic_font_ascender,
                                        serif_italic_descender: s2.serif_italic_font_descender,
                                        serif_italic_line_gap: s2.serif_italic_font_line_gap,
                                        serif_italic_cap_height: s2.serif_italic_font_cap_height,
                                    };
                                    let mono_fi = layout::FontInfo {
                                        data: font_data(),
                                        upem: s2.font_upem,
                                        content_id: protocol::content::CONTENT_ID_FONT_MONO,
                                        ascender: s2.font_ascender,
                                        descender: s2.font_descender,
                                        line_gap: s2.font_line_gap,
                                        cap_height: s2.font_cap_height,
                                    };
                                    let sans_fi = layout::FontInfo {
                                        data: sans_font_data(),
                                        upem: s2.sans_font_upem,
                                        content_id: protocol::content::CONTENT_ID_FONT_SANS,
                                        ascender: s2.sans_font_ascender,
                                        descender: s2.sans_font_descender,
                                        line_gap: s2.sans_font_line_gap,
                                        cap_height: s2.sans_font_cap_height,
                                    };
                                    let serif_fi = layout::FontInfo {
                                        data: serif_font_data(),
                                        upem: s2.serif_font_upem,
                                        content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                                        ascender: s2.serif_font_ascender,
                                        descender: s2.serif_font_descender,
                                        line_gap: s2.serif_font_line_gap,
                                        cap_height: s2.serif_font_cap_height,
                                    };
                                    let rich_lines = layout::layout_rich_lines(
                                        pt_buf,
                                        &mut scratch,
                                        dw as f32,
                                        scene_cfg.line_height as i32,
                                        &mono_fi,
                                        &sans_fi,
                                        &serif_fi,
                                    );
                                    layout::rich_xy_to_byte(
                                        pt_buf,
                                        &scratch,
                                        rel_x as f32,
                                        adjusted_y as f32,
                                        &rich_lines,
                                        &rich_fonts,
                                    )
                                } else {
                                    let li = content_text_layout(page_width, page_padding);
                                    let t = documents::doc_content();
                                    li.xy_to_byte(t, rel_x, adjusted_y)
                                };

                                // Extract text for double/triple-click word/line boundary ops.
                                let click_text_buf: alloc::vec::Vec<u8>;
                                let text: &[u8] = if is_rich {
                                    let tl = documents::rich_text_len();
                                    click_text_buf = {
                                        let mut v = alloc::vec![0u8; tl];
                                        documents::rich_copy_text(&mut v);
                                        v
                                    };
                                    &click_text_buf
                                } else {
                                    click_text_buf = alloc::vec::Vec::new();
                                    documents::doc_content()
                                };
                                let layout_info = content_text_layout(page_width, page_padding);

                                // Double/triple-click detection.
                                // 400ms window, within 4pt of previous click.
                                let dx = if click_x > s.last_click_x {
                                    click_x - s.last_click_x
                                } else {
                                    s.last_click_x - click_x
                                };
                                let dy = if click_y > s.last_click_y {
                                    click_y - s.last_click_y
                                } else {
                                    s.last_click_y - click_y
                                };
                                let dt = now_ms.saturating_sub(s.last_click_ms);
                                let same_spot = dx <= 4 && dy <= 4 && dt <= 400;

                                let click_count = if same_spot {
                                    // Cycle: 1 → 2 → 3 → 1 → ...
                                    (s.click_count % 3) + 1
                                } else {
                                    1
                                };

                                {
                                    let s = state();
                                    s.last_click_ms = now_ms;
                                    s.last_click_x = click_x;
                                    s.last_click_y = click_y;
                                    s.click_count = click_count;
                                }

                                let cols = layout_info.cols();

                                match click_count {
                                    2 => {
                                        // Double-click: select word at click position.
                                        // byte_pos is a cursor position (between characters).
                                        // If it lands at the start of a word, adjust backward
                                        // search so it finds THIS word, not the previous one.
                                        let at_word = byte_pos < text.len()
                                            && !layout_lib::is_whitespace(text[byte_pos]);
                                        let back_pos =
                                            if at_word { byte_pos + 1 } else { byte_pos };
                                        let lo =
                                            input_handling::word_boundary_backward(text, back_pos);
                                        // Forward: find end of word only (exclude trailing
                                        // whitespace — word_boundary_forward includes it
                                        // because it's designed for Opt+Right navigation).
                                        let mut hi = byte_pos;
                                        while hi < text.len()
                                            && !layout_lib::is_whitespace(text[hi])
                                        {
                                            hi += 1;
                                        }
                                        let s = state();
                                        s.anchor = lo;
                                        s.cursor_pos = hi;
                                        s.has_selection = hi > lo;
                                        input_handling::update_selection_from_anchor();
                                    }
                                    3 => {
                                        // Triple-click: select entire visual line.
                                        let lo =
                                            input_handling::visual_line_start(text, byte_pos, cols);
                                        let mut hi =
                                            input_handling::visual_line_end(text, byte_pos, cols);
                                        // Include the newline if present.
                                        if hi < text.len() && text[hi] == b'\n' {
                                            hi += 1;
                                        }
                                        let s = state();
                                        s.anchor = lo;
                                        s.cursor_pos = hi;
                                        s.has_selection = hi > lo;
                                        input_handling::update_selection_from_anchor();
                                    }
                                    _ => {
                                        // Single click: position cursor, clear selection.
                                        let s = state();
                                        s.cursor_pos = byte_pos;
                                        input_handling::clear_selection();
                                    }
                                }

                                state().goal_column = None;
                                state().goal_x = None;
                                documents::doc_write_header();
                                input_handling::sync_cursor_to_editor(&editor_ch);

                                let _ = sys::channel_signal(EDITOR_HANDLE);

                                changed = true;
                                selection_changed = true;
                                had_user_input = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Read pointer state register ─────────────────────────────
        //
        // The input driver writes pointer position to a shared memory
        // register (atomic u64). We read it once after draining all event
        // rings. This replaces MSG_POINTER_ABS — no ring overflow possible.
        // ── Read pointer state register ─────────────────────────────
        //
        // The input driver writes pointer position to a shared memory
        // register (atomic u64). We read it once after draining all event
        // rings. This replaces MSG_POINTER_ABS — no ring overflow possible.
        {
            let s = state();
            // SAFETY: input_state_va points to a PointerState page mapped
            // by init. Atomic load-acquire for cross-core visibility.
            let packed = unsafe {
                let atom = &*(s.input_state_va as *const core::sync::atomic::AtomicU64);
                atom.load(core::sync::atomic::Ordering::Acquire)
            };
            if packed != s.last_pointer_xy && packed != 0 {
                s.last_pointer_xy = packed;
                let x = protocol::input::PointerState::unpack_x(packed);
                let y = protocol::input::PointerState::unpack_y(packed);
                s.mouse_x = scale_pointer_coord(x, fb_width);
                s.mouse_y = scale_pointer_coord(y, fb_height);

                // Cancel any pending fade-out and restore full opacity.
                if let Some(id) = s.pointer_fade_id {
                    s.timeline.cancel(id);
                    s.pointer_fade_id = None;
                }
                s.pointer_visible = true;
                // Only trigger scene publish when opacity actually changes
                // (pointer was fading/hidden). Position-only changes are
                // handled by the render service reading the shared register.
                if s.pointer_opacity != 255 {
                    s.pointer_opacity = 255;
                    changed = true;
                }
                s.pointer_last_event_ms = now_ms;

                pointer_position_changed = true;
            }
        }

        // Process editor write requests.
        let is_rich = state().doc_format == DocumentFormat::Rich;
        while editor_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_WRITE_INSERT => {
                    let Some(edit::Message::WriteInsert(insert)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let pos = insert.position as usize;

                    let ok = if is_rich {
                        documents::rich_insert(pos, insert.byte)
                    } else {
                        documents::doc_insert(pos, insert.byte)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = pos + 1;
                        if is_rich {
                            documents::rich_set_cursor_pos(s.cursor_pos);
                        }

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    let Some(edit::Message::WriteDelete(del)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let pos = del.position as usize;

                    let ok = if is_rich {
                        documents::rich_delete(pos)
                    } else {
                        documents::doc_delete(pos)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = pos;
                        if is_rich {
                            documents::rich_set_cursor_pos(s.cursor_pos);
                        }

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_CURSOR_MOVE => {
                    let Some(edit::Message::CursorMove(cm)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let pos = cm.position as usize;
                    let doc_text_len = if is_rich {
                        documents::rich_text_len()
                    } else {
                        state().doc_len
                    };

                    if pos <= doc_text_len {
                        state().cursor_pos = pos;

                        if is_rich {
                            documents::rich_set_cursor_pos(pos);
                        } else {
                            documents::doc_write_header();
                        }

                        changed = true;
                    }
                }
                MSG_SELECTION_UPDATE => {
                    let Some(edit::Message::SelectionUpdate(su)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let s = state();
                    s.sel_start = su.sel_start as usize;
                    s.sel_end = su.sel_end as usize;

                    changed = true;
                    selection_changed = true;
                }
                MSG_WRITE_DELETE_RANGE => {
                    let Some(edit::Message::WriteDeleteRange(dr)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    let start = dr.start as usize;
                    let end = dr.end as usize;

                    let ok = if is_rich {
                        documents::rich_delete_range(start, end)
                    } else {
                        documents::doc_delete_range(start, end)
                    };
                    if ok {
                        let s = state();
                        s.cursor_pos = start;
                        if is_rich {
                            documents::rich_set_cursor_pos(s.cursor_pos);
                        }

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_STYLE_APPLY => {
                    // Only valid for rich text documents.
                    if !is_rich {
                        continue;
                    }
                    let Some(edit::Message::StyleApply(sa)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    documents::rich_apply_style(sa.start as usize, sa.end as usize, sa.style_id);
                    // Style changes modify the piece table — trigger rebuild.
                    changed = true;
                    text_changed = true;
                }
                MSG_STYLE_SET_CURRENT => {
                    if !is_rich {
                        continue;
                    }
                    let Some(edit::Message::StyleSetCurrent(sc)) =
                        edit::decode(msg.msg_type, &msg.payload)
                    else {
                        continue;
                    };
                    documents::rich_set_current_style(sc.style_id);
                    // No scene rebuild needed — only affects future insertions.
                }
                _ => {}
            }
        }

        // ── Persist + snapshot / undo / redo ────────────────────────────
        //
        // Commit is always immediate (crash safety). Snapshots are debounced:
        // rapid keystrokes within COALESCE_MS produce one undo step instead
        // of one per character. The snapshot fires when a typing pause is
        // detected, or immediately when undo/redo is requested (flush).
        //
        // Helper: take a snapshot and push it onto the undo stack.
        // Defined as a macro to avoid borrow conflicts with `fs_ch`.
        macro_rules! take_snapshot {
            () => {{
                let file_id = state().doc_file_id;
                let snap_payload = DocSnapshot {
                    file_count: 1,
                    _pad: 0,
                    file_ids: [file_id, 0, 0, 0, 0, 0],
                };
                // SAFETY: DocSnapshot is repr(C) and fits in 60-byte payload.
                let snap_msg =
                    unsafe { ipc::Message::from_payload(MSG_DOC_SNAPSHOT, &snap_payload) };
                fs_ch.send(&snap_msg);
                let _ = sys::channel_signal(FS_HANDLE);

                let mut reply = ipc::Message::new(0);
                if fs_ch.recv_blocking(FS_HANDLE.0, &mut reply)
                    && reply.msg_type == MSG_DOC_SNAPSHOT_RESULT
                {
                    if let Some(protocol::document::Message::DocSnapshotResult(result)) =
                        protocol::document::decode(reply.msg_type, &reply.payload)
                    {
                        if result.status == 0 {
                            let mut discarded = [0u64; MAX_UNDO];
                            let n = undo_state.push(result.snapshot_id, &mut discarded);
                            for &snap in &discarded[..n] {
                                let del = DocDeleteSnapshot { snapshot_id: snap };
                                // SAFETY: DocDeleteSnapshot is repr(C), fits in 60 bytes.
                                let del_msg = unsafe {
                                    ipc::Message::from_payload(MSG_DOC_DELETE_SNAPSHOT, &del)
                                };
                                fs_ch.send(&del_msg);
                            }
                            if n > 0 {
                                let _ = sys::channel_signal(FS_HANDLE);
                            }
                        }
                    }
                }
            }};
        }

        if text_changed {
            let file_id = state().doc_file_id;

            // Commit current content to disk (always immediate, fire-and-forget).
            let commit_payload = DocCommit { file_id };
            // SAFETY: DocCommit is repr(C) and fits in 60-byte payload.
            let commit_msg = unsafe { ipc::Message::from_payload(MSG_DOC_COMMIT, &commit_payload) };
            fs_ch.send(&commit_msg);
            let _ = sys::channel_signal(FS_HANDLE);

            // Mark snapshot as pending — it will fire after a typing pause.
            snapshot_pending = true;
            last_edit_ms = now_ms;
        }

        // Flush pending snapshot on typing pause (coalesce window elapsed).
        if snapshot_pending && !text_changed && now_ms.saturating_sub(last_edit_ms) >= COALESCE_MS {
            // Advance piece table operation_id at snapshot boundary.
            if state().doc_format == DocumentFormat::Rich {
                documents::rich_next_operation();
            }
            take_snapshot!();
            snapshot_pending = false;
        }

        // Flush pending snapshot immediately before undo/redo so the
        // current editing burst is undoable as a single step.
        if snapshot_pending && (undo_requested || redo_requested) {
            if state().doc_format == DocumentFormat::Rich {
                documents::rich_next_operation();
            }
            take_snapshot!();
            snapshot_pending = false;
        }

        if undo_requested {
            if let Some(snap_id) = undo_state.undo() {
                // Restore the snapshot.
                let restore_payload = DocRestore {
                    snapshot_id: snap_id,
                };
                // SAFETY: DocRestore is repr(C) and fits in 60-byte payload.
                let restore_msg =
                    unsafe { ipc::Message::from_payload(MSG_DOC_RESTORE, &restore_payload) };
                fs_ch.send(&restore_msg);
                let _ = sys::channel_signal(FS_HANDLE);

                let mut reply = ipc::Message::new(0);
                if fs_ch.recv_blocking(FS_HANDLE.0, &mut reply)
                    && reply.msg_type == MSG_DOC_RESTORE_RESULT
                {
                    if let Some(protocol::document::Message::DocRestoreResult(result)) =
                        protocol::document::decode(reply.msg_type, &reply.payload)
                    {
                        if result.status == 0 {
                            // Reload document content from the restored snapshot.
                            let s = state();
                            let read_payload = DocRead {
                                file_id: s.doc_file_id,
                                target_va: 0,
                                capacity: s.doc_capacity as u32,
                                _pad: 0,
                            };
                            // SAFETY: DocRead is repr(C) and fits in 60-byte payload.
                            let read_msg =
                                unsafe { ipc::Message::from_payload(MSG_DOC_READ, &read_payload) };
                            fs_ch.send(&read_msg);
                            let _ = sys::channel_signal(FS_HANDLE);

                            if fs_ch.recv_blocking(FS_HANDLE.0, &mut reply)
                                && reply.msg_type == MSG_DOC_READ_DONE
                            {
                                if let Some(protocol::document::Message::DocReadDone(done)) =
                                    protocol::document::decode(reply.msg_type, &reply.payload)
                                {
                                    if done.status == 0 {
                                        let s = state();
                                        s.doc_len = done.len as usize;
                                        if s.doc_format == DocumentFormat::Rich {
                                            // Restore cursor from piece table header.
                                            let text_len = documents::rich_text_len();
                                            s.cursor_pos = documents::rich_cursor_pos();
                                            if s.cursor_pos > text_len {
                                                s.cursor_pos = text_len;
                                            }
                                        } else {
                                            if s.cursor_pos > s.doc_len {
                                                s.cursor_pos = s.doc_len;
                                            }
                                        }
                                        input_handling::clear_selection();
                                        if s.doc_format != DocumentFormat::Rich {
                                            documents::doc_write_header();
                                        }
                                        input_handling::sync_cursor_to_editor(&editor_ch);
                                        let _ = sys::channel_signal(EDITOR_HANDLE);
                                        text_changed = true;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if redo_requested {
            if let Some(snap_id) = undo_state.redo() {
                // Restore the snapshot (same pattern as undo).
                let restore_payload = DocRestore {
                    snapshot_id: snap_id,
                };
                // SAFETY: DocRestore is repr(C) and fits in 60-byte payload.
                let restore_msg =
                    unsafe { ipc::Message::from_payload(MSG_DOC_RESTORE, &restore_payload) };
                fs_ch.send(&restore_msg);
                let _ = sys::channel_signal(FS_HANDLE);

                let mut reply = ipc::Message::new(0);
                if fs_ch.recv_blocking(FS_HANDLE.0, &mut reply)
                    && reply.msg_type == MSG_DOC_RESTORE_RESULT
                {
                    if let Some(protocol::document::Message::DocRestoreResult(result)) =
                        protocol::document::decode(reply.msg_type, &reply.payload)
                    {
                        if result.status == 0 {
                            let s = state();
                            let read_payload = DocRead {
                                file_id: s.doc_file_id,
                                target_va: 0,
                                capacity: s.doc_capacity as u32,
                                _pad: 0,
                            };
                            // SAFETY: DocRead is repr(C) and fits in 60-byte payload.
                            let read_msg =
                                unsafe { ipc::Message::from_payload(MSG_DOC_READ, &read_payload) };
                            fs_ch.send(&read_msg);
                            let _ = sys::channel_signal(FS_HANDLE);

                            if fs_ch.recv_blocking(FS_HANDLE.0, &mut reply)
                                && reply.msg_type == MSG_DOC_READ_DONE
                            {
                                if let Some(protocol::document::Message::DocReadDone(done)) =
                                    protocol::document::decode(reply.msg_type, &reply.payload)
                                {
                                    if done.status == 0 {
                                        let s = state();
                                        s.doc_len = done.len as usize;
                                        if s.doc_format == DocumentFormat::Rich {
                                            let text_len = documents::rich_text_len();
                                            s.cursor_pos = documents::rich_cursor_pos();
                                            if s.cursor_pos > text_len {
                                                s.cursor_pos = text_len;
                                            }
                                        } else {
                                            if s.cursor_pos > s.doc_len {
                                                s.cursor_pos = s.doc_len;
                                            }
                                        }
                                        input_handling::clear_selection();
                                        if s.doc_format != DocumentFormat::Rich {
                                            documents::doc_write_header();
                                        }
                                        input_handling::sync_cursor_to_editor(&editor_ch);
                                        let _ = sys::channel_signal(EDITOR_HANDLE);
                                        text_changed = true;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Update scroll offset for cursor/text changes.
        // Track whether the scroll position actually changed — the scene
        // dispatch needs to know so it does a full rebuild (visible lines
        // differ) instead of an incremental single-line update.
        let scroll_before = state().scroll_offset;
        if (changed || text_changed) && state().active_space == 0 {
            input_handling::update_scroll_offset(page_width, page_height, page_padding);
        }
        let scroll_after = state().scroll_offset;
        let scroll_diff = if scroll_before > scroll_after {
            scroll_before - scroll_after
        } else {
            scroll_after - scroll_before
        };
        let scroll_moved = scroll_diff > scene::MPT_PER_PT / 2;

        // ── Cursor blink ─────────────────────────────────────────────
        //
        // Reset blink to fully visible on any user input (keystroke or
        // click). Then advance the blink state machine — may produce a
        // scene update even when no events arrived (phase transition or
        // fade frame).
        let now_ms = {
            let s = state();
            let freq = s.counter_freq;
            if freq > 0 {
                sys::counter() * 1000 / freq
            } else {
                0
            }
        };
        // Actual frame delta for spring physics. Zero when multiple events
        // arrive in the same millisecond (spring tick is a no-op at dt=0).
        // Capped at 50ms to prevent spiral-of-death from long stalls.
        let frame_dt = {
            let elapsed = now_ms.saturating_sub(prev_ms);
            (elapsed.min(50) as f32) / 1000.0
        };
        prev_ms = now_ms;

        if had_user_input {
            blink::reset_blink(state(), now_ms);
        }
        state().timeline.tick(now_ms);
        let blink_changed = blink::advance_blink(state(), now_ms);
        if blink_changed {
            changed = true;
        }

        // ── Animation tick ───────────────────────────────────────────
        //
        // Advance the scroll spring toward its target. This must happen
        // after event processing (which may update the target) and before
        // scene dispatch (which reads scroll_offset).
        let mut scroll_changed = scroll_moved;
        let mut slide_changed = false;
        if scroll_moved && !text_changed {
            text_changed = true;
        }

        if state().scroll_animating {
            let old_scroll = state().scroll_offset;
            let s = state();
            s.scroll_spring.tick(frame_dt);
            s.scroll_offset = scene::f32_to_mpt(s.scroll_spring.value());

            if s.scroll_spring.settled() {
                // Snap to exact target (nearest whole-point-aligned Mpt)
                // to avoid persistent sub-pixel jitter.
                s.scroll_offset = scene::mpt_round_pt(s.scroll_target);
                s.scroll_animating = false;
            }

            let new_scroll = state().scroll_offset;
            let diff = if old_scroll > new_scroll {
                old_scroll - new_scroll
            } else {
                new_scroll - old_scroll
            };
            if diff > scene::MPT_PER_PT / 2 {
                scroll_changed = true;

                if !text_changed {
                    text_changed = true;
                }
            }
            changed = true; // trigger scene update
        }

        // ── Selection fade animation ────────────────────────────────
        //
        // When the selection changes, start a fade-in animation from
        // opacity 0→255 over 100ms. The animation value is applied to
        // selection nodes after each scene build.
        if selection_changed {
            let s = state();
            // Cancel any previous selection fade in progress.
            if let Some(old_id) = s.selection_fade_id {
                s.timeline.cancel(old_id);
            }
            s.selection_fade_id = s
                .timeline
                .start(0.0, 255.0, 100, animation::Easing::EaseOut, now_ms)
                .ok();
            s.selection_opacity = 0;
        }
        // Tick the selection fade (if active).
        {
            let s = state();
            if let Some(id) = s.selection_fade_id {
                if s.timeline.is_active(id) {
                    let new_val = s.timeline.value(id) as u8;
                    if new_val != s.selection_opacity {
                        s.selection_opacity = new_val;
                        changed = true;
                    }
                } else {
                    s.selection_opacity = 255;
                    s.selection_fade_id = None;
                }
            }
        }

        // ── Document switch slide animation ─────────────────────────
        //
        // Ctrl+Tab sets slide_target to the next space. The spring
        // animates slide_offset toward the target. We update N_STRIP's
        // content_transform each frame via apply_slide.
        //
        // The slide does NOT set `changed` — it uses its own publish
        // path (apply_slide) and compositor signal. Setting `changed`
        // would trigger an unnecessary update_cursor dispatch.
        if state().slide_animating {
            let s = state();
            // On the first frame, frame_dt includes idle sleep time
            // (up to 50ms) — advancing the spring by that much causes
            // a visible first-frame jump. Clamp to one frame interval
            // for smooth onset; subsequent frames use wall-clock dt.
            let dt = if s.slide_first_frame {
                s.slide_first_frame = false;
                (frame_interval_ns as f32) / 1_000_000_000.0
            } else {
                frame_dt
            };
            s.slide_spring.tick(dt);
            let new_offset = scene::f32_to_mpt(s.slide_spring.value());
            if new_offset != s.slide_offset {
                s.slide_offset = new_offset;
                slide_changed = true;
            }
            if s.slide_spring.settled() {
                s.slide_offset = s.slide_target; // both Mpt, exact match
                s.slide_animating = false;
                slide_changed = true;
            }
        }

        // ── Pointer auto-hide ─────────────────────────────────────
        //
        // After 3 s of inactivity, start a 300 ms EaseOut fade-out.
        // When the fade completes, mark the pointer hidden (opacity 0).
        // On any pointer move, the handler above cancels the fade and
        // restores full opacity immediately.
        {
            const POINTER_HIDE_MS: u64 = 3000;
            const POINTER_FADE_MS: u32 = 300;

            let s = state();

            // Start fade-out after 3 s of inactivity.
            if s.pointer_visible && s.pointer_fade_id.is_none() && s.pointer_opacity == 255 {
                let idle_ms = now_ms.saturating_sub(s.pointer_last_event_ms);
                if idle_ms >= POINTER_HIDE_MS {
                    s.pointer_fade_id = s
                        .timeline
                        .start(
                            255.0,
                            0.0,
                            POINTER_FADE_MS,
                            animation::Easing::EaseOut,
                            now_ms,
                        )
                        .ok();
                }
            }

            // Tick pointer fade animation.
            if let Some(id) = s.pointer_fade_id {
                if s.timeline.is_active(id) {
                    let new_opacity = s.timeline.value(id) as u8;
                    if new_opacity != s.pointer_opacity {
                        s.pointer_opacity = new_opacity;
                        changed = true;
                    }
                } else {
                    // Fade complete — pointer is now hidden.
                    s.pointer_opacity = 0;
                    s.pointer_visible = false;
                    s.pointer_fade_id = None;
                    changed = true;
                }
            }
        }

        // ── Scene update dispatch ──────────────────────────────────
        //
        // Use targeted updates for incremental changes instead of
        // rebuilding the entire scene graph every frame.
        //
        // Priority order (most-specific first):
        // 1. context_switched → full rebuild
        // 2. text_changed     → update_document_content (+ clock if timer)
        // 3. selection_changed → update_selection (+ clock if timer)
        // 4. changed (cursor/pointer only) → update_cursor (+ clock if changed)
        // 5. clock_changed only → update_clock
        //
        // Clock text is formatted at the top of every scene update so the
        // displayed time is always current. clock_changed is true when the
        // timer fired OR when the formatted second differs from the previous
        // frame (catches blink wakeups that land right after a second boundary).

        let needs_scene_update =
            changed || text_changed || selection_changed || timer_fired || slide_changed;

        if needs_scene_update {
            // Always format the current time so every scene rebuild shows the
            // correct clock — not just when the timer fires. Detect whether the
            // displayed second actually changed so we mark the clock node dirty.
            let prev_time = time_buf;
            documents::format_time_hms(clock_seconds(), &mut time_buf);
            let clock_changed = timer_fired || time_buf != prev_time;

            // Only context_switched requires a full rebuild. Timer+input
            // coincidence is handled incrementally by each targeted method.
            let is_rich_doc = state().doc_format == DocumentFormat::Rich;

            if context_switched {

                let s = state();
                let title: &[u8] = if s.active_space != 0 {
                    b"Image"
                } else if is_rich_doc {
                    b"Rich Text"
                } else {
                    b"Text"
                };
                // Full scene build always uses flat text for structure.
                // For rich text, we follow with a rich content update.
                scene.build_editor_scene(
                    &scene_cfg,
                    if is_rich_doc {
                        &[]
                    } else {
                        documents::doc_content()
                    },
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    title,
                    &time_buf,
                    s.scroll_offset,
                    s.cursor_opacity,
                    s.mouse_x,
                    s.mouse_y,
                    s.pointer_opacity,
                    s.slide_offset,
                    s.active_space,
                );
                // Immediately rebuild document content with rich text layout.
                if is_rich_doc {
                    let rich_fonts = scene_state::RichFonts {
                        mono_data: font_data(),
                        mono_upem: s.font_upem,
                        mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
                        mono_ascender: s.font_ascender,
                        mono_descender: s.font_descender,
                        mono_line_gap: s.font_line_gap,
                        mono_cap_height: s.font_cap_height,
                        sans_data: sans_font_data(),
                        sans_upem: s.sans_font_upem,
                        sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
                        sans_ascender: s.sans_font_ascender,
                        sans_descender: s.sans_font_descender,
                        sans_line_gap: s.sans_font_line_gap,
                        sans_cap_height: s.sans_font_cap_height,
                        serif_data: serif_font_data(),
                        serif_upem: s.serif_font_upem,
                        serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                        serif_ascender: s.serif_font_ascender,
                        serif_descender: s.serif_font_descender,
                        serif_line_gap: s.serif_font_line_gap,
                        serif_cap_height: s.serif_font_cap_height,
                        mono_italic_data: mono_italic_font_data(),
                        mono_italic_upem: s.mono_italic_font_upem,
                        mono_italic_content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
                        mono_italic_ascender: s.mono_italic_font_ascender,
                        mono_italic_descender: s.mono_italic_font_descender,
                        mono_italic_line_gap: s.mono_italic_font_line_gap,
                        mono_italic_cap_height: s.mono_italic_font_cap_height,
                        sans_italic_data: sans_italic_font_data(),
                        sans_italic_upem: s.sans_italic_font_upem,
                        sans_italic_content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
                        sans_italic_ascender: s.sans_italic_font_ascender,
                        sans_italic_descender: s.sans_italic_font_descender,
                        sans_italic_line_gap: s.sans_italic_font_line_gap,
                        sans_italic_cap_height: s.sans_italic_font_cap_height,
                        serif_italic_data: serif_italic_font_data(),
                        serif_italic_upem: s.serif_italic_font_upem,
                        serif_italic_content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
                        serif_italic_ascender: s.serif_italic_font_ascender,
                        serif_italic_descender: s.serif_italic_font_descender,
                        serif_italic_line_gap: s.serif_italic_font_line_gap,
                        serif_italic_cap_height: s.serif_italic_font_cap_height,
                    };
                    let lines = scene.update_rich_document_content(
                        &scene_cfg,
                        documents::rich_buf_ref(),
                        &rich_fonts,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        title,
                        &time_buf,
                        s.scroll_offset,
                        true,
                        s.cursor_opacity,
                    );
                    state().rich_lines = lines;
                }
            } else if text_changed && is_rich_doc {
                // Rich text content changed — always full rebuild.
                let s = state();
                let rich_fonts = scene_state::RichFonts {
                    mono_data: font_data(),
                    mono_upem: s.font_upem,
                    mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
                    mono_ascender: s.font_ascender,
                    mono_descender: s.font_descender,
                    mono_line_gap: s.font_line_gap,
                    mono_cap_height: s.font_cap_height,
                    sans_data: sans_font_data(),
                    sans_upem: s.sans_font_upem,
                    sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
                    sans_ascender: s.sans_font_ascender,
                    sans_descender: s.sans_font_descender,
                    sans_line_gap: s.sans_font_line_gap,
                    sans_cap_height: s.sans_font_cap_height,
                    serif_data: serif_font_data(),
                    serif_upem: s.serif_font_upem,
                    serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                    serif_ascender: s.serif_font_ascender,
                    serif_descender: s.serif_font_descender,
                    serif_line_gap: s.serif_font_line_gap,
                    serif_cap_height: s.serif_font_cap_height,
                    mono_italic_data: mono_italic_font_data(),
                    mono_italic_upem: s.mono_italic_font_upem,
                    mono_italic_content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
                    mono_italic_ascender: s.mono_italic_font_ascender,
                    mono_italic_descender: s.mono_italic_font_descender,
                    mono_italic_line_gap: s.mono_italic_font_line_gap,
                    mono_italic_cap_height: s.mono_italic_font_cap_height,
                    sans_italic_data: sans_italic_font_data(),
                    sans_italic_upem: s.sans_italic_font_upem,
                    sans_italic_content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
                    sans_italic_ascender: s.sans_italic_font_ascender,
                    sans_italic_descender: s.sans_italic_font_descender,
                    sans_italic_line_gap: s.sans_italic_font_line_gap,
                    sans_italic_cap_height: s.sans_italic_font_cap_height,
                    serif_italic_data: serif_italic_font_data(),
                    serif_italic_upem: s.serif_italic_font_upem,
                    serif_italic_content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
                    serif_italic_ascender: s.serif_italic_font_ascender,
                    serif_italic_descender: s.serif_italic_font_descender,
                    serif_italic_line_gap: s.serif_italic_font_line_gap,
                    serif_italic_cap_height: s.serif_italic_font_cap_height,
                };
                let lines = scene.update_rich_document_content(
                    &scene_cfg,
                    documents::rich_buf_ref(),
                    &rich_fonts,
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    b"Rich Text",
                    &time_buf,
                    s.scroll_offset,
                    clock_changed,
                    s.cursor_opacity,
                );
                state().rich_lines = lines;
            } else if text_changed {
                // Plain text content changed (insert/delete/scroll).
                let doc = documents::doc_content();
                let new_line_count = scene_state::count_lines(doc);

                if scroll_changed {
                    let s = state();
                    scene.update_document_content(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        clock_changed,
                        s.cursor_opacity,
                    );
                } else if new_line_count == prev_line_count {
                    let s = state();
                    let cpl = if s.char_w_fx > 0 {
                        ((scene_cfg
                            .page_width
                            .saturating_sub(2 * scene_cfg.text_inset_x)
                            as i64
                            * 65536)
                            / s.char_w_fx as i64)
                            .max(1) as usize
                    } else {
                        80
                    };
                    let changed_line = scene_state::byte_to_line_col(doc, s.cursor_pos, cpl).0;
                    scene.update_document_incremental(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        changed_line,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        clock_changed,
                        s.cursor_opacity,
                    );
                } else if new_line_count == prev_line_count + 1 {
                    let s = state();
                    scene.update_document_insert_line(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        clock_changed,
                        s.cursor_opacity,
                    );
                } else if new_line_count + 1 == prev_line_count {
                    let s = state();
                    scene.update_document_delete_line(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        clock_changed,
                        s.cursor_opacity,
                    );
                } else {
                    let s = state();
                    scene.update_document_content(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        clock_changed,
                        s.cursor_opacity,
                    );
                }

                prev_line_count = new_line_count;
            } else if selection_changed && is_rich_doc {
                // Rich text selection — full rebuild (proportional positioning).
                let s = state();
                let rich_fonts = scene_state::RichFonts {
                    mono_data: font_data(),
                    mono_upem: s.font_upem,
                    mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
                    mono_ascender: s.font_ascender,
                    mono_descender: s.font_descender,
                    mono_line_gap: s.font_line_gap,
                    mono_cap_height: s.font_cap_height,
                    sans_data: sans_font_data(),
                    sans_upem: s.sans_font_upem,
                    sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
                    sans_ascender: s.sans_font_ascender,
                    sans_descender: s.sans_font_descender,
                    sans_line_gap: s.sans_font_line_gap,
                    sans_cap_height: s.sans_font_cap_height,
                    serif_data: serif_font_data(),
                    serif_upem: s.serif_font_upem,
                    serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                    serif_ascender: s.serif_font_ascender,
                    serif_descender: s.serif_font_descender,
                    serif_line_gap: s.serif_font_line_gap,
                    serif_cap_height: s.serif_font_cap_height,
                    mono_italic_data: mono_italic_font_data(),
                    mono_italic_upem: s.mono_italic_font_upem,
                    mono_italic_content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
                    mono_italic_ascender: s.mono_italic_font_ascender,
                    mono_italic_descender: s.mono_italic_font_descender,
                    mono_italic_line_gap: s.mono_italic_font_line_gap,
                    mono_italic_cap_height: s.mono_italic_font_cap_height,
                    sans_italic_data: sans_italic_font_data(),
                    sans_italic_upem: s.sans_italic_font_upem,
                    sans_italic_content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
                    sans_italic_ascender: s.sans_italic_font_ascender,
                    sans_italic_descender: s.sans_italic_font_descender,
                    sans_italic_line_gap: s.sans_italic_font_line_gap,
                    sans_italic_cap_height: s.sans_italic_font_cap_height,
                    serif_italic_data: serif_italic_font_data(),
                    serif_italic_upem: s.serif_italic_font_upem,
                    serif_italic_content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
                    serif_italic_ascender: s.serif_italic_font_ascender,
                    serif_italic_descender: s.serif_italic_font_descender,
                    serif_italic_line_gap: s.serif_italic_font_line_gap,
                    serif_italic_cap_height: s.serif_italic_font_cap_height,
                };
                let lines = scene.update_rich_document_content(
                    &scene_cfg,
                    documents::rich_buf_ref(),
                    &rich_fonts,
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    b"Rich Text",
                    &time_buf,
                    s.scroll_offset,
                    clock_changed,
                    s.cursor_opacity,
                );
                state().rich_lines = lines;
            } else if selection_changed {
                // Mono text selection changed.
                let s = state();
                let sel_text_h = scene_cfg
                    .page_height
                    .saturating_sub(2 * scene_cfg.text_inset_x);
                let scroll_pt = s.scroll_offset / scene::MPT_PER_PT;

                scene.update_selection(
                    &scene_cfg,
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    documents::doc_content(),
                    sel_text_h,
                    scroll_pt,
                    s.cursor_opacity,
                );
            } else if changed && is_rich_doc {
                // Rich text cursor-only update — full rebuild needed because
                // proportional cursor positioning requires the styled layout.
                let s = state();
                let rich_fonts = scene_state::RichFonts {
                    mono_data: font_data(),
                    mono_upem: s.font_upem,
                    mono_content_id: protocol::content::CONTENT_ID_FONT_MONO,
                    mono_ascender: s.font_ascender,
                    mono_descender: s.font_descender,
                    mono_line_gap: s.font_line_gap,
                    mono_cap_height: s.font_cap_height,
                    sans_data: sans_font_data(),
                    sans_upem: s.sans_font_upem,
                    sans_content_id: protocol::content::CONTENT_ID_FONT_SANS,
                    sans_ascender: s.sans_font_ascender,
                    sans_descender: s.sans_font_descender,
                    sans_line_gap: s.sans_font_line_gap,
                    sans_cap_height: s.sans_font_cap_height,
                    serif_data: serif_font_data(),
                    serif_upem: s.serif_font_upem,
                    serif_content_id: protocol::content::CONTENT_ID_FONT_SERIF,
                    serif_ascender: s.serif_font_ascender,
                    serif_descender: s.serif_font_descender,
                    serif_line_gap: s.serif_font_line_gap,
                    serif_cap_height: s.serif_font_cap_height,
                    mono_italic_data: mono_italic_font_data(),
                    mono_italic_upem: s.mono_italic_font_upem,
                    mono_italic_content_id: protocol::content::CONTENT_ID_FONT_MONO_ITALIC,
                    mono_italic_ascender: s.mono_italic_font_ascender,
                    mono_italic_descender: s.mono_italic_font_descender,
                    mono_italic_line_gap: s.mono_italic_font_line_gap,
                    mono_italic_cap_height: s.mono_italic_font_cap_height,
                    sans_italic_data: sans_italic_font_data(),
                    sans_italic_upem: s.sans_italic_font_upem,
                    sans_italic_content_id: protocol::content::CONTENT_ID_FONT_SANS_ITALIC,
                    sans_italic_ascender: s.sans_italic_font_ascender,
                    sans_italic_descender: s.sans_italic_font_descender,
                    sans_italic_line_gap: s.sans_italic_font_line_gap,
                    sans_italic_cap_height: s.sans_italic_font_cap_height,
                    serif_italic_data: serif_italic_font_data(),
                    serif_italic_upem: s.serif_italic_font_upem,
                    serif_italic_content_id: protocol::content::CONTENT_ID_FONT_SERIF_ITALIC,
                    serif_italic_ascender: s.serif_italic_font_ascender,
                    serif_italic_descender: s.serif_italic_font_descender,
                    serif_italic_line_gap: s.serif_italic_font_line_gap,
                    serif_italic_cap_height: s.serif_italic_font_cap_height,
                };
                let lines = scene.update_rich_document_content(
                    &scene_cfg,
                    documents::rich_buf_ref(),
                    &rich_fonts,
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    b"Rich Text",
                    &time_buf,
                    s.scroll_offset,
                    clock_changed,
                    s.cursor_opacity,
                );
                state().rich_lines = lines;
            } else if changed {
                // Mono text cursor-only update.
                let s = state();
                let dw = scene_cfg
                    .page_width
                    .saturating_sub(2 * scene_cfg.text_inset_x);
                let chars_per_line = if s.char_w_fx > 0 {
                    ((dw as i64 * 65536) / s.char_w_fx as i64).max(1) as u32
                } else {
                    80
                };

                scene.update_cursor(
                    &scene_cfg,
                    s.cursor_pos as u32,
                    documents::doc_content(),
                    chars_per_line,
                    if clock_changed { Some(&time_buf) } else { None },
                    s.cursor_opacity,
                );
            } else if clock_changed {
                // Clock changed without any other scene change — just update the clock text.
                scene.update_clock(&scene_cfg, &time_buf);
            }

            // Apply post-build opacity (selection fade-in).
            {
                let s = state();
                scene.apply_opacity(255, s.selection_opacity);
            }

            // Apply slide offset if it changed this frame.
            if slide_changed {
                scene.apply_slide(state().slide_offset);
            }

            // Apply pointer cursor opacity to the scene graph. Position
            // is read directly from the shared register by the render
            // service, so we only need to publish when something else in
            // the scene already changed (opacity, visibility, image).
            if needs_scene_update {
                let s = state();
                scene.apply_pointer(s.mouse_x, s.mouse_y, s.pointer_opacity);
            }
        }

        // Signal compositor for scene changes AND pointer-only moves.
        // For pointer-only moves (no scene publish), the render service
        // wakes up, sees no generation change, reads the pointer state
        // register, and sends a cursor-only frame (no full scene walk).
        if needs_scene_update || pointer_position_changed {
            compositor_ch.send(&scene_msg);
            let _ = sys::channel_signal(COMPOSITOR_HANDLE);
        }

        // Update cached scene generation and sweep deferred frees.
        // Only runs when entries are pending reclamation (common case: 0).
        if needs_scene_update {
            let s = state();
            s.scene_generation = scene.generation();
            if s.content_alloc.pending_count() > 0 && s.content_va != 0 {
                let reader_gen = scene.reader_done_gen();
                // SAFETY: content_va is mapped read-write; header is repr(C).
                let header =
                    unsafe { &mut *(s.content_va as *mut protocol::content::ContentRegionHeader) };
                s.content_alloc.sweep(reader_gen, header);
            }
        }
    }
}

/// Scale an absolute pointer coordinate from the [0, 32767] range to
/// [0, max_pixels). Uses integer math: `coord * max_pixels / 32768`.
/// The divisor is 32768 (not 32767) to ensure the result never equals
/// max_pixels (stays in [0, max_pixels-1]).
fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;

    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}
