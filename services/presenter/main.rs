//! Presenter service — event loop, input routing, and scene graph building.
//!
//! Owns all view state (cursor, selection, scroll, focus, animation).
//! Reads document buffer (RO) from A and layout results (RO) from B.
//! Sole writer to the scene graph. Routes input to editors.
//!
//! # Responsibilities
//!
//! - Scene graph building (writes to shared memory)
//! - Input routing (keyboard → editor, pointer → hit testing)
//! - View state (cursor position, selection, scroll, blink, animation)
//! - Editor communication (receives cursor/selection updates, sends input events)
//! - Clock / RTC
//! - Scroll management
//! - Cursor shape resolution (hit testing)
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: input driver → presenter (keyboard events)
//! Handle 2: presenter → compositor (scene update signal)
//! Handle 3: presenter ↔ editor (input events out, cursor/selection in)
//! Handle 4: A → C (doc-changed notifications, undo/redo requests)
//! Handle 5: layout ↔ presenter (layout recompute/ready signals)
//! Handle 6: second input device (tablet) → C (optional)

#![no_std]
#![no_main]

extern crate alloc;
extern crate animation;
extern crate drawing;
extern crate fonts;
extern crate icons as icon_lib;
extern crate layout as layout_lib;
extern crate piecetable;
extern crate render;
extern crate scene;

#[path = "blink.rs"]
mod blink;
#[path = "documents.rs"]
mod documents;
#[path = "input.rs"]
mod input_handling;
#[path = "scene/mod.rs"]
mod layout;
#[path = "scene_state.rs"]
mod scene_state;

use protocol::{
    edit::{
        self, CursorMove, SelectionUpdate, MSG_CURSOR_MOVE, MSG_SELECTION_UPDATE, MSG_SET_CURSOR,
    },
    init::{
        self as init_proto, CoreConfig, FrameRateMsg, RtcConfig, MSG_CORE_CONFIG, MSG_FRAME_RATE,
        MSG_RTC_CONFIG, MSG_SCENE_UPDATED,
    },
    input::{self, KeyEvent, PointerButton, MSG_KEY_EVENT, MSG_POINTER_BUTTON},
    layout::{
        self as layout_proto, CoreLayoutConfig, LayoutResultsHeader, LineInfo, ViewportState,
        VisibleRun, MSG_CORE_LAYOUT_CONFIG, MSG_LAYOUT_READY, MSG_LAYOUT_RECOMPUTE,
    },
    view::{
        self as view_proto, DocChanged, DocLoaded, ImageDecoded, DOC_CHANGED_CLEAR_SELECTION,
        MSG_DOC_CHANGED, MSG_DOC_LOADED, MSG_IMAGE_DECODED, MSG_REDO_REQUEST, MSG_UNDO_REQUEST,
    },
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

pub(crate) use protocol::edit::DocumentFormat;

/// Boot timeout: force-transition after 5 seconds regardless of pending replies.
const BOOT_TIMEOUT_NS: u64 = 5_000_000_000;

/// Spinner rotation increment per frame (~5° per tick at 60fps → ~1.2 sec/rev).
const SPINNER_ANGLE_DELTA: f32 = 0.0873;

pub(crate) const DOC_HEADER_SIZE: usize = 64;
const FONT_SIZE: u32 = 18;
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
// TEXT_INSET_X removed: page_padding (24) is the SSOT for text inset,
// passed through to write_viewport_state and scene config.
const TITLE_BAR_H: u32 = 36;

// ── View state sub-structs ─────────────────────────────────────────

/// Cursor state: text cursor position, blink cycle, goal column.
pub(crate) struct CursorState {
    pub(crate) pos: usize,
    pub(crate) opacity: u8,
    pub(crate) blink_phase: blink::BlinkPhase,
    pub(crate) blink_phase_start_ms: u64,
    pub(crate) blink_id: Option<animation::AnimationId>,
    /// Sticky goal column for Up/Down navigation (plain text).
    pub(crate) goal_column: Option<usize>,
    /// Sticky goal x-position (millipoints) for Up/Down navigation (rich text).
    pub(crate) goal_x: Option<i32>,
}

/// Selection state: anchor, range, fade, click detection, drag tracking.
pub(crate) struct SelectionState {
    /// Selection anchor: the fixed end of a selection range.
    pub(crate) anchor: usize,
    /// True when a selection is active.
    pub(crate) active: bool,
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// Animation ID for the selection highlight fade-in (0→255).
    pub(crate) fade_id: Option<animation::AnimationId>,
    /// Current selection highlight opacity (animated on selection change).
    pub(crate) opacity: u8,
    /// Click state for double/triple-click detection.
    pub(crate) last_click_ms: u64,
    pub(crate) last_click_x: u32,
    pub(crate) last_click_y: u32,
    pub(crate) click_count: u8,
    /// True while the left mouse button is held after a mouse-down on text.
    /// While dragging, pointer movement extends the selection from the anchor.
    pub(crate) dragging: bool,
    /// For double/triple-click-drag: the original anchor word/line boundaries.
    /// Selection snaps to word (click_count=2) or line (click_count=3) increments.
    pub(crate) drag_origin_start: usize,
    pub(crate) drag_origin_end: usize,
}

/// Scroll state: spring-based smooth scrolling.
pub(crate) struct ScrollState {
    pub(crate) animating: bool,
    pub(crate) offset: scene::Mpt,
    pub(crate) spring: animation::Spring,
    pub(crate) target: scene::Mpt,
}

/// Animation state: timeline, timer, space-switching slide animation.
pub(crate) struct AnimationState {
    pub(crate) timeline: animation::Timeline,
    pub(crate) timer_active: bool,
    pub(crate) timer_handle: sys::TimerHandle,
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
}

/// Pointer state: mouse position, visibility, cursor shape.
pub(crate) struct PointerState {
    pub(crate) x: u32,
    pub(crate) y: u32,
    /// Animation ID for the pointer fade-out (255→0, 300ms EaseOut).
    pub(crate) fade_id: Option<animation::AnimationId>,
    /// Timestamp (ms) of the last pointer movement event.
    pub(crate) last_event_ms: u64,
    /// Current pointer cursor opacity (0 = hidden, 255 = fully visible).
    pub(crate) opacity: u8,
    /// True when the pointer cursor is currently shown (recently moved).
    pub(crate) visible: bool,
    /// VA of the shared PointerState register (input driver writes, C reads).
    pub(crate) input_state_va: usize,
    /// VA of the shared CursorState page (C writes, compositor reads).
    pub(crate) cursor_state_va: usize,
    /// Last-seen packed pointer_xy value (for change detection).
    pub(crate) last_xy: u64,
    /// Current shape_generation written to the cursor state page.
    pub(crate) shape_generation: u32,
    /// Current cursor shape name (static literal for pointer identity comparison).
    pub(crate) shape_name: &'static str,
}

// ── Top-level view state ───────────────────────────────────────────

pub(crate) struct ViewState {
    pub(crate) cursor: CursorState,
    pub(crate) selection: SelectionState,
    pub(crate) scroll: ScrollState,
    pub(crate) animation: AnimationState,
    pub(crate) pointer: PointerState,
    pub(crate) boot_counter: u64,
    /// Character advance in 16.16 fixed-point points.
    pub(crate) char_w_fx: i32,
    pub(crate) counter_freq: u64,
    pub(crate) doc_buf: *mut u8,
    pub(crate) doc_capacity: usize,
    /// Generation counter for the doc buffer header (offset 16).
    pub(crate) doc_generation: u32,
    /// FileId of the active text document in the store.
    pub(crate) doc_file_id: u64,
    /// Document format (Plain for text/plain, Rich for text/rich).
    pub(crate) doc_format: DocumentFormat,
    pub(crate) doc_len: usize,
    pub(crate) font_data_ptr: *const u8,
    pub(crate) font_data_len: usize,
    pub(crate) font_upem: u16,
    pub(crate) font_ascender: i16,
    pub(crate) font_descender: i16,
    pub(crate) font_line_gap: i16,
    pub(crate) font_cap_height: i16,
    pub(crate) sans_font_data_ptr: *const u8,
    pub(crate) sans_font_data_len: usize,
    pub(crate) sans_font_upem: u16,
    pub(crate) sans_font_ascender: i16,
    pub(crate) sans_font_descender: i16,
    pub(crate) sans_font_line_gap: i16,
    pub(crate) sans_font_cap_height: i16,
    pub(crate) serif_font_data_ptr: *const u8,
    pub(crate) serif_font_data_len: usize,
    pub(crate) serif_font_upem: u16,
    pub(crate) serif_font_ascender: i16,
    pub(crate) serif_font_descender: i16,
    pub(crate) serif_font_line_gap: i16,
    pub(crate) serif_font_cap_height: i16,
    pub(crate) mono_italic_font_data_ptr: *const u8,
    pub(crate) mono_italic_font_data_len: usize,
    pub(crate) mono_italic_font_upem: u16,
    pub(crate) mono_italic_font_ascender: i16,
    pub(crate) mono_italic_font_descender: i16,
    pub(crate) mono_italic_font_line_gap: i16,
    pub(crate) mono_italic_font_cap_height: i16,
    pub(crate) sans_italic_font_data_ptr: *const u8,
    pub(crate) sans_italic_font_data_len: usize,
    pub(crate) sans_italic_font_upem: u16,
    pub(crate) sans_italic_font_ascender: i16,
    pub(crate) sans_italic_font_descender: i16,
    pub(crate) sans_italic_font_line_gap: i16,
    pub(crate) sans_italic_font_cap_height: i16,
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
    pub(crate) line_h: u32,
    /// VA of the layout results shared memory (read-only, B writes).
    pub(crate) layout_results_va: usize,
    /// Layout results region capacity in bytes.
    pub(crate) layout_results_capacity: usize,
    /// VA of the viewport state register (read-write, C writes, B reads).
    pub(crate) viewport_state_va: usize,
    /// Last layout generation read from B.
    pub(crate) layout_generation: u32,
    pub(crate) rtc_mmio_va: usize,
    /// PL031 epoch captured once at boot — never re-read.
    pub(crate) rtc_epoch_at_boot: u64,
    /// CNTVCT value at the moment PL031 was read (for elapsed computation).
    pub(crate) boot_counter_at_rtc_read: u64,
    /// Cached line info from B's layout results (for navigation/hit-testing).
    pub(crate) cached_lines: alloc::vec::Vec<protocol::layout::LineInfo>,
    // ── Channel handles (populated from CoreLayoutConfig at boot) ────
    pub(crate) input_handle: sys::ChannelHandle,
    pub(crate) compositor_handle: sys::ChannelHandle,
    pub(crate) editor_handle: sys::ChannelHandle,
    pub(crate) docmodel_handle: sys::ChannelHandle,
    pub(crate) layout_handle: sys::ChannelHandle,
    pub(crate) input2_handle: sys::ChannelHandle,
}

impl ViewState {
    const fn new() -> Self {
        Self {
            cursor: CursorState {
                pos: 0,
                opacity: 255,
                blink_phase: blink::BlinkPhase::VisibleHold,
                blink_phase_start_ms: 0,
                blink_id: None,
                goal_column: None,
                goal_x: None,
            },
            selection: SelectionState {
                anchor: 0,
                active: false,
                start: 0,
                end: 0,
                fade_id: None,
                opacity: 255,
                last_click_ms: 0,
                last_click_x: 0,
                last_click_y: 0,
                click_count: 0,
                dragging: false,
                drag_origin_start: 0,
                drag_origin_end: 0,
            },
            scroll: ScrollState {
                animating: false,
                offset: 0,
                spring: animation::Spring::snappy(0.0),
                target: 0,
            },
            animation: AnimationState {
                timeline: animation::Timeline::new(),
                timer_active: false,
                timer_handle: sys::TimerHandle(0),
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
            },
            pointer: PointerState {
                x: 0,
                y: 0,
                fade_id: None,
                last_event_ms: 0,
                opacity: 0,
                visible: false,
                input_state_va: 0,
                cursor_state_va: 0,
                last_xy: 0,
                shape_generation: 0,
                shape_name: "pointer",
            },
            boot_counter: 0,
            char_w_fx: 8 * 65536,
            counter_freq: 0,
            doc_buf: core::ptr::null_mut(),
            doc_capacity: 0,
            doc_generation: 0,
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
            line_h: 20,
            layout_results_va: 0,
            layout_results_capacity: 0,
            viewport_state_va: 0,
            layout_generation: 0,
            rtc_mmio_va: 0,
            rtc_epoch_at_boot: 0,
            boot_counter_at_rtc_read: 0,
            cached_lines: alloc::vec::Vec::new(),
            input_handle: sys::ChannelHandle(u16::MAX),
            compositor_handle: sys::ChannelHandle(u16::MAX),
            editor_handle: sys::ChannelHandle(u16::MAX),
            docmodel_handle: sys::ChannelHandle(u16::MAX),
            layout_handle: sys::ChannelHandle(u16::MAX),
            input2_handle: sys::ChannelHandle(u16::MAX),
        }
    }
}

struct SyncState(core::cell::UnsafeCell<ViewState>);
// SAFETY: Single-threaded userspace process.
unsafe impl Sync for SyncState {}
static STATE: SyncState = SyncState(core::cell::UnsafeCell::new(ViewState::new()));

pub(crate) fn state() -> &'static mut ViewState {
    // SAFETY: Single-threaded userspace process. No concurrent access.
    unsafe { &mut *STATE.0.get() }
}

