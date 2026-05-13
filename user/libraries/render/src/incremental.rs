//! Incremental rendering state: per-node tracking and dirty rect computation.
//!
//! `IncrementalState` persists across frames, storing each node's previous
//! bounds, visibility, scroll offset, and content hash. On each frame, the
//! dirty bitmap from the scene header is compared against previous state to
//! produce a minimal set of dirty rectangles for partial repaint.

use scene::{DIRTY_BITMAP_WORDS, MAX_NODES, NULL, Node, NodeId, abs_bounds, build_parent_map};

use crate::damage::DamageTracker;

// ── IncrementalState ────────────────────────────────────────────────

/// Per-node state persisted across frames for incremental rendering.
pub struct IncrementalState {
    /// Absolute visual bounds of each node from the previous frame.
    pub prev_bounds: [(i32, i32, u32, u32); MAX_NODES],
    /// Bitmap: was node visible last frame? One bit per node.
    pub prev_visible: [u64; DIRTY_BITMAP_WORDS],
    /// Per-node child_offset from the previous frame.
    pub prev_child_offset: [(i32, i32); MAX_NODES],
    /// Per-node content_hash from the previous frame. Used by the render
    /// backends (Tasks 7/8) to detect property-only changes: if a node is
    /// dirty but content_hash is unchanged, the backend can blit from its
    /// per-node cache instead of re-rasterizing.
    pub prev_content_hash: [u32; MAX_NODES],
    /// True until the first frame has been rendered.
    pub first_frame: bool,
}

impl IncrementalState {
    pub fn new() -> Self {
        Self {
            prev_bounds: [(0, 0, 0, 0); MAX_NODES],
            prev_visible: [0u64; DIRTY_BITMAP_WORDS],
            prev_child_offset: [(0, 0); MAX_NODES],
            prev_content_hash: [0u32; MAX_NODES],
            first_frame: true,
        }
    }

    /// Compute dirty rects from dirty bitmap + previous state.
    ///
    /// Returns `Some(tracker)` with the set of rectangles that need
    /// repainting. Returns `None` if a full repaint is needed (first
    /// frame or all dirty bits set).
    ///
    /// When the returned tracker has `count == 0`, nothing changed and
    /// no repaint is needed.
    pub fn compute_dirty_rects(
        &self,
        nodes: &[Node],
        node_count: u16,
        dirty_bits: &[u64; DIRTY_BITMAP_WORDS],
        fb_width: u16,
        fb_height: u16,
    ) -> Option<DamageTracker> {
        // First frame: no previous state to diff against.
        if self.first_frame {
            return None;
        }

        // All dirty bits set means full repaint (e.g., after clear()).
        if all_bits_set(dirty_bits, node_count) {
            return None;
        }

        // No dirty bits means nothing changed.
        if all_bits_zero(dirty_bits) {
            return Some(DamageTracker::new(fb_width, fb_height));
        }

        let count = node_count as usize;
        let parent_map = build_parent_map(nodes, count);
        let mut tracker = DamageTracker::new(fb_width, fb_height);

        for i in iter_set_bits(dirty_bits) {
            if i >= count {
                break;
            }

            let was_visible = bit_is_set(&self.prev_visible, i);
            let is_visible = i < nodes.len() && nodes[i].visible();

            match (was_visible, is_visible) {
                // Deleted: was visible, now invisible. Damage = old bounds.
                (true, false) => {
                    let (ox, oy, ow, oh) = self.prev_bounds[i];

                    add_rect_clamped(&mut tracker, ox, oy, ow, oh, fb_width, fb_height);
                }
                // New: was not visible, now visible. Damage = current bounds.
                (false, true) => {
                    let (cx, cy, cw, ch) = abs_bounds(nodes, &parent_map, i);

                    add_rect_clamped(&mut tracker, cx, cy, cw, ch, fb_width, fb_height);
                }
                // Updated: both visible. Damage = union of old and new bounds.
                (true, true) => {
                    let (ox, oy, ow, oh) = self.prev_bounds[i];
                    let (cx, cy, cw, ch) = abs_bounds(nodes, &parent_map, i);
                    let (ux, uy, uw, uh) = union_bounds(ox, oy, ow, oh, cx, cy, cw, ch);

                    add_rect_clamped(&mut tracker, ux, uy, uw, uh, fb_width, fb_height);
                    // Note: when a container moves, the union of old + new
                    // bounds already covers all children — no extra rect needed.
                }
                // Was not visible, still not visible. Nothing to do.
                (false, false) => {}
            }

            // Early out: tracker overflowed to full-screen.
            if tracker.full_screen {
                return Some(tracker);
            }
        }

        Some(tracker)
    }

