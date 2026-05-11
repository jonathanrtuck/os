//! Mutable scene graph builder operating on a flat byte buffer.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    node::{
        Node, NodeId, SceneHeader, DATA_BUFFER_SIZE, DATA_OFFSET, DIRTY_BITMAP_WORDS,
        GENERATION_OFFSET, MAX_NODES, NODES_OFFSET, NULL, SCENE_SIZE,
    },
    primitives::{Content, DataRef, ShapedGlyph},
};

const NODE_SIZE: usize = core::mem::size_of::<Node>();

// ── Sibling chain iterator ──────────────────────────────────────────

/// Iterator over the sibling chain starting from a given node.
/// Yields `NodeId`s until `NULL` is reached or `stop_before` is encountered.
/// Use via `SceneWriter::children_until()` or `SceneWriter::siblings()`.
pub struct ChildIter<'a> {
    buf: &'a [u8],
    current: NodeId,
    stop_before: NodeId,
}

impl<'a> Iterator for ChildIter<'a> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.current == NULL || self.current == self.stop_before {
            return None;
        }

        let id = self.current;
        // Read next_sibling from the node at `id`.
        // SAFETY: the buffer and node layout are the same as SceneWriter::node().
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        if offset + NODE_SIZE > self.buf.len() {
            self.current = NULL;

            return None; // Bounds safety
        }

        let node = unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) };

        self.current = node.next_sibling;

        Some(id)
    }
}

/// Builds and mutates a scene graph in a flat byte buffer conforming
/// to the shared memory layout (Header + Node array + Data buffer).
///
/// The writer operates on a `&mut [u8]` of at least `SCENE_SIZE` bytes.
/// In the process split, the OS service writes to shared memory via
/// this API; the compositor reads via `SceneReader`.
pub struct SceneWriter<'a> {
    buf: &'a mut [u8],
}

