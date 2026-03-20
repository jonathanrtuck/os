//! Immutable scene graph reader operating on a flat byte buffer.

use crate::{
    node::{
        Node, NodeId, SceneHeader, DATA_BUFFER_SIZE, DATA_OFFSET, MAX_NODES, NODES_OFFSET,
        SCENE_SIZE,
    },
    primitives::{DataRef, ShapedGlyph},
};

const NODE_SIZE: usize = core::mem::size_of::<Node>();

/// Read-only view of a scene graph in a flat byte buffer.
///
/// The compositor uses this to walk the tree and render to pixels.
/// Operates on the same shared memory layout as `SceneWriter`.
pub struct SceneReader<'a> {
    buf: &'a [u8],
}

impl<'a> SceneReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        assert!(buf.len() >= SCENE_SIZE);

        Self { buf }
    }

    fn header(&self) -> &SceneHeader {
        // SAFETY: SceneHeader is repr(C) at offset 0 within the SCENE_SIZE
        // buffer (asserted in `new`). Shared borrow prevents mutation.
        unsafe { &*(self.buf.as_ptr() as *const SceneHeader) }
    }

    /// Resolve a `DataRef` to a byte slice.
    /// Returns an empty slice if the reference is out of bounds.
    pub fn data(&self, dref: DataRef) -> &[u8] {
        let start = DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end <= self.buf.len() && dref.offset + dref.length <= self.header().data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }
    /// Get the used portion of the data buffer as a slice.
    pub fn data_buf(&self) -> &[u8] {
        let used = (self.data_used() as usize).min(DATA_BUFFER_SIZE);

        &self.buf[DATA_OFFSET..DATA_OFFSET + used]
    }
    pub fn data_used(&self) -> u32 {
        self.header().data_used
    }
    pub fn generation(&self) -> u32 {
        self.header().generation
    }
    /// Get a reference to a node by ID.
    pub fn node(&self, id: NodeId) -> &Node {
        debug_assert!((id as usize) < MAX_NODES, "NodeId out of bounds in reader");
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: `id` is a valid NodeId (bounded by node_count <= MAX_NODES),
        // so `offset` is within the SCENE_SIZE buffer. Node is repr(C).
        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    /// Get all live nodes as a slice.
    pub fn nodes(&self) -> &[Node] {
        let count = self.node_count() as usize;
        // SAFETY: NODES_OFFSET is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. `count` <= MAX_NODES.
        let ptr = unsafe { self.buf.as_ptr().add(NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        // Shared borrow on `self` prevents concurrent mutation.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    pub fn root(&self) -> NodeId {
        self.header().root
    }
    /// Interpret a DataRef as an array of `ShapedGlyph` structs.
    ///
    /// `glyph_count` is the number of glyphs expected (from `Content::Glyphs`).
    /// Returns a slice of up to `glyph_count` glyphs, or fewer if the data
    /// buffer doesn't contain enough bytes.
    pub fn shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();

        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }

        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);

        // SAFETY: ShapedGlyph is #[repr(C)], data buffer is aligned by
        // push_shaped_glyphs to ShapedGlyph alignment.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }
}