    /// Detect child_offset changes on container nodes (scroll/slide).
    ///
    /// Returns `Some((node_id, delta_x, delta_y))` for the first dirty
    /// container whose child_offset changed. Only reports the first
    /// scrolled container — sufficient for the current single-document
    /// model.
    pub fn detect_scroll(
        &self,
        nodes: &[Node],
        dirty_bits: &[u64; DIRTY_BITMAP_WORDS],
    ) -> Option<(NodeId, f32, f32)> {
        for i in iter_set_bits(dirty_bits) {
            if i >= nodes.len() {
                break;
            }

            let node = &nodes[i];
            let (prev_x, prev_y) = self.prev_child_offset[i];

            if node.first_child != NULL
                && (node.child_offset_x != prev_x || node.child_offset_y != prev_y)
            {
                let delta_x = scene::mpt_to_f32(node.child_offset_x - prev_x);
                let delta_y = scene::mpt_to_f32(node.child_offset_y - prev_y);

                return Some((i as NodeId, delta_x, delta_y));
            }
        }
        None
    }

    /// Update previous state from the current frame.
    ///
    /// Call at the END of each frame after rendering.
    pub fn update_from_frame(&mut self, nodes: &[Node], node_count: u16) {
        let count = node_count as usize;
        let parent_map = build_parent_map(nodes, count);

        // Clear all previous visibility bits.
        for word in self.prev_visible.iter_mut() {
            *word = 0;
        }

        for i in 0..count.min(nodes.len()).min(MAX_NODES) {
            if nodes[i].visible() {
                let bounds = abs_bounds(nodes, &parent_map, i);
                self.prev_bounds[i] = bounds;
                set_bit(&mut self.prev_visible, i);
            } else {
                self.prev_bounds[i] = (0, 0, 0, 0);
            }

            self.prev_child_offset[i] = (nodes[i].child_offset_x, nodes[i].child_offset_y);
            self.prev_content_hash[i] = nodes[i].content_hash;
        }

        // Clear state for nodes beyond the current count.
        for i in count..MAX_NODES {
            self.prev_bounds[i] = (0, 0, 0, 0);
            self.prev_child_offset[i] = (0, 0);
            self.prev_content_hash[i] = 0;
            // prev_visible already cleared above.
        }

        self.first_frame = false;
    }
}

// ── Bitmap helpers ──────────────────────────────────────────────────

/// Check whether bit `i` is set in a u64 bitmap.
fn bit_is_set(bitmap: &[u64; DIRTY_BITMAP_WORDS], i: usize) -> bool {
    let word = i / 64;
    let bit = i % 64;

    if word < DIRTY_BITMAP_WORDS {
        bitmap[word] & (1u64 << bit) != 0
    } else {
        false
    }
}

/// Set bit `i` in a u64 bitmap.
fn set_bit(bitmap: &mut [u64; DIRTY_BITMAP_WORDS], i: usize) {
    let word = i / 64;
    let bit = i % 64;

    if word < DIRTY_BITMAP_WORDS {
        bitmap[word] |= 1u64 << bit;
    }
}

/// Are all dirty bits zero?
pub fn all_bits_zero(bits: &[u64; DIRTY_BITMAP_WORDS]) -> bool {
    bits.iter().all(|&w| w == 0)
}