/// Resolve cursor shape by hit-testing the scene graph.
///
/// Walks the scene tree depth-first (pre-order) to find the topmost
/// visible node containing the pointer. Then walks up through ancestors
/// to find the first `cursor_shape` declaration (inheritance). Returns
/// the cursor icon name.
///
/// Coordinates: mouse position in pixels, scene graph in millipoints.
/// Handles child_offset (scroll/slide) by inverting the offset when
/// descending into children.
fn resolve_cursor_shape(
    nodes: &[scene::Node],
    data_buf: &[u8],
    mouse_x: u32,
    mouse_y: u32,
) -> &'static str {
    if nodes.is_empty() {
        return "pointer";
    }

    // Convert pixel coordinates to millipoints.
    let test_x = (mouse_x as i64) * (scene::MPT_PER_PT as i64);
    let test_y = (mouse_y as i64) * (scene::MPT_PER_PT as i64);

    // Parent map for inheritance walk. Built during traversal.
    let mut parent = [scene::NULL; 64];

    // Topmost hit node (last in rendering order that contains the point).
    let mut hit: scene::NodeId = scene::NULL;

    // Iterative depth-first walk with coordinate tracking.
    // Stack entries: (node_id, origin_x_mpt, origin_y_mpt).
    // origin is the accumulated position from all ancestors, in millipoints.
    let mut stack: [(scene::NodeId, i64, i64); 48] = [(scene::NULL, 0, 0); 48];
    let mut sp: usize = 0;

    // Seed with root node at the origin.
    if nodes[0].flags.contains(scene::NodeFlags::VISIBLE) {
        stack[0] = (0, 0, 0);
        sp = 1;
    }

    while sp > 0 {
        sp -= 1;
        let (id, ox, oy) = stack[sp];

        let node = &nodes[id as usize];

        // Absolute position of this node's top-left corner.
        let abs_x = ox + node.x as i64;
        let abs_y = oy + node.y as i64;

        // Bounding box test.
        let inside = test_x >= abs_x
            && test_x < abs_x + node.width as i64
            && test_y >= abs_y
            && test_y < abs_y + node.height as i64;

        if inside {
            // Fine phase: for Path nodes, test whether the point is actually
            // inside the path geometry (winding number), not just the bounding
            // box. This enables precise hit-testing for circles, icons, and
            // arbitrary shapes.
            if let scene::Content::Path { contours, .. } = node.content {
                if contours.length > 0 {
                    let start = contours.offset as usize;
                    let end = start + contours.length as usize;
                    if end <= data_buf.len() {
                        // Convert test point to node-local coordinates (points, f32).
                        let local_x = (test_x - abs_x) as f32 / scene::MPT_PER_PT as f32;
                        let local_y = (test_y - abs_y) as f32 / scene::MPT_PER_PT as f32;
                        let w = scene::path_winding_number(&data_buf[start..end], local_x, local_y);
                        if w != 0 {
                            hit = id;
                        }
                        // If winding == 0, point is outside the path — skip this
                        // node (don't update hit). The bounding box matched but
                        // the actual geometry didn't.
                    } else {
                        hit = id;
                    }
                } else {
                    hit = id;
                }
            } else {
                hit = id;
            }
        }

        // Skip clipped-out children: if this node clips children and the
        // point is outside, no child can be hit.
        if node.clips_children() && !inside {
            continue;
        }

        // Children's origin: node's absolute position + inverse child_offset.
        // child_offset shifts children's coordinate space (scroll/slide).
        // To test a point against children, apply the inverse.
        let child_ox = abs_x - (node.child_offset_x * scene::MPT_PER_PT as f32) as i64;
        let child_oy = abs_y - (node.child_offset_y * scene::MPT_PER_PT as f32) as i64;

        // Collect children (forward-linked: first_child → next_sibling → ...).
        let mut children: [scene::NodeId; 16] = [scene::NULL; 16];
        let mut nc: usize = 0;
        let mut c = node.first_child;
        while c != scene::NULL && (c as usize) < nodes.len() && nc < 16 {
            children[nc] = c;
            parent[c as usize & 63] = id;
            nc += 1;
            c = nodes[c as usize].next_sibling;
        }

        // Push in reverse order: first child on top → processed first.
        // Later siblings processed later → their hits override (topmost wins).
        for i in (0..nc).rev() {
            let cid = children[i];
            if nodes[cid as usize]
                .flags
                .contains(scene::NodeFlags::VISIBLE)
                && sp < stack.len()
            {
                stack[sp] = (cid, child_ox, child_oy);
                sp += 1;
            }
        }
    }

    // Resolve cursor shape with inheritance: walk up from hit node to
    // the first ancestor with a non-inherit cursor_shape declaration.
    let mut cursor_node = hit;
    while cursor_node != scene::NULL && (cursor_node as usize) < nodes.len() {
        let shape = nodes[cursor_node as usize].cursor_shape;
        if shape != scene::CURSOR_INHERIT {
            return match shape {
                scene::CURSOR_TEXT => "cursor-text",
                _ => "pointer",
            };
        }
        cursor_node = parent[cursor_node as usize & 63];
    }

    "pointer"
}