impl<'a> SceneWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= SCENE_SIZE);

        // SAFETY: buf is at least SCENE_SIZE bytes (asserted above).
        // SceneHeader is repr(C) at offset 0 with size <= SCENE_SIZE.
        // Exclusive &mut borrow prevents aliasing.
        let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut SceneHeader) };

        hdr.generation = 0;
        hdr.node_count = 0;
        hdr.root = NULL;
        hdr.data_used = 0;
        hdr.dirty_bits = [0u64; DIRTY_BITMAP_WORDS];

        Self { buf }
    }

    pub(crate) fn header(&self) -> &SceneHeader {
        // SAFETY: SceneHeader is repr(C) at offset 0 within the SCENE_SIZE
        // buffer. The shared borrow on `self` prevents concurrent mutation.
        unsafe { &*(self.buf.as_ptr() as *const SceneHeader) }
    }
    pub(crate) fn header_mut(&mut self) -> &mut SceneHeader {
        // SAFETY: SceneHeader is repr(C) at offset 0 within the SCENE_SIZE
        // buffer. The exclusive borrow on `self` prevents aliasing.
        unsafe { &mut *(self.buf.as_mut_ptr() as *mut SceneHeader) }
    }

    /// Link `child` as the last child of `parent`.
    pub fn add_child(&mut self, parent: NodeId, child: NodeId) {
        debug_assert!(parent != child, "add_child: self-parenting");

        let first = self.node(parent).first_child;

        if first == NULL {
            self.node_mut(parent).first_child = child;

            return;
        }

        // Walk to the last sibling.
        let mut cur = first;

        loop {
            let next = self.node(cur).next_sibling;

            if next == NULL {
                break;
            }

            cur = next;
        }

        self.node_mut(cur).next_sibling = child;
    }
    /// Allocate a new node slot. Returns `None` if the array is full.
    /// The node is initialized to `Node::EMPTY`.
    pub fn alloc_node(&mut self) -> Option<NodeId> {
        let count = self.header().node_count;

        if (count as usize) >= MAX_NODES {
            return None;
        }

        self.header_mut().node_count = count + 1;

        let id = count;
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: offset is within bounds (checked by MAX_NODES cap).
        unsafe {
            let ptr = self.buf.as_mut_ptr().add(offset) as *mut Node;

            core::ptr::write(ptr, Node::EMPTY);
        }

        Some(id)
    }
    /// Reset node count and data usage for a full rebuild.
    ///
    /// Sets an odd generation (mutation-in-progress signal) so that
    /// concurrent readers spin until `commit()` publishes the even
    /// generation. This is the write side of the seqlock protocol.
    pub fn clear(&mut self) {
        let gen = self.header().generation;
        let odd = gen.wrapping_add(1) | 1;

        self.store_generation_release(odd);

        self.header_mut().node_count = 0;
        self.header_mut().data_used = 0;
        self.header_mut().root = NULL;
        self.set_all_dirty();
    }
    /// Zero all dirty bits (no nodes marked dirty).
    pub fn clear_dirty(&mut self) {
        self.header_mut().dirty_bits = [0u64; DIRTY_BITMAP_WORDS];
    }
    /// Count the number of dirty bits set (popcount across all words).
    pub fn dirty_count(&self) -> u32 {
        let bits = &self.header().dirty_bits;
        let mut count = 0u32;
        let mut i = 0;

        while i < DIRTY_BITMAP_WORDS {
            count += bits[i].count_ones();
            i += 1;
        }

        count
    }
    /// Publish the scene: stores an even generation with release ordering.
    ///
    /// Pairs with `clear()` which sets the odd generation. Together they
    /// form a seqlock: readers spin on odd, read on even, verify unchanged.
    pub fn commit(&mut self) {
        let gen = self.header().generation;
        let even = gen.wrapping_add(1) & !1;

        self.store_generation_release(even);
    }
    /// Get the used portion of the data buffer as a read-only slice.
    pub fn data_buf(&self) -> &[u8] {
        let used = (self.data_used() as usize).min(DATA_BUFFER_SIZE);

        &self.buf[DATA_OFFSET..DATA_OFFSET + used]
    }
    pub fn data_used(&self) -> u32 {
        self.header().data_used
    }
    /// Wrap a previously initialized buffer without resetting state.
    pub fn from_existing(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= SCENE_SIZE);

        Self { buf }
    }
    pub fn generation(&self) -> u32 {
        self.header().generation
    }
    /// Test whether a node is marked dirty.
    pub fn is_dirty(&self, node_id: NodeId) -> bool {
        let idx = node_id as usize;

        if idx >= MAX_NODES {
            return false;
        }

        let word = idx / 64;
        let bit = idx % 64;

        (self.header().dirty_bits[word] & (1u64 << bit)) != 0
    }
    /// Mark a single node as dirty in the bitmap.
    pub fn mark_dirty(&mut self, node_id: NodeId) {
        let idx = node_id as usize;

        if idx >= MAX_NODES {
            return;
        }

        let word = idx / 64;
        let bit = idx % 64;

        self.header_mut().dirty_bits[word] |= 1u64 << bit;
    }
    /// Get a shared reference to a node by ID.
    pub fn node(&self, id: NodeId) -> &Node {
        assert!((id as usize) < MAX_NODES, "NodeId out of bounds");

        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: `id` is a NodeId returned by `alloc_node` (bounded by
        // MAX_NODES), so `offset` is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. Shared borrow prevents mutation.
        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    /// Truncate the node array to `count` nodes.
    ///
    /// Nodes with IDs >= `count` are logically freed. Any surviving node
    /// (ID < `count`) whose `first_child` points to a now-dead node gets
    /// its `first_child` cleared to NULL. This prevents the tree walker
    /// from following dangling pointers into reallocated memory.
    ///
    /// The caller must ensure `count` does not exceed the previously
    /// allocated high-water mark within the current buffer.
    pub fn set_node_count(&mut self, count: u16) {
        let old_count = self.header().node_count;

        self.header_mut().node_count = count;

        // Clean up dangling first_child pointers in surviving nodes.
        if count < old_count {
            for id in 0..count {
                let n = self.node_mut(id);

                if n.first_child >= count {
                    n.first_child = NULL;
                }
            }
        }
    }
    /// Get a mutable reference to a node by ID.
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        assert!((id as usize) < MAX_NODES, "NodeId out of bounds");

        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: Same bounds reasoning as `node()`. Exclusive borrow on
        // `self` prevents aliasing.
        unsafe { &mut *(self.buf.as_mut_ptr().add(offset) as *mut Node) }
    }
    /// Get all live nodes as a read-only slice.
    pub fn nodes(&self) -> &[Node] {
        let count = self.node_count() as usize;
        // SAFETY: NODES_OFFSET is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. `count` <= MAX_NODES.
        let ptr = unsafe { self.buf.as_ptr().add(NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        // Shared borrow on `self` prevents concurrent mutation.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    /// Append bytes to the data buffer. Returns a `DataRef`.
    /// If the buffer is full, truncates to fit.
    pub fn push_data(&mut self, bytes: &[u8]) -> DataRef {
        let used = self.header().data_used;
        let avail = DATA_BUFFER_SIZE.saturating_sub(used as usize);
        let actual = if bytes.len() < avail {
            bytes.len()
        } else {
            avail
        };

        if actual > 0 {
            let start = DATA_OFFSET + used as usize;

            self.buf[start..start + actual].copy_from_slice(&bytes[..actual]);
            self.header_mut().data_used = used + actual as u32;
        }

        DataRef {
            offset: used,
            length: actual as u32,
        }
    }
    /// Push serialized path commands into the data buffer.
    /// Ensures 4-byte alignment (f32 alignment) before writing.
    /// Returns a `DataRef` covering the path command data.
    pub fn push_path_commands(&mut self, commands: &[u8]) -> DataRef {
        // Align data_used to 4 bytes (f32 alignment).
        let align = 4usize;
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
            // Zero the padding gap so byte-level comparisons produce
            // consistent results regardless of alignment padding.
            self.buf[DATA_OFFSET + used..DATA_OFFSET + aligned].fill(0);
            self.header_mut().data_used = aligned as u32;
        }

        self.push_data(commands)
    }
    /// Push an array of `ShapedGlyph` structs into the data buffer.
    /// Aligns the write offset to `align_of::<ShapedGlyph>()` first.
    /// Returns a `DataRef` covering the glyph data.
    pub fn write_shaped_glyphs_at(&mut self, data_ref: DataRef, glyphs: &[ShapedGlyph]) {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                glyphs.as_ptr() as *const u8,
                glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
            )
        };
        let start = DATA_OFFSET + data_ref.offset as usize;
        let end = start + bytes.len().min(data_ref.length as usize);

        self.buf[start..end].copy_from_slice(&bytes[..end - start]);
    }

    pub fn push_shaped_glyphs(&mut self, glyphs: &[ShapedGlyph]) -> DataRef {
        // Align data_used to ShapedGlyph alignment (2 bytes for i16/u16).
        let align = core::mem::align_of::<ShapedGlyph>();
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
            // Zero the alignment gap so content hashes are deterministic.
            let gap = aligned - used;
            let base = DATA_OFFSET + used;

            // SAFETY: base..base+gap is within the data buffer (checked above).
            unsafe {
                core::ptr::write_bytes(self.buf.as_mut_ptr().add(base), 0, gap);
            }

            self.header_mut().data_used = aligned as u32;
        }

        // SAFETY: ShapedGlyph is #[repr(C)] with no padding, so
        // transmuting to bytes is safe for serialization.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                glyphs.as_ptr() as *const u8,
                glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
            )
        };

        self.push_data(bytes)
    }

    /// Append new data, returning a fresh DataRef. The old DataRef (if any)
    /// is abandoned — the bump allocator does not reclaim space until
    /// `reset_data()`. This is equivalent to `push_data` but named to
    /// clarify intent at call sites where a previous DataRef is being
    /// logically replaced.
    pub fn push_data_replacing(&mut self, bytes: &[u8]) -> DataRef {
        self.push_data(bytes)
    }
    /// Check if the data buffer has room for `bytes` more bytes.
    pub fn has_data_space(&self, bytes: usize) -> bool {
        (self.header().data_used as usize) + bytes <= DATA_BUFFER_SIZE
    }
    /// Reset the data buffer usage counter (bump allocator rewind).
    ///
    /// Also clears `Content` on surviving nodes whose content references
    /// the data buffer (`Content::Glyphs`, `Content::Path`). Setting them
    /// to `Content::None` forces callers to explicitly re-push all content
    /// after a reset. Missed re-pushes render as empty (visible error)
    /// instead of stale data (silent error). `clip_path` DataRefs are
    /// similarly cleared.
    pub fn reset_data(&mut self) {
        let count = self.header().node_count;

        for id in 0..count {
            let n = self.node_mut(id);

            match n.content {
                Content::Glyphs { .. } | Content::Path { .. } => {
                    n.content = Content::None;
                }
                _ => {}
            }

            if !n.clip_path.is_empty() {
                n.clip_path = DataRef::EMPTY;
            }
        }

        self.header_mut().data_used = 0;
    }
    pub fn root(&self) -> NodeId {
        self.header().root
    }
    /// Set all dirty bits to `u64::MAX` (every node slot marked dirty).
    /// Used after `clear()` to signal a full repaint.
    pub fn set_all_dirty(&mut self) {
        self.header_mut().dirty_bits = [u64::MAX; DIRTY_BITMAP_WORDS];
    }
    pub fn set_root(&mut self, id: NodeId) {
        self.header_mut().root = id;
    }
    /// Overwrite an existing DataRef in place (must be same length).
    /// Returns true on success, false if lengths don't match.
    pub fn update_data(&mut self, dref: DataRef, bytes: &[u8]) -> bool {
        if bytes.len() != dref.length as usize {
            return false;
        }

        let start = DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end > DATA_OFFSET + DATA_BUFFER_SIZE {
            return false;
        }

        self.buf[start..end].copy_from_slice(bytes);

        true
    }

    /// Iterate over the sibling chain starting from `start`, stopping before
    /// `stop_before` (or `NULL`). Does not include `stop_before` itself.
    ///
    /// Example: `for node_id in w.children_until(first_child, N_CURSOR) { ... }`
    pub fn children_until(&self, start: NodeId, stop_before: NodeId) -> ChildIter<'_> {
        ChildIter {
            buf: self.buf,
            current: start,
            stop_before,
        }
    }

    /// Iterate all siblings from `start` until `NULL`.
    pub fn siblings(&self, start: NodeId) -> ChildIter<'_> {
        self.children_until(start, NULL)
    }

    /// Hit-test the scene graph at point `(x, y)` in points.
    ///
    /// Walks the tree front-to-back (last child first, depth-first),
    /// inverting transforms at each level and respecting `CLIPS_CHILDREN`.
    /// Returns the `NodeId` of the frontmost focusable node
    /// (`STATE_FOCUSABLE`) whose bounds contain the point, or `None`.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        let root = self.root();

        if root == NULL {
            return None;
        }

        self.hit_test_node(root, x, y)
    }

    fn hit_test_node(&self, id: NodeId, x: f32, y: f32) -> Option<NodeId> {
        use crate::node::{MPT_PER_PT, STATE_FOCUSABLE};

        let n = self.node(id);

        if !n.visible() {
            return None;
        }

        let mpt = MPT_PER_PT as f32;
        let nx = n.x as f32 / mpt;
        let ny = n.y as f32 / mpt;
        let nw = n.width as f32 / mpt;
        let nh = n.height as f32 / mpt;
        // Transform the test point into this node's local coordinate space.
        let local_x = x - nx;
        let local_y = y - ny;
        let (lx, ly) = if n.transform.is_identity() {
            (local_x, local_y)
        } else if let Some(inv) = n.transform.inverse() {
            inv.transform_point(local_x, local_y)
        } else {
            return None;
        };

        // Clip check: if this node clips children, reject points outside bounds.
        if n.clips_children() && (lx < 0.0 || ly < 0.0 || lx >= nw || ly >= nh) {
            return None;
        }

        // Apply child_offset (scrolling) before testing children.
        let cx = lx - n.child_offset_x;
        let cy = ly - n.child_offset_y;
        // Collect children into a stack to iterate last-to-first (front-to-back).
        let mut children = [NULL; 64];
        let mut count = 0usize;
        let mut child = n.first_child;

        while child != NULL && count < children.len() {
            children[count] = child;
            count += 1;
            child = self.node(child).next_sibling;
        }

        // Test children in reverse order (last child = frontmost).
        for i in (0..count).rev() {
            if let Some(hit) = self.hit_test_node(children[i], cx, cy) {
                return Some(hit);
            }
        }

        // No child was hit — test this node itself.
        if n.state & STATE_FOCUSABLE != 0 && lx >= 0.0 && ly >= 0.0 && lx < nw && ly < nh {
            return Some(id);
        }

        None
    }

    fn store_generation_release(&mut self, gen: u32) {
        // SAFETY: GENERATION_OFFSET is within SceneHeader (offset 0, first
        // field). AtomicU32 has the same size/alignment as u32. Release
        // ordering ensures all prior writes are visible before the
        // generation update.
        unsafe {
            let ptr = self.buf.as_mut_ptr().add(GENERATION_OFFSET) as *const AtomicU32;

            (*ptr).store(gen, Ordering::Release);
        }
    }
}