/// Are all bits set for node indices 0..node_count?
fn all_bits_set(bits: &[u64; DIRTY_BITMAP_WORDS], node_count: u16) -> bool {
    let count = node_count as usize;

    if count == 0 {
        return false;
    }

    let full_words = count / 64;
    let remaining = count % 64;

    for i in 0..full_words {
        if bits[i] != u64::MAX {
            return false;
        }
    }

    if remaining > 0 && full_words < DIRTY_BITMAP_WORDS {
        let mask = (1u64 << remaining) - 1;

        if bits[full_words] & mask != mask {
            return false;
        }
    }

    true
}

/// Iterator over set bit indices in a u64 bitmap.
fn iter_set_bits(bits: &[u64; DIRTY_BITMAP_WORDS]) -> SetBitIter<'_> {
    SetBitIter {
        bits,
        word_idx: 0,
        remaining: if DIRTY_BITMAP_WORDS > 0 { bits[0] } else { 0 },
    }
}

struct SetBitIter<'a> {
    bits: &'a [u64; DIRTY_BITMAP_WORDS],
    word_idx: usize,
    remaining: u64,
}

impl Iterator for SetBitIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if self.remaining != 0 {
                let bit = self.remaining.trailing_zeros() as usize;
                self.remaining &= self.remaining - 1; // Clear lowest set bit
                return Some(self.word_idx * 64 + bit);
            }

            self.word_idx += 1;

            if self.word_idx >= DIRTY_BITMAP_WORDS {
                return None;
            }

            self.remaining = self.bits[self.word_idx];
        }
    }
}

// ── Scroll blit-shift ───────────────────────────────────────────────

/// Parameters for a vertical blit-shift operation on the framebuffer.
///
/// All coordinates are in physical pixels. The `exposed` rect describes
/// the newly-revealed strip that must be re-rendered after the shift.
pub struct ScrollBlitParams {
    /// Container's physical x position.
    pub cx: u32,
    /// Container's physical y position.
    pub cy: u32,
    /// Container's physical width.
    pub cw: u32,
    /// Container's physical height (clipped to framebuffer).
    pub ch: u32,
    /// Vertical pixel delta (negative = content moves up / scroll down).
    pub dy_px: i32,
    /// The exposed strip that needs re-rendering.
    pub exposed: crate::DirtyRect,
    /// The scrolled container's node ID.
    pub container_id: scene::NodeId,
}

/// Compute physical pixel blit-shift parameters from a detected scroll.
///
/// Returns `None` if the scroll cannot be optimized via blit-shift:
/// - horizontal scroll (`dx != 0`)
/// - subpixel scroll (rounds to 0 pixels)
/// - scroll amount >= container height
/// - container is zero-sized or off-screen
///
/// `bounds` is the container's previous absolute bounds in point coords
/// (from `IncrementalState::prev_bounds`).
pub fn compute_scroll_blit(
    container_id: scene::NodeId,
    delta_tx: f32,
    delta_ty: f32,
    bounds: (i32, i32, u32, u32),
    scale: f32,
    fb_width: u16,
    fb_height: u16,
) -> Option<ScrollBlitParams> {
    let dx_px = crate::round_f32(delta_tx * scale);
    let dy_px = crate::round_f32(delta_ty * scale);

    // Only vertical scroll for now.
    if dx_px != 0 || dy_px == 0 {
        return None;
    }

    // Scale point bounds to physical pixels.
    let cx = crate::round_f32(bounds.0 as f32 / 1024.0 * scale).max(0) as u32;
    let cy = crate::round_f32(bounds.1 as f32 / 1024.0 * scale).max(0) as u32;
    let cw = crate::scale_size(bounds.0, bounds.2 as i32, scale).max(0) as u32;
    let raw_ch = crate::scale_size(bounds.1, bounds.3 as i32, scale).max(0) as u32;
    // Clip container to framebuffer bounds.
    let fb_w = fb_width as u32;
    let fb_h = fb_height as u32;
    let cx = cx.min(fb_w);
    let cy = cy.min(fb_h);
    let cw = cw.min(fb_w.saturating_sub(cx));
    let ch = raw_ch.min(fb_h.saturating_sub(cy));

    if cw == 0 || ch == 0 {
        return None;
    }

    let abs_dy = (dy_px as i32).unsigned_abs();

    if abs_dy >= ch {
        return None;
    }

    // Compute exposed strip.
    let exposed = if dy_px < 0 {
        // Scroll down (content moves up): exposed at bottom.
        crate::DirtyRect::new(
            cx as u16,
            (cy + ch - abs_dy) as u16,
            cw as u16,
            abs_dy as u16,
        )
    } else {
        // Scroll up (content moves down): exposed at top.
        crate::DirtyRect::new(cx as u16, cy as u16, cw as u16, abs_dy as u16)
    };

    Some(ScrollBlitParams {
        cx,
        cy,
        cw,
        ch,
        dy_px,
        exposed,
        container_id,
    })
}