/// Write cursor shape data to the CursorState shared page.
///
/// Looks up the icon, concatenates its path commands, and writes the
/// header + data. Bumps shape_generation with a store-release so the
/// render driver sees the complete write.
fn write_cursor_shape(cursor_state_va: usize, generation: &mut u32, icon_name: &str) {
    use protocol::view::{CursorState, CURSOR_DATA_OFFSET};

    let icon = icon_lib::get(icon_name, None);

    // Concatenate all sub-path commands.
    let mut data_len: u32 = 0;
    for path in icon.paths {
        let bytes = path.commands;
        let dst_offset = CURSOR_DATA_OFFSET + data_len as usize;
        // SAFETY: cursor_state_va is a valid RW page from init.
        unsafe {
            let dst = (cursor_state_va + dst_offset) as *mut u8;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }
        data_len += bytes.len() as u32;
    }

    // Hotspot depends on cursor shape.
    let (hotspot_x, hotspot_y) = match icon_name {
        "pointer" => (4.0_f32, 4.0_f32),       // arrow tip in 24×24 viewbox
        "cursor-text" => (12.0_f32, 12.0_f32), // I-beam center
        _ => (0.0_f32, 0.0_f32),
    };

    // Icons with open paths must be rendered stroke-only — filling them
    // implicitly closes arcs with straight lines, creating solid wedges.
    let flags = if icon.all_paths_closed() {
        0
    } else {
        CursorState::FLAG_STROKE_ONLY
    };

    // Write header fields (non-atomic — protected by generation protocol).
    // SAFETY: cursor_state_va points to a valid CursorState page.
    unsafe {
        let header = cursor_state_va as *mut CursorState;
        (*header).viewbox = icon.viewbox;
        (*header).stroke_width = icon.stroke_width;
        (*header).hotspot_x = hotspot_x;
        (*header).hotspot_y = hotspot_y;
        (*header).fill_color = CursorState::pack_color(0, 0, 0, 255); // black body
        (*header).stroke_color = CursorState::pack_color(255, 255, 255, 255); // white outline
        (*header).data_len = data_len;
        (*header).flags = flags;
    }

    // Bump generation with store-release.
    *generation = generation.wrapping_add(1);
    unsafe {
        let gen_ptr = cursor_state_va as *const core::sync::atomic::AtomicU32;
        (*gen_ptr).store(*generation, core::sync::atomic::Ordering::Release);
    }
}

/// Write cursor opacity to the CursorState page (independent of shape).
fn write_cursor_opacity(cursor_state_va: usize, opacity: u8) {
    // SAFETY: cursor_state_va is a valid shared page.
    // opacity field is at offset 4 (after shape_generation u32).
    unsafe {
        let opacity_ptr = (cursor_state_va + 4) as *const core::sync::atomic::AtomicU32;
        (*opacity_ptr).store(opacity as u32, core::sync::atomic::Ordering::Release);
    }
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

// ── Layout service communication ─────────────────────────────────

/// Write viewport state to the shared register so B can read it.
fn write_viewport_state(
    fb_width: u32,
    fb_height: u32,
    scroll_y: scene::Mpt,
    page_width: u32,
    page_height: u32,
    text_inset: u32,
) {
    let s = state();
    if s.viewport_state_va == 0 {
        return;
    }
    let text_area_h = page_height.saturating_sub(2 * text_inset);
    let doc_format = match s.doc_format {
        DocumentFormat::Plain => 0u32,
        DocumentFormat::Rich => 1u32,
    };
    let vp = ViewportState {
        generation: s.layout_generation.wrapping_add(1),
        scroll_y_mpt: scroll_y as i32,
        viewport_width_pt: fb_width,
        viewport_height_pt: text_area_h,
        page_width_pt: page_width,
        page_height_pt: page_height,
        text_inset_x: text_inset,
        font_size: FONT_SIZE as u16,
        _pad0: 0,
        char_width_fx: s.char_w_fx,
        line_height: s.line_h,
        doc_format,
        doc_len: s.doc_len as u32,
        _reserved: [0; 4],
    };
    // SAFETY: viewport_state_va points to mapped shared memory, ViewportState is 64 bytes.
    unsafe {
        let ptr = s.viewport_state_va as *mut ViewportState;
        core::ptr::write_volatile(ptr, vp);
        // Store-release on generation.
        let gen_ptr = s.viewport_state_va as *const core::sync::atomic::AtomicU32;
        (*gen_ptr).store(vp.generation, core::sync::atomic::Ordering::Release);
    }
}

/// Signal B to recompute layout.
fn signal_layout_recompute(layout_ch: &ipc::Channel) {
    let msg = ipc::Message::new(MSG_LAYOUT_RECOMPUTE);
    layout_ch.send(&msg);
    let _ = sys::channel_signal(state().layout_handle);
}

/// Read layout results header from shared memory.
fn read_layout_header() -> Option<LayoutResultsHeader> {
    let s = state();
    if s.layout_results_va == 0 {
        return None;
    }
    let ptr = s.layout_results_va as *const LayoutResultsHeader;
    // Load-acquire on generation to ensure we see all data written by B.
    let generation = unsafe {
        let gen_ptr = s.layout_results_va as *const core::sync::atomic::AtomicU32;
        (*gen_ptr).load(core::sync::atomic::Ordering::Acquire)
    };
    if generation == 0 {
        return None;
    }
    // SAFETY: layout_results_va points to mapped shared memory with valid header.
    let header = unsafe { core::ptr::read_volatile(ptr) };
    Some(header)
}

/// Read a LineInfo entry from the layout results.
fn read_line_info(index: usize) -> LineInfo {
    let s = state();
    let off = layout_proto::line_info_offset() + index * core::mem::size_of::<LineInfo>();
    let ptr = (s.layout_results_va + off) as *const LineInfo;
    // SAFETY: within mapped region, index checked by caller.
    unsafe { core::ptr::read(ptr) }
}

/// Read a VisibleRun entry from the layout results.
fn read_visible_run(header: &LayoutResultsHeader, index: usize) -> VisibleRun {
    let s = state();
    let off = layout_proto::visible_run_offset(header.total_line_count)
        + index * core::mem::size_of::<VisibleRun>();
    let ptr = (s.layout_results_va + off) as *const VisibleRun;
    // SAFETY: within mapped region, index checked by caller.
    unsafe { core::ptr::read(ptr) }
}

/// Read glyph data from the layout results.
fn read_glyph_data(
    header: &LayoutResultsHeader,
    offset: u32,
    count: u16,
) -> &'static [scene::ShapedGlyph] {
    let s = state();
    let glyph_base =
        layout_proto::glyph_data_offset(header.total_line_count, header.visible_run_count);
    let ptr = (s.layout_results_va + glyph_base + offset as usize) as *const scene::ShapedGlyph;
    // SAFETY: within mapped region, offset + count*16 within glyph_data_used.
    unsafe { core::slice::from_raw_parts(ptr, count as usize) }
}

