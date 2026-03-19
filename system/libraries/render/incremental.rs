//! Incremental rendering state: per-node tracking and dirty rect computation.
//!
//! `IncrementalState` persists across frames, storing each node's previous
//! bounds, visibility, scroll offset, and content hash. On each frame, the
//! dirty bitmap from the scene header is compared against previous state to
//! produce a minimal set of dirty rectangles for partial repaint.

use scene::{abs_bounds, build_parent_map, Node, NodeId, DIRTY_BITMAP_WORDS, MAX_NODES, NULL};

use crate::damage::DamageTracker;

// ── IncrementalState ────────────────────────────────────────────────

/// Per-node state persisted across frames for incremental rendering.
pub struct IncrementalState {
    /// Absolute visual bounds of each node from the previous frame.
    pub prev_bounds: [(i32, i32, u32, u32); MAX_NODES],
    /// Bitmap: was node visible last frame? One bit per node.
    pub prev_visible: [u64; DIRTY_BITMAP_WORDS],
    /// Per-node scroll_y from the previous frame.
    pub prev_scroll_y: [i32; MAX_NODES],
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
            prev_scroll_y: [0i32; MAX_NODES],
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

    /// Detect scroll_y changes on container nodes.
    ///
    /// Returns `Some((node_id, delta))` for the first dirty container
    /// with a changed scroll_y. `delta` is `current - previous`.
    /// Only reports the first scrolled container — sufficient for the
    /// current single-document model. Multi-container scroll would
    /// require returning an iterator or small array.
    pub fn detect_scroll(
        &self,
        nodes: &[Node],
        dirty_bits: &[u64; DIRTY_BITMAP_WORDS],
    ) -> Option<(NodeId, i32)> {
        for i in iter_set_bits(dirty_bits) {
            if i >= nodes.len() {
                break;
            }
            let node = &nodes[i];
            if node.first_child != NULL && node.scroll_y != self.prev_scroll_y[i] {
                let delta = node.scroll_y - self.prev_scroll_y[i];
                return Some((i as NodeId, delta));
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
            self.prev_scroll_y[i] = nodes[i].scroll_y;
            self.prev_content_hash[i] = nodes[i].content_hash;
        }

        // Clear state for nodes beyond the current count.
        for i in count..MAX_NODES {
            self.prev_bounds[i] = (0, 0, 0, 0);
            self.prev_scroll_y[i] = 0;
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
fn all_bits_zero(bits: &[u64; DIRTY_BITMAP_WORDS]) -> bool {
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
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    fb_width: u16,
    fb_height: u16,
) {
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