/// Shift scanlines vertically within a container region of the framebuffer.
///
/// `buf` is the raw BGRA framebuffer. `fb_stride` is the full framebuffer
/// width in pixels (not bytes). Container region: `(cx, cy, cw, ch)` in
/// pixels. `dy` is the pixel shift: negative = content moves up (scroll
/// down), positive = content moves down (scroll up).
///
/// Uses `copy` (memmove semantics) for overlapping source/destination
/// within each scanline band. Copy direction is chosen to avoid
/// overwriting source data before it's read.
pub fn blit_shift_vertical(
    buf: &mut [u8],
    cx: u32,
    cy: u32,
    cw: u32,
    ch: u32,
    fb_stride: u32,
    dy: i32,
) {
    if dy == 0 || cw == 0 || ch == 0 {
        return;
    }

    let abs_dy = dy.unsigned_abs();

    if abs_dy >= ch {
        return;
    }

    let bpp: u32 = 4; // BGRA
    let row_bytes = (cw * bpp) as usize;
    let stride_bytes = (fb_stride * bpp) as usize;

    if dy < 0 {
        // Content moves up (scroll down): copy from higher rows to lower.
        // Process top-to-bottom to avoid overwriting source.
        for row in 0..(ch - abs_dy) {
            let src_y = cy + row + abs_dy;
            let dst_y = cy + row;
            let src_off = (src_y as usize) * stride_bytes + (cx as usize) * (bpp as usize);
            let dst_off = (dst_y as usize) * stride_bytes + (cx as usize) * (bpp as usize);

            if src_off + row_bytes > buf.len() || dst_off + row_bytes > buf.len() {
                break;
            }

            // SAFETY: src and dst may overlap (same buffer). Use copy_within
            // for safe overlapping copy (memmove semantics).
            buf.copy_within(src_off..src_off + row_bytes, dst_off);
        }
    } else {
        // Content moves down (scroll up): copy from lower rows to higher.
        // Process bottom-to-top to avoid overwriting source.
        for i in 0..(ch - abs_dy) {
            let row = ch - abs_dy - 1 - i;
            let src_y = cy + row;
            let dst_y = cy + row + abs_dy;
            let src_off = (src_y as usize) * stride_bytes + (cx as usize) * (bpp as usize);
            let dst_off = (dst_y as usize) * stride_bytes + (cx as usize) * (bpp as usize);

            if src_off + row_bytes > buf.len() || dst_off + row_bytes > buf.len() {
                break;
            }

            buf.copy_within(src_off..src_off + row_bytes, dst_off);
        }
    }
}