/// Read the style registry from the layout results (raw bytes).
fn read_layout_style_registry(header: &LayoutResultsHeader) -> &'static [u8] {
    let s = state();
    let off = layout_proto::style_registry_offset(
        header.total_line_count,
        header.visible_run_count,
        header.glyph_data_used,
    );
    let ptr = (s.layout_results_va + off) as *const u8;
    let len = header.style_registry_size as usize;
    if len == 0 {
        return &[];
    }
    // SAFETY: within mapped region.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// Get a slice of document text for a given byte range.
/// For plain text, slices doc_content(). For rich text, extracts from
/// the piece table into a static scratch buffer (single-threaded, safe).
fn doc_text_for_range(start: usize, end: usize) -> &'static [u8] {
    if state().doc_format == DocumentFormat::Rich {
        // Rich text: extract into a static scratch buffer.
        // SAFETY: Single-threaded userspace process.
        static mut SCRATCH: [u8; 4096] = [0u8; 4096];
        let needed = end.saturating_sub(start);
        if needed == 0 || needed > 4096 {
            return &[];
        }
        let buf = documents::rich_buf_ref();
        // SAFETY: single-threaded, SCRATCH only used within this function scope.
        // text_slice expects (start, end) — not (start, length).
        unsafe {
            let copied = piecetable::text_slice(buf, start as u32, end as u32, &mut SCRATCH);
            &SCRATCH[..copied]
        }
    } else {
        let doc = documents::doc_content();
        if start >= doc.len() {
            return &[];
        }
        let actual_end = end.min(doc.len());
        &doc[start..actual_end]
    }
}

/// Get cached LineInfo entries from B's layout results.
fn cached_line_info() -> &'static [protocol::layout::LineInfo] {
    &state().cached_lines
}

