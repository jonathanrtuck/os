//! Mutable scene graph builder operating on a flat byte buffer.

use crate::{
    node::{
        Node, NodeId, SceneHeader, CHANGE_LIST_CAPACITY, DATA_BUFFER_SIZE, DATA_OFFSET,
        FULL_REPAINT, MAX_NODES, NODES_OFFSET, NULL, SCENE_SIZE,
    },
    primitives::{DataRef, ShapedGlyph},
};

const NODE_SIZE: usize = core::mem::size_of::<Node>();

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
        hdr.change_count = 0;
        hdr.changed_nodes = [NULL; CHANGE_LIST_CAPACITY];

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
    /// Reset node count and data usage. Preserves generation.
    /// Sets change_count to FULL_REPAINT — a full rebuild means the
    /// compositor must repaint the entire screen.
    pub fn clear(&mut self) {
        self.header_mut().node_count = 0;
        self.header_mut().data_used = 0;
        self.header_mut().root = NULL;
        self.header_mut().change_count = FULL_REPAINT;
    }
    /// Increment the generation counter (signals a complete update).
    pub fn commit(&mut self) {
        let g = self.header().generation;

        self.header_mut().generation = g.wrapping_add(1);
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
    /// Record a node ID in the change list. If the list is already at
    /// capacity, sets the FULL_REPAINT sentinel instead. Duplicate IDs
    /// are stored as-is (the compositor treats them as a set).
    pub fn mark_changed(&mut self, node_id: NodeId) {
        let hdr = self.header_mut();

        if hdr.change_count == FULL_REPAINT {
            return; // already overflowed
        }

        let idx = hdr.change_count as usize;

        if idx >= CHANGE_LIST_CAPACITY {
            hdr.change_count = FULL_REPAINT;

            return;
        }

        hdr.changed_nodes[idx] = node_id;
        hdr.change_count = (idx + 1) as u16;
    }
    /// Get a shared reference to a node by ID.
    pub fn node(&self, id: NodeId) -> &Node {
        debug_assert!((id as usize) < MAX_NODES, "NodeId out of bounds");
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: `id` is a NodeId returned by `alloc_node` (bounded by
        // MAX_NODES), so `offset` is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. Shared borrow prevents mutation.
        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    /// Set the node count directly.
    ///
    /// Used to truncate the node array (e.g., removing dynamic selection
    /// rect nodes by resetting count to the well-known node count).
    /// The caller must ensure `count` does not exceed the previously
    /// allocated high-water mark within the current buffer.
    pub fn set_node_count(&mut self, count: u16) {
        self.header_mut().node_count = count;
    }
    /// Get a mutable reference to a node by ID.
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        debug_assert!((id as usize) < MAX_NODES, "NodeId out of bounds");
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
            // Zero the padding gap so byte-level comparisons (diff_scenes)
            // produce consistent results regardless of alignment padding.
            self.buf[DATA_OFFSET + used..DATA_OFFSET + aligned].fill(0);
            self.header_mut().data_used = aligned as u32;
        }

        self.push_data(commands)
    }
    /// Push an array of `ShapedGlyph` structs into the data buffer.
    /// Aligns the write offset to `align_of::<ShapedGlyph>()` first.
    /// Returns a `DataRef` covering the glyph data.
    pub fn push_shaped_glyphs(&mut self, glyphs: &[ShapedGlyph]) -> DataRef {
        // Align data_used to ShapedGlyph alignment (2 bytes for i16/u16).
        let align = core::mem::align_of::<ShapedGlyph>();
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
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
    /// Reset the data buffer usage counter (bump allocator rewind).
    pub fn reset_data(&mut self) {
        self.header_mut().data_used = 0;
    }
    pub fn root(&self) -> NodeId {
        self.header().root
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
}