/// Build scroll-adjusted damage: replace the scrolled container's full
/// dirty rect with just the exposed strip.
///
/// Iterates the original damage rects. Any rect that overlaps the blit-
/// shifted container region is excluded (the blit-shift already placed
/// those pixels correctly). The exposed strip is added. Non-overlapping
/// rects (e.g., cursor, other dirty nodes) are kept as-is.
///
/// Note: the overlap test uses the container's CLIPPED physical bounds
/// from `ScrollBlitParams`. A rect is considered "container-overlap" if
/// it is fully contained within the container bounds — partially
/// overlapping rects are kept (conservative: renders more, never less).
pub fn compute_scroll_damage(
    original: &DamageTracker,
    blit: &ScrollBlitParams,
    fb_width: u16,
    fb_height: u16,
) -> DamageTracker {
    let mut adjusted = DamageTracker::new(fb_width, fb_height);

    // Add the exposed strip.
    adjusted.add(
        blit.exposed.x,
        blit.exposed.y,
        blit.exposed.w,
        blit.exposed.h,
    );

    // Container bounds for overlap test.
    let c_x0 = blit.cx as u16;
    let c_y0 = blit.cy as u16;
    let c_x1 = c_x0.saturating_add(blit.cw as u16);
    let c_y1 = c_y0.saturating_add(blit.ch as u16);

    // Copy non-container rects from the original tracker.
    for i in 0..original.count {
        let r = &original.rects[i];

        if r.w == 0 || r.h == 0 {
            continue;
        }

        let r_x1 = r.x.saturating_add(r.w);
        let r_y1 = r.y.saturating_add(r.h);

        // Skip rects fully contained within the container (the blit-shift
        // already moved those pixels into place).
        if r.x >= c_x0 && r.y >= c_y0 && r_x1 <= c_x1 && r_y1 <= c_y1 {
            continue;
        }

        adjusted.add(r.x, r.y, r.w, r.h);
    }

    adjusted
}

// ── Geometry helpers ────────────────────────────────────────────────

/// Compute the union (bounding box) of two rectangles in (x, y, w, h) form.
fn union_bounds(
    ax: i32,
    ay: i32,
    aw: u32,
    ah: u32,
    bx: i32,
    by: i32,
    bw: u32,
    bh: u32,
) -> (i32, i32, u32, u32) {
    let min_x = ax.min(bx);
    let min_y = ay.min(by);
    let max_x = ax
        .saturating_add(aw.min(i32::MAX as u32) as i32)
        .max(bx.saturating_add(bw.min(i32::MAX as u32) as i32));
    let max_y = ay
        .saturating_add(ah.min(i32::MAX as u32) as i32)
        .max(by.saturating_add(bh.min(i32::MAX as u32) as i32));
    let w = (max_x - min_x).max(0) as u32;
    let h = (max_y - min_y).max(0) as u32;

    (min_x, min_y, w, h)
}

/// Add a dirty rect to the tracker, clamping from i32/u32 to u16 and
/// clipping to framebuffer bounds. Coordinates can be negative (off-screen).
fn add_rect_clamped(
    tracker: &mut DamageTracker,
    mpt_x: i32,
    mpt_y: i32,
    mpt_w: u32,
    mpt_h: u32,
    fb_width: u16,
    fb_height: u16,
) {
    // Convert millipoints to whole points (>> 10 = / 1024).
    // Origin floors (shifts dirty region left/up — conservative).
    // Size ceils (ensures dirty region covers the rightmost/bottom sub-point pixels).
    let x = mpt_x >> 10;
    let y = mpt_y >> 10;
    let w = (mpt_w + 1023) >> 10;
    let h = (mpt_h + 1023) >> 10;
    // Clip to framebuffer: clamp origin to 0, adjust size.
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w as i32).min(fb_width as i32).max(0);
    let y1 = (y + h as i32).min(fb_height as i32).max(0);
    let cw = (x1 - x0).max(0);
    let ch = (y1 - y0).max(0);

    if cw == 0 || ch == 0 {
        return;
    }

    // Clamp to u16 range (already within fb bounds, so safe).
    tracker.add(x0 as u16, y0 as u16, cw as u16, ch as u16);
}