/// Refresh cached LineInfo from B's shared memory.
fn refresh_cached_lines(header: &LayoutResultsHeader) {
    let s = state();
    s.cached_lines.clear();
    for i in 0..header.total_line_count as usize {
        let li = read_line_info(i);
        s.cached_lines.push(li);
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
            s.animation.timer_handle = handle;
            s.animation.timer_active = true;
            true
        }
        Err(_) => {
            state().animation.timer_active = false;
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

    sys::print(b"  \xF0\x9F\xA7\xA0 presenter - starting\n");

    // Read presenter config from init channel.
    // SAFETY: channel_shm_va(0) is the base of the init channel SHM region mapped by the kernel;
    // alignment guaranteed by page-boundary allocation.
    let init_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_CORE_CONFIG {
        sys::print(b"presenter: no config message\n");
        sys::exit();
    }

    let Some(init_proto::CoreMessage::CoreConfig(config)) =
        init_proto::decode_core(msg.msg_type, &msg.payload)
    else {
        sys::print(b"presenter: bad config payload\n");
        sys::exit();
    };
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;

    // Read frame rate from separate message (CoreConfig is full at 56 bytes).
    // Init sends FrameRateMsg immediately after CoreConfig on the same channel.
    let _ = sys::wait(&[0], 100_000_000); // 100ms timeout on init channel
    let frame_rate: u64 = if let Some(init_proto::CoreMessage::FrameRate(fr)) = init_ch
        .try_recv(&mut msg)
        .then(|| init_proto::decode_core(msg.msg_type, &msg.payload))
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

    // Read layout config (layout results VA + viewport state VA).
    let _ = sys::wait(&[0], 100_000_000);
    if let Some(layout_proto::Message::CoreLayoutConfig(lc)) = init_ch
        .try_recv(&mut msg)
        .then(|| layout_proto::decode(msg.msg_type, &msg.payload))
        .flatten()
    {
        let s = state();
        s.layout_results_va = lc.layout_results_va as usize;
        s.layout_results_capacity = lc.layout_results_capacity as usize;
        s.viewport_state_va = lc.viewport_state_va as usize;
        s.input_handle = sys::ChannelHandle(lc.input_handle);
        s.compositor_handle = sys::ChannelHandle(lc.compositor_handle);
        s.editor_handle = sys::ChannelHandle(lc.editor_handle);
        s.docmodel_handle = sys::ChannelHandle(lc.docmodel_handle);
        s.layout_handle = sys::ChannelHandle(lc.layout_handle);
        s.input2_handle = sys::ChannelHandle(lc.input2_handle);
        sys::print(b"     layout config received\n");
    } else {
        sys::print(b"presenter: no layout config\n");
    }

    if config.doc_va == 0 || config.scene_va == 0 {
        sys::print(b"presenter: bad config\n");
        sys::exit();
    }

    {
        let s = state();
        s.doc_buf = config.doc_va as *mut u8;
        s.doc_capacity = config.doc_capacity as usize;
        s.doc_len = 0;
        s.pointer.input_state_va = config.input_state_va as usize;
        s.pointer.cursor_state_va = config.cursor_state_va as usize;
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
            if let Some(fm) = fonts::metrics::font_metrics(font_data) {
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
                let space_gid = fonts::metrics::glyph_id_for_char(font_data, ' ').unwrap_or(0);
                let (advance_fu, _) =
                    fonts::metrics::glyph_h_metrics(font_data, space_gid).unwrap_or((0, 0));
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
            if let Some(fm) = fonts::metrics::font_metrics(sans_data) {
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
            if let Some(fm) = fonts::metrics::font_metrics(serif_data) {
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
            if let Some(fm) = fonts::metrics::font_metrics(data) {
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
            if let Some(fm) = fonts::metrics::font_metrics(data) {
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
            if let Some(fm) = fonts::metrics::font_metrics(data) {
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
    let input_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().input_handle.0 as usize),
            ipc::PAGE_SIZE,
            1,
        )
    };
    let compositor_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().compositor_handle.0 as usize),
            ipc::PAGE_SIZE,
            0,
        )
    };
    let editor_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().editor_handle.0 as usize),
            ipc::PAGE_SIZE,
            0,
        )
    };
    // presenter↔document channel: presenter received endpoint A (index 0) from init.
    let docmodel_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().docmodel_handle.0 as usize),
            ipc::PAGE_SIZE,
            0,
        )
    };
    // SAFETY: layout channel SHM region is mapped by the kernel.
    let layout_ch = unsafe {
        ipc::Channel::from_base(
            protocol::channel_shm_va(state().layout_handle.0 as usize),
            ipc::PAGE_SIZE,
            1,
        )
    };

    // ── Read remaining init channel messages (fast, already queued) ──

    // RTC config — fast, synchronous (already on init channel).
    if let Some(init_proto::ComposeMessage::RtcConfig(rtc_config)) = init_ch
        .try_recv(&mut msg)
        .then(|| init_proto::decode_compose(msg.msg_type, &msg.payload))
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
    let _ = sys::channel_signal(state().compositor_handle);
    sys::print(b"     loading scene published\n");

    // ── Create animation timer ──────────────────────────────────────
    let mut anim_timer = sys::timer_create(frame_interval_ns).unwrap_or(sys::TimerHandle(255));

    // ── Boot animation loop ─────────────────────────────────────────
    //
    // Wait for document service to load the document and signal us.
    // Animate spinner while waiting. A handles document queries, reads,
    // image decode, and initial undo snapshot.
    let boot_start = sys::counter();
    let boot_timeout_ticks = if state().counter_freq > 0 {
        BOOT_TIMEOUT_NS as u128 * state().counter_freq as u128 / 1_000_000_000
    } else {
        u128::MAX
    } as u64;

    let mut boot_doc_loaded = false;
    let mut boot_spinner_angle: f32 = 0.0;

    loop {
        let _ = sys::wait(
            &[anim_timer.0, state().docmodel_handle.0],
            frame_interval_ns,
        );

        // ── Timer tick: rotate spinner ──────────────────────────────
        if let Ok(_) = sys::wait(&[anim_timer.0], 0) {
            let _ = sys::handle_close(anim_timer.0);
            boot_spinner_angle += SPINNER_ANGLE_DELTA;
            scene.update_spinner(boot_spinner_angle);
            compositor_ch.send(&scene_msg);
            let _ = sys::channel_signal(state().compositor_handle);
            anim_timer = sys::timer_create(frame_interval_ns).unwrap_or(sys::TimerHandle(255));
        }

        // ── Document-model notifications ────────────────────────────
        while docmodel_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_DOC_LOADED => {
                    if let Some(view_proto::Message::DocLoaded(loaded)) =
                        view_proto::decode(msg.msg_type, &msg.payload)
                    {
                        let s = state();
                        s.doc_file_id = loaded.doc_file_id;
                        s.doc_len = loaded.doc_len as usize;
                        s.cursor.pos = loaded.cursor_pos as usize;
                        s.doc_format = if loaded.format == 1 {
                            DocumentFormat::Rich
                        } else {
                            DocumentFormat::Plain
                        };
                        sys::print(b"     document loaded via A\n");
                        boot_doc_loaded = true;
                    }
                }
                MSG_IMAGE_DECODED => {
                    if let Some(view_proto::Message::ImageDecoded(img)) =
                        view_proto::decode(msg.msg_type, &msg.payload)
                    {
                        let s = state();
                        s.image_content_id = img.content_id;
                        s.image_width = img.width;
                        s.image_height = img.height;
                        sys::print(b"     image decoded via A\n");
                    }
                }
                _ => {}
            }
        }

        if boot_doc_loaded {
            sys::print(b"     boot init complete\n");
            break;
        }

        if sys::counter() - boot_start > boot_timeout_ticks {
            sys::print(b"     boot timeout - proceeding with available data\n");
            break;
        }
    }

    // Clean up animation timer.
    let _ = sys::handle_close(anim_timer.0);

    let has_input2 = match sys::wait(&[state().input2_handle.0], 0) {
        Ok(_) => true,
        Err(sys::SyscallError::WouldBlock) => true,
        _ => false,
    };
    let input2_ch = if has_input2 {
        sys::print(b"     tablet input channel detected\n");
        // SAFETY: same invariant as channel_shm_va(1..3) from_base above.
        Some(unsafe {
            ipc::Channel::from_base(
                protocol::channel_shm_va(state().input2_handle.0 as usize),
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

    // Write initial cursor shape (arrow) to cursor state page.
    {
        let s = state();
        if s.pointer.cursor_state_va != 0 {
            write_cursor_shape(
                s.pointer.cursor_state_va,
                &mut s.pointer.shape_generation,
                "pointer",
            );
            write_cursor_opacity(s.pointer.cursor_state_va, 0); // hidden initially
        }
    }

    // Signal layout service for initial layout computation.
    {
        write_viewport_state(
            fb_width,
            fb_height,
            0,
            page_width,
            page_height,
            page_padding,
        );
        signal_layout_recompute(&layout_ch);
        // Wait for B to compute initial layout.
        let _ = sys::wait(&[state().layout_handle.0], 50_000_000); // 50ms
        let mut layout_msg = ipc::Message::new(0);
        while layout_ch.try_recv(&mut layout_msg) {}
    }

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
            s.cursor.pos as u32,
            s.selection.start as u32,
            s.selection.end as u32,
            title_label,
            &time_buf,
            0,
            s.cursor.opacity,
            0,
            0,
        );
        // For rich text, immediately rebuild document content with styled layout.
        if is_rich_doc {
            scene.update_rich_document_content(
                &scene_cfg,
                documents::rich_buf_ref(),
                s.cursor.pos as u32,
                s.selection.start as u32,
                s.selection.end as u32,
                title_label,
                &time_buf,
                0,
                true,
                s.cursor.opacity,
                s.animation.active_space,
            );
            if let Some(header) = read_layout_header() {
                refresh_cached_lines(&header);
            }
        }
    }

    // Signal compositor that first frame is ready.
    let scene_msg = ipc::Message::new(MSG_SCENE_UPDATED);

    compositor_ch.send(&scene_msg);

    let _ = sys::channel_signal(state().compositor_handle);

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

    loop {
        let timer_active = state().animation.timer_active;
        let timer_handle = state().animation.timer_handle;
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
        let any_animating = state().scroll.animating
            || state().animation.slide_animating
            || state().animation.timeline.any_active();

        let timeout_ns: u64 = if any_animating {
            frame_interval_ns
        } else {
            let s = state();
            let elapsed = now_ms.saturating_sub(s.cursor.blink_phase_start_ms);
            let remaining_ms = match s.cursor.blink_phase {
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
        // Snapshot coalescing is now managed by the document service.
        let _ = match (timer_active, has_input2) {
            (true, true) => sys::wait(
                &[
                    state().input_handle.0,
                    state().editor_handle.0,
                    state().docmodel_handle.0,
                    timer_handle.0,
                    state().input2_handle.0,
                    state().layout_handle.0,
                ],
                timeout_ns,
            ),
            (true, false) => sys::wait(
                &[
                    state().input_handle.0,
                    state().editor_handle.0,
                    state().docmodel_handle.0,
                    timer_handle.0,
                    state().layout_handle.0,
                ],
                timeout_ns,
            ),
            (false, true) => sys::wait(
                &[
                    state().input_handle.0,
                    state().editor_handle.0,
                    state().docmodel_handle.0,
                    state().input2_handle.0,
                    state().layout_handle.0,
                ],
                timeout_ns,
            ),
            (false, false) => sys::wait(
                &[
                    state().input_handle.0,
                    state().editor_handle.0,
                    state().docmodel_handle.0,
                    state().layout_handle.0,
                ],
                timeout_ns,
            ),
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

        // ── Read pointer state register (BEFORE button events) ─────
        //
        // The input driver writes pointer position to a shared memory
        // register (atomic u64). Read it before draining event rings so
        // that button events see the current position, not the previous
        // frame's position. Critical for click coordinate accuracy and
        // double/triple-click same-spot detection.
        {
            let s = state();
            // SAFETY: input_state_va points to a PointerState page mapped
            // by init. Atomic load-acquire for cross-core visibility.
            let packed = unsafe {
                let atom = &*(s.pointer.input_state_va as *const core::sync::atomic::AtomicU64);
                atom.load(core::sync::atomic::Ordering::Acquire)
            };
            if packed != s.pointer.last_xy && packed != 0 {
                s.pointer.last_xy = packed;
                let x = protocol::input::PointerState::unpack_x(packed);
                let y = protocol::input::PointerState::unpack_y(packed);
                s.pointer.x = scale_pointer_coord(x, fb_width);
                s.pointer.y = scale_pointer_coord(y, fb_height);

                // Cancel any pending fade-out and restore full opacity.
                if let Some(id) = s.pointer.fade_id {
                    s.animation.timeline.cancel(id);
                    s.pointer.fade_id = None;
                }
                s.pointer.visible = true;
                if s.pointer.opacity != 255 {
                    s.pointer.opacity = 255;
                    if s.pointer.cursor_state_va != 0 {
                        write_cursor_opacity(s.pointer.cursor_state_va, 255);
                    }
                    changed = true;
                }
                s.pointer.last_event_ms = now_ms;

                pointer_position_changed = true;
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
                // Forward pending delete to document service.
                if let Some((start, end)) = action.pending_delete {
                    let del = protocol::edit::WriteDeleteRange { start, end };
                    // SAFETY: WriteDeleteRange is repr(C) and fits in 60-byte payload.
                    let del_msg = unsafe {
                        ipc::Message::from_payload(protocol::edit::MSG_WRITE_DELETE_RANGE, &del)
                    };
                    docmodel_ch.send(&del_msg);
                    let _ = sys::channel_signal(state().docmodel_handle);
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
                        if let Some((start, end)) = action.pending_delete {
                            let del = protocol::edit::WriteDeleteRange { start, end };
                            let del_msg = unsafe {
                                ipc::Message::from_payload(
                                    protocol::edit::MSG_WRITE_DELETE_RANGE,
                                    &del,
                                )
                            };
                            docmodel_ch.send(&del_msg);
                            let _ = sys::channel_signal(state().docmodel_handle);
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
                            let click_x = s.pointer.x;
                            let click_y = s.pointer.y;

                            if click_y >= TITLE_BAR_H && s.animation.active_space == 0 {
                                // Text origin = page position + padding.
                                let page_x = (fb_width - page_width) / 2;
                                let page_y_abs =
                                    content_y + (content_h.saturating_sub(page_height)) / 2;
                                let text_origin_x = page_x + page_padding;
                                let text_origin_y = page_y_abs + page_padding;
                                let rel_x = click_x.saturating_sub(text_origin_x);
                                let rel_y = click_y.saturating_sub(text_origin_y);
                                let adjusted_y =
                                    rel_y + (s.scroll.offset / scene::MPT_PER_PT) as u32;
                                let is_rich = state().doc_format == DocumentFormat::Rich;
                                let byte_pos = if is_rich {
                                    // Rich text: proportional hit test using cached layout.
                                    layout::rich_xy_to_byte(rel_x as f32, adjusted_y as f32)
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
                                let dx = if click_x > s.selection.last_click_x {
                                    click_x - s.selection.last_click_x
                                } else {
                                    s.selection.last_click_x - click_x
                                };
                                let dy = if click_y > s.selection.last_click_y {
                                    click_y - s.selection.last_click_y
                                } else {
                                    s.selection.last_click_y - click_y
                                };
                                let dt = now_ms.saturating_sub(s.selection.last_click_ms);
                                let same_spot = dx <= 4 && dy <= 4 && dt <= 400;

                                let click_count = if same_spot {
                                    // Cycle: 1 → 2 → 3 → 1 → ...
                                    (s.selection.click_count % 3) + 1
                                } else {
                                    1
                                };

                                {
                                    let s = state();
                                    s.selection.last_click_ms = now_ms;
                                    s.selection.last_click_x = click_x;
                                    s.selection.last_click_y = click_y;
                                    s.selection.click_count = click_count;
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
                                        s.selection.anchor = lo;
                                        s.cursor.pos = hi;
                                        s.selection.active = hi > lo;
                                        input_handling::update_selection_from_anchor();
                                    }
                                    3 => {
                                        // Triple-click: select entire visual line.
                                        let (lo, mut hi) = if is_rich {
                                            // Rich text: use B's cached LineInfo for line boundaries.
                                            let cached = cached_line_info();
                                            let line_idx =
                                                layout::line_info_byte_to_line(cached, byte_pos);
                                            if line_idx < cached.len() {
                                                let li = &cached[line_idx];
                                                let start = li.byte_offset as usize;
                                                let end = start + li.byte_length as usize;
                                                (start, end)
                                            } else {
                                                (0, text.len())
                                            }
                                        } else {
                                            let lo = input_handling::visual_line_start(
                                                text, byte_pos, cols,
                                            );
                                            let hi = input_handling::visual_line_end(
                                                text, byte_pos, cols,
                                            );
                                            (lo, hi)
                                        };
                                        // Include the newline if present.
                                        if hi < text.len() && text[hi] == b'\n' {
                                            hi += 1;
                                        }
                                        let s = state();
                                        s.selection.anchor = lo;
                                        s.cursor.pos = hi;
                                        s.selection.active = hi > lo;
                                        input_handling::update_selection_from_anchor();
                                    }
                                    _ => {
                                        // Single click: position cursor, set anchor for potential drag.
                                        // clear_selection() resets anchor to 0, so set it after.
                                        input_handling::clear_selection();
                                        let s = state();
                                        s.cursor.pos = byte_pos;
                                        s.selection.anchor = byte_pos;
                                    }
                                }

                                // Start drag tracking. For double/triple-click, remember
                                // the original word/line boundaries so drag extends in
                                // those increments (matching macOS behavior).
                                {
                                    let s = state();
                                    s.selection.dragging = true;
                                    s.selection.drag_origin_start = s.selection.anchor;
                                    s.selection.drag_origin_end = s.cursor.pos;
                                }

                                state().cursor.goal_column = None;
                                state().cursor.goal_x = None;
                                documents::doc_write_header();
                                input_handling::sync_cursor_to_editor(&editor_ch);

                                let _ = sys::channel_signal(state().editor_handle);

                                changed = true;
                                selection_changed = true;
                                had_user_input = true;
                            }
                        } else if btn.button == 0 && btn.pressed == 0 {
                            // Button release: end drag.
                            state().selection.dragging = false;
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Drag selection: update on pointer move while button held ──
        if pointer_position_changed && state().selection.dragging {
            let s = state();
            let drag_x = s.pointer.x;
            let drag_y = s.pointer.y;
            let click_count = s.selection.click_count;

            if drag_y >= TITLE_BAR_H && s.animation.active_space == 0 {
                let page_x = (fb_width - page_width) / 2;
                let page_y_abs = content_y + (content_h.saturating_sub(page_height)) / 2;
                let text_origin_x = page_x + page_padding;
                let text_origin_y = page_y_abs + page_padding;
                let rel_x = drag_x.saturating_sub(text_origin_x);
                let rel_y = drag_y.saturating_sub(text_origin_y);
                let adjusted_y = rel_y + (s.scroll.offset / scene::MPT_PER_PT) as u32;
                let is_rich = s.doc_format == DocumentFormat::Rich;

                let byte_pos = if is_rich {
                    layout::rich_xy_to_byte(rel_x as f32, adjusted_y as f32)
                } else {
                    let li = content_text_layout(page_width, page_padding);
                    let t = documents::doc_content();
                    li.xy_to_byte(t, rel_x, adjusted_y)
                };

                match click_count {
                    2 => {
                        // Word-granularity drag: extend in word-sized increments.
                        let text_buf: alloc::vec::Vec<u8>;
                        let text: &[u8] = if is_rich {
                            let tl = documents::rich_text_len();
                            text_buf = {
                                let mut v = alloc::vec![0u8; tl];
                                documents::rich_copy_text(&mut v);
                                v
                            };
                            &text_buf
                        } else {
                            text_buf = alloc::vec::Vec::new();
                            documents::doc_content()
                        };

                        let s = state();
                        let origin_start = s.selection.drag_origin_start;
                        let origin_end = s.selection.drag_origin_end;

                        if byte_pos < origin_start {
                            // Dragging before the origin word: snap backward to word start.
                            let lo = input_handling::word_boundary_backward(text, byte_pos);
                            s.selection.anchor = origin_end;
                            s.cursor.pos = lo;
                        } else if byte_pos >= origin_end {
                            // Dragging after the origin word: snap forward to word end.
                            let mut hi = byte_pos;
                            while hi < text.len() && !layout_lib::is_whitespace(text[hi]) {
                                hi += 1;
                            }
                            s.selection.anchor = origin_start;
                            s.cursor.pos = hi;
                        } else {
                            // Still within the origin word: keep original selection.
                            s.selection.anchor = origin_start;
                            s.cursor.pos = origin_end;
                        }
                        s.selection.active = s.selection.anchor != s.cursor.pos;
                    }
                    3 => {
                        // Line-granularity drag: extend in line-sized increments.
                        let text_buf: alloc::vec::Vec<u8>;
                        let text: &[u8] = if is_rich {
                            let tl = documents::rich_text_len();
                            text_buf = {
                                let mut v = alloc::vec![0u8; tl];
                                documents::rich_copy_text(&mut v);
                                v
                            };
                            &text_buf
                        } else {
                            text_buf = alloc::vec::Vec::new();
                            documents::doc_content()
                        };

                        let s = state();
                        let origin_start = s.selection.drag_origin_start;
                        let origin_end = s.selection.drag_origin_end;
                        let cols = content_text_layout(page_width, page_padding).cols();

                        if byte_pos < origin_start {
                            let lo = if is_rich {
                                let cached = cached_line_info();
                                let li_idx = layout::line_info_byte_to_line(cached, byte_pos);
                                if li_idx < cached.len() {
                                    cached[li_idx].byte_offset as usize
                                } else {
                                    0
                                }
                            } else {
                                input_handling::visual_line_start(text, byte_pos, cols)
                            };
                            s.selection.anchor = origin_end;
                            s.cursor.pos = lo;
                        } else if byte_pos >= origin_end {
                            let mut hi = if is_rich {
                                let cached = cached_line_info();
                                let li_idx = layout::line_info_byte_to_line(cached, byte_pos);
                                if li_idx < cached.len() {
                                    let li = &cached[li_idx];
                                    (li.byte_offset + li.byte_length) as usize
                                } else {
                                    text.len()
                                }
                            } else {
                                input_handling::visual_line_end(text, byte_pos, cols)
                            };
                            if hi < text.len() && text[hi] == b'\n' {
                                hi += 1;
                            }
                            s.selection.anchor = origin_start;
                            s.cursor.pos = hi;
                        } else {
                            s.selection.anchor = origin_start;
                            s.cursor.pos = origin_end;
                        }
                        s.selection.active = s.selection.anchor != s.cursor.pos;
                    }
                    _ => {
                        // Character-granularity drag.
                        let s = state();
                        s.cursor.pos = byte_pos;
                        s.selection.active = s.selection.anchor != byte_pos;
                    }
                }

                input_handling::update_selection_from_anchor();
                documents::doc_write_header();
                input_handling::sync_cursor_to_editor(&editor_ch);
                let _ = sys::channel_signal(state().editor_handle);

                changed = true;
                selection_changed = true;
                had_user_input = true;
            }
        }

        // Process document service notifications.
        // A signals core when the document buffer changes (edits, undo/redo).
        while docmodel_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_DOC_CHANGED => {
                    if let Some(view_proto::Message::DocChanged(dc)) =
                        view_proto::decode(msg.msg_type, &msg.payload)
                    {
                        let s = state();
                        s.doc_len = dc.doc_len as usize;
                        s.cursor.pos = dc.cursor_pos as usize;
                        if dc.flags & DOC_CHANGED_CLEAR_SELECTION != 0 {
                            input_handling::clear_selection();
                        }
                        // Sync cursor to editor so it tracks the new position.
                        input_handling::sync_cursor_to_editor(&editor_ch);
                        let _ = sys::channel_signal(state().editor_handle);
                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_IMAGE_DECODED => {
                    if let Some(view_proto::Message::ImageDecoded(img)) =
                        view_proto::decode(msg.msg_type, &msg.payload)
                    {
                        let s = state();
                        s.image_content_id = img.content_id;
                        s.image_width = img.width;
                        s.image_height = img.height;
                        changed = true;
                    }
                }
                _ => {}
            }
        }

        // ── Undo / redo ──────────────────────────────────────────────────
        //
        // Forward undo/redo requests to document service. A manages the
        // undo ring and communicates with the document service. When A
        // completes the operation, it sends MSG_DOC_CHANGED which we
        // process on the next iteration.
        if undo_requested {
            let undo_msg = ipc::Message::new(MSG_UNDO_REQUEST);
            docmodel_ch.send(&undo_msg);
            let _ = sys::channel_signal(state().docmodel_handle);
        } else if redo_requested {
            let redo_msg = ipc::Message::new(MSG_REDO_REQUEST);
            docmodel_ch.send(&redo_msg);
            let _ = sys::channel_signal(state().docmodel_handle);
        }

        // Update scroll offset for cursor/text changes.
        // Track whether the scroll position actually changed — the scene
        // dispatch needs to know so it does a full rebuild (visible lines
        // differ) instead of an incremental single-line update.
        let scroll_before = state().scroll.offset;
        if (changed || text_changed) && state().animation.active_space == 0 {
            input_handling::update_scroll_offset(page_width, page_height, page_padding);
        }
        let scroll_after = state().scroll.offset;
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
        state().animation.timeline.tick(now_ms);
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

        if state().scroll.animating {
            let old_scroll = state().scroll.offset;
            let s = state();
            s.scroll.spring.tick(frame_dt);
            s.scroll.offset = scene::f32_to_mpt(s.scroll.spring.value());

            if s.scroll.spring.settled() {
                // Snap to exact target (nearest whole-point-aligned Mpt)
                // to avoid persistent sub-pixel jitter.
                s.scroll.offset = scene::mpt_round_pt(s.scroll.target);
                s.scroll.animating = false;
            }

            let new_scroll = state().scroll.offset;
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
        //
        // Exception: during an active drag, skip the fade-in and show the
        // highlight at full opacity immediately. Without this, every pointer
        // move restarts the 100ms fade from 0, so the highlight never becomes
        // visible until the drag ends.
        if selection_changed {
            let s = state();
            if s.selection.dragging {
                // Drag in progress — full opacity, no animation.
                if let Some(old_id) = s.selection.fade_id {
                    s.animation.timeline.cancel(old_id);
                    s.selection.fade_id = None;
                }
                s.selection.opacity = 255;
            } else {
                // Discrete selection change (click, keyboard) — fade in.
                if let Some(old_id) = s.selection.fade_id {
                    s.animation.timeline.cancel(old_id);
                }
                s.selection.fade_id = s
                    .animation
                    .timeline
                    .start(0.0, 255.0, 100, animation::Easing::EaseOut, now_ms)
                    .ok();
                s.selection.opacity = 0;
            }
        }
        // Tick the selection fade (if active).
        {
            let s = state();
            if let Some(id) = s.selection.fade_id {
                if s.animation.timeline.is_active(id) {
                    let new_val = s.animation.timeline.value(id) as u8;
                    if new_val != s.selection.opacity {
                        s.selection.opacity = new_val;
                        changed = true;
                    }
                } else {
                    s.selection.opacity = 255;
                    s.selection.fade_id = None;
                }
            }
        }

        // ── Document switch slide animation ─────────────────────────
        //
        // Ctrl+Tab sets slide_target to the next space. The spring
        // animates slide_offset toward the target. We update N_STRIP's
        // child_offset each frame via apply_slide.
        //
        // The slide does NOT set `changed` — it uses its own publish
        // path (apply_slide) and compositor signal. Setting `changed`
        // would trigger an unnecessary update_cursor dispatch.
        if state().animation.slide_animating {
            let s = state();
            // On the first frame, frame_dt includes idle sleep time
            // (up to 50ms) — advancing the spring by that much causes
            // a visible first-frame jump. Clamp to one frame interval
            // for smooth onset; subsequent frames use wall-clock dt.
            let dt = if s.animation.slide_first_frame {
                s.animation.slide_first_frame = false;
                (frame_interval_ns as f32) / 1_000_000_000.0
            } else {
                frame_dt
            };
            s.animation.slide_spring.tick(dt);
            let new_offset = scene::f32_to_mpt(s.animation.slide_spring.value());
            if new_offset != s.animation.slide_offset {
                s.animation.slide_offset = new_offset;
                slide_changed = true;
            }
            if s.animation.slide_spring.settled() {
                s.animation.slide_offset = s.animation.slide_target; // both Mpt, exact match
                s.animation.slide_animating = false;
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
            if s.pointer.visible && s.pointer.fade_id.is_none() && s.pointer.opacity == 255 {
                let idle_ms = now_ms.saturating_sub(s.pointer.last_event_ms);
                if idle_ms >= POINTER_HIDE_MS {
                    s.pointer.fade_id = s
                        .animation
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
            if let Some(id) = s.pointer.fade_id {
                if s.animation.timeline.is_active(id) {
                    let new_opacity = s.animation.timeline.value(id) as u8;
                    if new_opacity != s.pointer.opacity {
                        s.pointer.opacity = new_opacity;
                        if s.pointer.cursor_state_va != 0 {
                            write_cursor_opacity(s.pointer.cursor_state_va, new_opacity);
                        }
                        changed = true;
                    }
                } else {
                    // Fade complete — pointer is now hidden.
                    s.pointer.opacity = 0;
                    s.pointer.visible = false;
                    s.pointer.fade_id = None;
                    if s.pointer.cursor_state_va != 0 {
                        write_cursor_opacity(s.pointer.cursor_state_va, 0);
                    }
                    changed = true;
                }
            }
        }

        // ── Scene update dispatch ──────────────────────────────────
        //
        // Use targeted updates for incremental changes instead of
        // rebuilding the entire scene graph every frame.
        //
        // ── Signal layout service for recompute if needed ────────
        //
        // Write viewport state and signal B when document content or scroll
        // position changed. B will recompute layout and signal back with
        // MSG_LAYOUT_READY. We drain the ready signal before scene dispatch.
        if text_changed || context_switched {
            write_viewport_state(
                fb_width,
                fb_height,
                state().scroll.offset,
                page_width,
                page_height,
                page_padding,
            );
            signal_layout_recompute(&layout_ch);
            // Wait briefly for B to finish layout (shared memory, fast).
            let _ = sys::wait(&[state().layout_handle.0], 5_000_000); // 5ms
                                                                      // Drain layout-ready signal.
            let mut layout_msg = ipc::Message::new(0);
            while layout_ch.try_recv(&mut layout_msg) {
                // MSG_LAYOUT_READY — results now in shared memory.
            }
        }

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
                let title: &[u8] = if s.animation.active_space != 0 {
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
                    s.cursor.pos as u32,
                    s.selection.start as u32,
                    s.selection.end as u32,
                    title,
                    &time_buf,
                    s.scroll.offset,
                    s.cursor.opacity,
                    s.animation.slide_offset,
                    s.animation.active_space,
                );
                // Immediately rebuild document content with rich text layout.
                if is_rich_doc {
                    scene.update_rich_document_content(
                        &scene_cfg,
                        documents::rich_buf_ref(),
                        s.cursor.pos as u32,
                        s.selection.start as u32,
                        s.selection.end as u32,
                        title,
                        &time_buf,
                        s.scroll.offset,
                        true,
                        s.cursor.opacity,
                        s.animation.active_space,
                    );
                    if let Some(header) = read_layout_header() {
                        refresh_cached_lines(&header);
                    }
                }
            } else if text_changed && is_rich_doc {
                // Rich text content changed — always full rebuild.
                let s = state();
                let title: &[u8] = if s.animation.active_space != 0 {
                    b"Image"
                } else {
                    b"Rich Text"
                };
                scene.update_rich_document_content(
                    &scene_cfg,
                    documents::rich_buf_ref(),
                    s.cursor.pos as u32,
                    s.selection.start as u32,
                    s.selection.end as u32,
                    title,
                    &time_buf,
                    s.scroll.offset,
                    clock_changed,
                    s.cursor.opacity,
                    s.animation.active_space,
                );
                if let Some(header) = read_layout_header() {
                    refresh_cached_lines(&header);
                }
            } else if text_changed {
                // Plain text content changed (insert/delete/scroll).
                let doc = documents::doc_content();
                let new_line_count = scene_state::count_lines(doc);

                if scroll_changed {
                    let s = state();
                    scene.update_document_content(
                        &scene_cfg,
                        doc,
                        s.cursor.pos as u32,
                        s.selection.start as u32,
                        s.selection.end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll.offset,
                        clock_changed,
                        s.cursor.opacity,
                        s.animation.active_space,
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
                    let changed_line = scene_state::byte_to_line_col(doc, s.cursor.pos, cpl).0;
                    scene.update_document_incremental(
                        &scene_cfg,
                        doc,
                        s.cursor.pos as u32,
                        s.selection.start as u32,
                        s.selection.end as u32,
                        changed_line,
                        b"Text",
                        &time_buf,
                        s.scroll.offset,
                        clock_changed,
                        s.cursor.opacity,
                        s.animation.active_space,
                    );
                } else if new_line_count == prev_line_count + 1 {
                    let s = state();
                    scene.update_document_insert_line(
                        &scene_cfg,
                        doc,
                        s.cursor.pos as u32,
                        s.selection.start as u32,
                        s.selection.end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll.offset,
                        clock_changed,
                        s.cursor.opacity,
                        s.animation.active_space,
                    );
                } else if new_line_count + 1 == prev_line_count {
                    let s = state();
                    scene.update_document_delete_line(
                        &scene_cfg,
                        doc,
                        s.cursor.pos as u32,
                        s.selection.start as u32,
                        s.selection.end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll.offset,
                        clock_changed,
                        s.cursor.opacity,
                        s.animation.active_space,
                    );
                } else {
                    let s = state();
                    scene.update_document_content(
                        &scene_cfg,
                        doc,
                        s.cursor.pos as u32,
                        s.selection.start as u32,
                        s.selection.end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll.offset,
                        clock_changed,
                        s.cursor.opacity,
                        s.animation.active_space,
                    );
                }

                prev_line_count = new_line_count;
            } else if selection_changed && is_rich_doc {
                // Rich text selection — full rebuild (proportional positioning).
                let s = state();
                let title: &[u8] = if s.animation.active_space != 0 {
                    b"Image"
                } else {
                    b"Rich Text"
                };
                scene.update_rich_document_content(
                    &scene_cfg,
                    documents::rich_buf_ref(),
                    s.cursor.pos as u32,
                    s.selection.start as u32,
                    s.selection.end as u32,
                    title,
                    &time_buf,
                    s.scroll.offset,
                    clock_changed,
                    s.cursor.opacity,
                    s.animation.active_space,
                );
                if let Some(header) = read_layout_header() {
                    refresh_cached_lines(&header);
                }
            } else if selection_changed {
                // Mono text selection changed.
                let s = state();
                let sel_text_h = scene_cfg
                    .page_height
                    .saturating_sub(2 * scene_cfg.text_inset_x);
                let scroll_pt = s.scroll.offset / scene::MPT_PER_PT;

                scene.update_selection(
                    &scene_cfg,
                    s.cursor.pos as u32,
                    s.selection.start as u32,
                    s.selection.end as u32,
                    documents::doc_content(),
                    sel_text_h,
                    scroll_pt,
                    s.cursor.opacity,
                );
            } else if changed && is_rich_doc {
                // Rich text cursor-only update — full rebuild needed because
                // proportional cursor positioning requires the styled layout.
                let s = state();
                let title: &[u8] = if s.animation.active_space != 0 {
                    b"Image"
                } else {
                    b"Rich Text"
                };
                scene.update_rich_document_content(
                    &scene_cfg,
                    documents::rich_buf_ref(),
                    s.cursor.pos as u32,
                    s.selection.start as u32,
                    s.selection.end as u32,
                    title,
                    &time_buf,
                    s.scroll.offset,
                    clock_changed,
                    s.cursor.opacity,
                    s.animation.active_space,
                );
                if let Some(header) = read_layout_header() {
                    refresh_cached_lines(&header);
                }
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
                    s.cursor.pos as u32,
                    documents::doc_content(),
                    chars_per_line,
                    if clock_changed { Some(&time_buf) } else { None },
                    s.cursor.opacity,
                );
            } else if clock_changed {
                // Clock changed without any other scene change — just update the clock text.
                scene.update_clock(&scene_cfg, &time_buf);
            }

            // Apply post-build opacity (selection fade-in).
            {
                let s = state();
                scene.apply_opacity(255, s.selection.opacity);
            }

            // Apply slide offset if it changed this frame.
            if slide_changed {
                scene.apply_slide(state().animation.slide_offset);
            }

            // Write cursor opacity to shared register (render driver reads).
            if needs_scene_update {
                let s = state();
                if s.pointer.cursor_state_va != 0 {
                    write_cursor_opacity(s.pointer.cursor_state_va, s.pointer.opacity);
                }
            }
        }

        // ── Cursor shape re-evaluation ──────────────────────────────
        //
        // Re-evaluate whenever either input to the cursor decision changed:
        // pointer position OR scene content (edit, context switch, slide,
        // scroll, animation). This decouples cursor shape from pointer
        // movement — a stationary pointer updates its shape when content
        // changes underneath it.
        if pointer_position_changed || needs_scene_update || slide_changed {
            let s = state();
            if s.pointer.cursor_state_va != 0 {
                let nodes = scene.latest_nodes();
                let data_buf = scene.latest_data_buf();
                let new_shape = resolve_cursor_shape(nodes, data_buf, s.pointer.x, s.pointer.y);
                if !core::ptr::eq(new_shape as *const str, s.pointer.shape_name as *const str) {
                    s.pointer.shape_name = new_shape;
                    write_cursor_shape(
                        s.pointer.cursor_state_va,
                        &mut s.pointer.shape_generation,
                        new_shape,
                    );
                }
            }
        }

        // Signal compositor for scene changes AND pointer-only moves.
        // For pointer-only moves (no scene publish), the render service
        // wakes up, sees no generation change, reads the pointer state
        // register, and sends a cursor-only frame (no full scene walk).
        if needs_scene_update || pointer_position_changed {
            compositor_ch.send(&scene_msg);
            let _ = sys::channel_signal(state().compositor_handle);
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
