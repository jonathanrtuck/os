//! Triple-buffered scene graph with lock-free mailbox semantics.
//!
//! The writer always has a free buffer (never blocks), the reader always
//! gets the most recent published buffer (intermediate frames are silently
//! skipped). Three buffers: one for reader, one "latest" (published), one
//! free for writer.

use crate::{
    node::{
        Node, NodeId, SceneHeader, DATA_BUFFER_SIZE, DATA_OFFSET, DIRTY_BITMAP_WORDS, MAX_NODES,
        NODES_OFFSET, SCENE_SIZE,
    },
    primitives::{DataRef, ShapedGlyph},
    writer::SceneWriter,
};

// ── Control region ──────────────────────────────────────────────────

/// Control region layout at the end of the triple-buffer shared memory.
///
/// ```text
/// Offset 0: latest_buf   (u32) — index (0,1,2) of the most recently published buffer
/// Offset 4: reader_buf   (u32) — index of buffer reader is using (0xFF = none)
/// Offset 8: generation    (u32) — global generation, incremented on each publish()
/// Offset 12: reader_done_gen (u32) — generation reader last finished reading
/// ```
const TRIPLE_CONTROL_SIZE: usize = 16;

/// Total size for a triple-buffered scene graph: three full scene buffers
/// plus a control region for lock-free coordination.
///
/// Mailbox semantics: the writer always has a free buffer (never blocks),
/// the reader always gets the most recent published buffer (intermediate
/// frames are silently skipped). Three buffers: one for reader, one
/// "latest" (published), one free for writer.
pub const TRIPLE_SCENE_SIZE: usize = 3 * SCENE_SIZE + TRIPLE_CONTROL_SIZE;

/// Compile-time assertion: TRIPLE_SCENE_SIZE is exactly what we expect.
const _: () = assert!(TRIPLE_SCENE_SIZE == 3 * SCENE_SIZE + 16);

/// Byte offset of the control region within the triple-buffer layout.
const TRIPLE_CONTROL_OFFSET: usize = 3 * SCENE_SIZE;

/// Sentinel value for `reader_buf` meaning no reader is active.
const NO_READER: u32 = 0xFF;

// ── Control region helpers ──────────────────────────────────────────

/// Read a u32 field from the control region using atomic load.
///
/// Takes a raw `*mut u8` instead of `&[u8]` because the underlying memory
/// is cross-process shared memory where both sides may read/write
/// concurrently. Deriving `*mut` from a shared `&[u8]` reference would be
/// aliasing UB; raw pointers preserve the necessary write provenance.
fn triple_read_ctrl(buf: *mut u8, field_offset: usize) -> u32 {
    // SAFETY: Caller guarantees `buf` points to a TRIPLE_SCENE_SIZE region.
    // TRIPLE_CONTROL_OFFSET + field_offset is within that region. The field
    // is a u32 at a 4-byte aligned offset. AtomicU32 is the correct model
    // for cross-process shared memory.
    unsafe {
        let ptr = buf.add(TRIPLE_CONTROL_OFFSET + field_offset) as *mut u32;

        core::sync::atomic::AtomicU32::from_ptr(ptr).load(core::sync::atomic::Ordering::Relaxed)
    }
}

/// Read a u32 field from the control region with acquire ordering.
/// Pairs with the writer's release store in `publish()`.
fn triple_read_ctrl_acquire(buf: *mut u8, field_offset: usize) -> u32 {
    // SAFETY: Same alignment and bounds reasoning as triple_read_ctrl.
    unsafe {
        let ptr = buf.add(TRIPLE_CONTROL_OFFSET + field_offset) as *mut u32;

        core::sync::atomic::AtomicU32::from_ptr(ptr).load(core::sync::atomic::Ordering::Acquire)
    }
}

/// Write a u32 field to the control region using atomic store.
///
/// Takes `*mut u8` — write provenance flows from the raw pointer, not
/// from a shared reference. This is sound for cross-process shared memory.
fn triple_write_ctrl(buf: *mut u8, field_offset: usize, value: u32) {
    // SAFETY: Caller guarantees `buf` points to a TRIPLE_SCENE_SIZE region.
    // Same alignment and bounds reasoning as triple_read_ctrl.
    unsafe {
        let ptr = buf.add(TRIPLE_CONTROL_OFFSET + field_offset) as *mut u32;

        core::sync::atomic::AtomicU32::from_ptr(ptr)
            .store(value, core::sync::atomic::Ordering::Relaxed)
    }
}

/// Write a u32 field to the control region with release ordering.
/// Ensures all prior writes are visible before this store is observed.
fn triple_write_ctrl_release(buf: *mut u8, field_offset: usize, value: u32) {
    // SAFETY: Same as triple_write_ctrl. Release ordering ensures all
    // prior writes (scene data, node fields) are visible before this
    // store is observed by an Acquire load in the reader.
    unsafe {
        let ptr = buf.add(TRIPLE_CONTROL_OFFSET + field_offset) as *mut u32;

        core::sync::atomic::AtomicU32::from_ptr(ptr)
            .store(value, core::sync::atomic::Ordering::Release)
    }
}

// Control region field offsets.
const CTRL_LATEST_BUF: usize = 0;
const CTRL_READER_BUF: usize = 4;
const CTRL_GENERATION: usize = 8;
const CTRL_READER_DONE_GEN: usize = 12;

// ── TripleWriter ────────────────────────────────────────────────────

/// Mutable access to a triple-buffered scene graph (mailbox semantics).
///
/// The OS service uses this to write scenes and publish them. `acquire()`
/// always returns a free buffer — never blocks, never fails. `publish()`
/// atomically makes the acquired buffer the latest. Intermediate frames
/// are silently skipped (only the latest matters for interactive UI).
pub struct TripleWriter<'a> {
    buf: &'a mut [u8],
    /// Index (0, 1, or 2) of the buffer currently acquired by the writer.
    /// Set by `acquire()`, consumed by `publish()`.
    acquired: u32,
}

/// Find the byte offset of buffer `idx` (0, 1, or 2).
#[inline]
fn buf_offset(idx: u32) -> usize {
    (idx as usize) * SCENE_SIZE
}

/// Find the free buffer index: the one that is neither `a` nor `b`.
/// Precondition: a != b, both in {0, 1, 2}.
#[inline]
fn free_index(a: u32, b: u32) -> u32 {
    debug_assert!(a != b, "free_index: a == b == {}", a);
    debug_assert!(a < 3 && b < 3, "free_index: out of range a={} b={}", a, b);

    // 0 + 1 + 2 = 3. The free one is 3 - a - b.
    3 - a - b
}

impl<'a> TripleWriter<'a> {
    /// Initialize a new triple-buffered scene graph. All three buffers
    /// are initialized to empty scenes. Buffer 0 starts as the "latest"
    /// (published) buffer with generation 0.
    pub fn new(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= TRIPLE_SCENE_SIZE);

        // Initialize all three scene buffer headers.
        {
            let (b0, rest) = buf.split_at_mut(SCENE_SIZE);
            let _ = SceneWriter::new(b0);
            let (b1, rest2) = rest.split_at_mut(SCENE_SIZE);
            let _ = SceneWriter::new(b1);
            let _ = SceneWriter::new(&mut rest2[..SCENE_SIZE]);
        }

        // Initialize control region.
        // SAFETY: Control region is within the TRIPLE_SCENE_SIZE buffer.
        unsafe {
            let ctrl = buf.as_mut_ptr().add(TRIPLE_CONTROL_OFFSET);

            // latest_buf = 0 (buffer 0 is the initial "latest")
            core::ptr::write_volatile(ctrl as *mut u32, 0);
            // reader_buf = NO_READER (no reader connected)
            core::ptr::write_volatile(ctrl.add(4) as *mut u32, NO_READER);
            // generation = 0
            core::ptr::write_volatile(ctrl.add(8) as *mut u32, 0);
            // reader_done_gen = 0
            core::ptr::write_volatile(ctrl.add(12) as *mut u32, 0);
        }

        Self { buf, acquired: 1 } // Writer starts with buffer 1 as acquired (free)
    }

    /// Wrap a previously initialized triple buffer without resetting.
    pub fn from_existing(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= TRIPLE_SCENE_SIZE);

        // Determine a free buffer to use as initial acquired slot.
        let latest = triple_read_ctrl(buf.as_mut_ptr(), CTRL_LATEST_BUF);
        // Acquire: pairs with reader's Release store of reader_buf.
        let reader = triple_read_ctrl_acquire(buf.as_mut_ptr(), CTRL_READER_BUF);
        let free = if reader == NO_READER || reader > 2 || reader == latest {
            // Pick any buffer that isn't latest.
            if latest == 0 {
                1
            } else {
                0
            }
        } else {
            free_index(latest, reader)
        };

        Self {
            buf,
            acquired: free,
        }
    }

    /// Acquire a free buffer for writing. Always succeeds — the writer
    /// always has a buffer that neither the reader nor the "latest" slot
    /// is using. Returns a `SceneWriter` for the acquired buffer.
    ///
    /// The returned `SceneWriter` operates on a clean buffer from the
    /// caller's perspective — the caller should call `clear()` to reset
    /// it, or use `from_existing` semantics to continue from previous
    /// state (the buffer may contain stale data from a previous frame).
    ///
    /// For incremental updates, call `copy_latest_to_acquired()` first,
    /// then `acquire()` to get a writable view of the copied buffer.
    pub fn acquire(&mut self) -> SceneWriter<'_> {
        self.select_free_buffer();

        let off = buf_offset(self.acquired);

        SceneWriter::from_existing(&mut self.buf[off..off + SCENE_SIZE])
    }

    /// Select a free buffer for writing without returning a SceneWriter.
    /// Use this before `copy_latest_to_acquired()` when you need to
    /// copy first and then get a writer via a second `acquire()` call.
    fn select_free_buffer(&mut self) {
        let latest = triple_read_ctrl(self.buf.as_mut_ptr(), CTRL_LATEST_BUF);
        // Acquire: pairs with reader's Release store of reader_buf.
        let reader = triple_read_ctrl_acquire(self.buf.as_mut_ptr(), CTRL_READER_BUF);

        // Find a free buffer (not latest, not reader).
        // When reader == latest or reader is inactive (NO_READER),
        // there are two free buffers — pick the first one that isn't latest.
        self.acquired = if reader == NO_READER || reader > 2 || reader == latest {
            // Pick any buffer that isn't latest.
            if latest == 0 {
                1
            } else if latest == 1 {
                2
            } else {
                0
            }
        } else {
            // Reader and latest are different — exactly one buffer is free.
            free_index(latest, reader)
        };
    }

    /// Publish the acquired buffer as the new "latest". Atomically swaps
    /// the latest pointer. The old latest becomes the new free buffer.
    /// Increments the global generation counter.
    ///
    /// A release fence ensures all scene data written via the `SceneWriter`
    /// returned by `acquire()` is visible before the latest pointer update.
    pub fn publish(&mut self) {
        let ptr = self.buf.as_mut_ptr();
        // Increment global generation.
        let generation = triple_read_ctrl(ptr, CTRL_GENERATION).wrapping_add(1);

        // Write generation into the acquired buffer's header.
        write_generation(ptr, buf_offset(self.acquired), generation);
        // Update control region: generation first, then publish latest_buf
        // with a release fence so all scene data + generation are visible
        // before the reader sees the new latest_buf pointer.
        triple_write_ctrl(ptr, CTRL_GENERATION, generation);
        triple_write_ctrl_release(ptr, CTRL_LATEST_BUF, self.acquired);
    }

    /// Read the current global generation counter.
    pub fn generation(&self) -> u32 {
        triple_read_ctrl(self.buf.as_ptr() as *mut u8, CTRL_GENERATION)
    }

    /// Generation the reader last finished reading. Entries removed from
    /// the scene at generation N are safe to free once `reader_done_gen() >= N`.
    pub fn reader_done_gen(&self) -> u32 {
        triple_read_ctrl(self.buf.as_ptr() as *mut u8, CTRL_READER_DONE_GEN)
    }

    /// Get a read-only view of the latest published buffer's nodes.
    pub fn latest_nodes(&self) -> &[Node] {
        let latest = triple_read_ctrl(self.buf.as_ptr() as *mut u8, CTRL_LATEST_BUF);
        let off = buf_offset(latest);
        // SAFETY: `off` is a valid scene buffer offset. SceneHeader is repr(C)
        // at the buffer start.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let count = (hdr.node_count as usize).min(MAX_NODES);
        // SAFETY: NODES_OFFSET is within each SCENE_SIZE buffer. Node is repr(C).
        let ptr = unsafe { self.buf.as_ptr().add(off + NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }

    /// Generation counter of the latest published buffer.
    pub fn latest_generation(&self) -> u32 {
        let ptr = self.buf.as_ptr() as *mut u8;
        let latest = triple_read_ctrl(ptr, CTRL_LATEST_BUF);

        read_generation(ptr as *const u8, buf_offset(latest))
    }

    /// Data buffer slice from the latest published buffer.
    pub fn latest_data_buf(&self) -> &[u8] {
        let latest = triple_read_ctrl(self.buf.as_ptr() as *mut u8, CTRL_LATEST_BUF);
        let off = buf_offset(latest);
        // SAFETY: Same as latest_nodes.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let used = (hdr.data_used as usize).min(DATA_BUFFER_SIZE);

        &self.buf[off + DATA_OFFSET..off + DATA_OFFSET + used]
    }

    /// Resolve a DataRef against the latest published buffer.
    pub fn latest_data(&self, dref: DataRef) -> &[u8] {
        let latest = triple_read_ctrl(self.buf.as_ptr() as *mut u8, CTRL_LATEST_BUF);
        let off = buf_offset(latest);
        // SAFETY: Same as latest_nodes.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let start = off + DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end <= self.buf.len() && dref.offset.saturating_add(dref.length) <= hdr.data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }

    /// Interpret a DataRef from the latest buffer as ShapedGlyph array.
    pub fn latest_shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.latest_data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();

        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }

        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);

        // SAFETY: ShapedGlyph is #[repr(C)] with no padding.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }

    /// Acquire a free buffer and copy the latest published buffer into it.
    /// Returns a `SceneWriter` for the acquired buffer, pre-populated with
    /// the latest scene state. The caller can then mutate specific nodes
    /// and call `publish()`.
    ///
    /// This is the triple-buffer equivalent of the old `copy_front_to_back()`
    /// + `back()` pattern, but it always succeeds — the acquired buffer is
    /// always free (not held by the reader).
    pub fn acquire_copy(&mut self) -> SceneWriter<'_> {
        self.select_free_buffer();
        self.copy_latest_to_acquired_inner();

        let off = buf_offset(self.acquired);

        SceneWriter::from_existing(&mut self.buf[off..off + SCENE_SIZE])
    }

    /// Copy the latest published buffer into the acquired buffer.
    /// This enables the copy-forward pattern: copy the previous frame,
    /// mutate specific nodes, then publish. Unlike the old double-buffer
    /// `copy_front_to_back()`, this always succeeds — the acquired buffer
    /// is always free (not held by the reader).
    ///
    /// The acquired buffer's dirty bits are reset to zero. The generation
    /// is NOT copied — it will be set by the next `publish()` call.
    ///
    /// Must be called after `select_free_buffer()` / `acquire()` and
    /// before `publish()`.
    fn copy_latest_to_acquired_inner(&mut self) {
        let latest = triple_read_ctrl(self.buf.as_mut_ptr(), CTRL_LATEST_BUF);
        let src_off = buf_offset(latest);
        let dst_off = buf_offset(self.acquired);
        // Read source header to determine how much to copy.
        // SAFETY: src_off is a valid scene buffer offset (0, SCENE_SIZE,
        // or 2*SCENE_SIZE). SceneHeader is repr(C) at the start.
        let src_hdr =
            unsafe { core::ptr::read(self.buf.as_ptr().add(src_off) as *const SceneHeader) };
        let node_count = src_hdr.node_count;
        let data_used = src_hdr.data_used;
        // Copy node array (only live nodes).
        let node_bytes = node_count as usize * core::mem::size_of::<Node>();

        if node_bytes > 0 {
            // SAFETY: src and dst are valid scene buffer offsets that don't
            // overlap (acquired != latest). NODES_OFFSET + node_bytes is
            // within SCENE_SIZE (bounded by MAX_NODES * NODE_SIZE).
            unsafe {
                let src = self.buf.as_ptr().add(src_off + NODES_OFFSET);
                let dst = self.buf.as_mut_ptr().add(dst_off + NODES_OFFSET);

                core::ptr::copy_nonoverlapping(src, dst, node_bytes);
            }
        }

        // Copy data buffer (only used portion).
        let data_bytes = data_used as usize;

        if data_bytes > 0 {
            // SAFETY: Same reasoning — DATA_OFFSET + data_bytes is within
            // SCENE_SIZE. src and dst don't overlap.
            unsafe {
                let src = self.buf.as_ptr().add(src_off + DATA_OFFSET);
                let dst = self.buf.as_mut_ptr().add(dst_off + DATA_OFFSET);

                core::ptr::copy_nonoverlapping(src, dst, data_bytes);
            }
        }

        // Write destination header: copy source metadata, reset dirty bits.
        // SAFETY: dst_off is a valid scene buffer offset. SceneHeader is
        // repr(C) at offset 0. Exclusive &mut borrow prevents aliasing.
        let dst_hdr = unsafe { &mut *(self.buf.as_mut_ptr().add(dst_off) as *mut SceneHeader) };

        dst_hdr.node_count = node_count;
        dst_hdr.root = src_hdr.root;
        dst_hdr.data_used = data_used;
        dst_hdr.dirty_bits = [0u64; DIRTY_BITMAP_WORDS];
    }

    /// Get the index of the buffer currently acquired by the writer.
    pub fn acquired_index(&self) -> u32 {
        self.acquired
    }
}

// ── TripleReader ────────────────────────────────────────────────────

/// Read-only access to a triple-buffered scene graph (mailbox semantics).
///
/// The compositor uses this when reading from shared memory written by
/// the OS service. Construction atomically claims the latest published
/// buffer. All reads within a single `TripleReader` instance reference
/// the same physical buffer.
///
/// Stores a raw `*mut u8` instead of `&[u8]` because the reader must
/// write to the control region (claiming/releasing buffers) while the
/// writer process may concurrently write to other parts of the same
/// shared memory. Using `&[u8]` and deriving `*mut` from it would be
/// aliasing UB under Rust's rules.
pub struct TripleReader {
    /// Raw pointer to the start of the TRIPLE_SCENE_SIZE shared memory
    /// region. Must remain valid for the lifetime of this reader.
    // SAFETY: This is cross-process shared memory mapped by the kernel.
    // The pointer has write provenance because the underlying memory is
    // mutable (mapped read-write by both processes). Atomic operations
    // on the control region are the only writes performed through this
    // pointer by the reader; all other accesses are reads.
    buf: *mut u8,
    /// Total length of the buffer (>= TRIPLE_SCENE_SIZE).
    len: usize,
    /// Byte offset of the buffer being read (0, SCENE_SIZE, or 2*SCENE_SIZE).
    read_off: usize,
    /// Generation of the buffer being read.
    read_gen: u32,
}

// SAFETY: TripleReader's raw pointer points to cross-process shared memory
// that outlives any single thread. The atomic access discipline (acquire/
// release on control fields) ensures correct visibility across cores.
unsafe impl Send for TripleReader {}

impl TripleReader {
    /// Claim the latest published buffer for reading. The reader atomically
    /// takes ownership of the latest buffer — the writer will not write to
    /// it. All reads within this `TripleReader` reference the same buffer.
    ///
    /// # Safety
    ///
    /// `buf` must point to a region of at least `TRIPLE_SCENE_SIZE` bytes
    /// that remains valid and mapped for the lifetime of the returned
    /// `TripleReader`. The pointer must have been derived from a mutable
    /// source (e.g. `&mut [u8]` or a raw mapping) so that writes to the
    /// control region through atomic operations are well-defined.
    pub unsafe fn new(buf: *mut u8, len: usize) -> Self {
        assert!(len >= TRIPLE_SCENE_SIZE);

        // Validated claim loop. The load of latest_buf and store of
        // reader_buf are not a single atomic operation. A concurrent
        // publish() between them changes latest — the old latest
        // becomes available and the writer's next acquire() could pick
        // it before seeing our claim, creating a data race.
        //
        // Fix: after claiming, Acquire-reload latest_buf. If it changed,
        // a publish happened during our window — retry. latest_buf uses
        // a Release/Acquire pair with publish(), so the Acquire reload
        // is properly synchronized. Three ABA cycles (latest_buf has
        // only 3 values) in the ~100 ns between two LDAR instructions
        // is physically impossible.
        loop {
            // Acquire: pairs with writer's Release in publish().
            // Ensures all scene data is visible before we read it.
            let latest = triple_read_ctrl_acquire(buf, CTRL_LATEST_BUF);

            // Claim with Release so writer's Acquire of reader_buf
            // sees it.
            triple_write_ctrl_release(buf, CTRL_READER_BUF, latest);

            // Acquire-reload: if latest_buf is unchanged, no publish
            // happened during our claim — the writer hasn't acquired
            // a new buffer, so our claimed buffer is safe.
            let latest2 = triple_read_ctrl_acquire(buf, CTRL_LATEST_BUF);

            if latest == latest2 {
                let read_off = buf_offset(latest);
                let read_gen = read_generation(buf as *const u8, read_off);

                return Self {
                    buf,
                    len,
                    read_off,
                    read_gen,
                };
            }
            // A publish happened — retry with the new latest.
        }
    }

    fn header(&self) -> &SceneHeader {
        // SAFETY: read_off is a valid scene buffer offset. SceneHeader is
        // repr(C) at the start of each scene buffer. The pointer is valid
        // for the lifetime of `self` (caller contract on `new`).
        unsafe { &*((self.buf as *const u8).add(self.read_off) as *const SceneHeader) }
    }

    /// Resolve a `DataRef` against the claimed buffer.
    pub fn front_data(&self, dref: DataRef) -> &[u8] {
        let off = self.read_off;
        let hdr = self.header();
        let start = off + DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end <= self.len && dref.offset.saturating_add(dref.length) <= hdr.data_used {
            // SAFETY: start..end is within the valid buffer region.
            unsafe { core::slice::from_raw_parts((self.buf as *const u8).add(start), end - start) }
        } else {
            &[]
        }
    }

    /// Data buffer slice from the claimed buffer.
    pub fn front_data_buf(&self) -> &[u8] {
        let off = self.read_off;
        let hdr = self.header();
        let used = (hdr.data_used as usize).min(DATA_BUFFER_SIZE);

        // SAFETY: off + DATA_OFFSET + used is within the valid buffer region.
        unsafe { core::slice::from_raw_parts((self.buf as *const u8).add(off + DATA_OFFSET), used) }
    }

    /// Generation of the buffer being read.
    pub fn front_generation(&self) -> u32 {
        self.read_gen
    }

    /// Root node ID from the claimed buffer.
    pub fn front_root(&self) -> NodeId {
        self.header().root
    }

    /// Node slice from the claimed buffer.
    pub fn front_nodes(&self) -> &[Node] {
        let off = self.read_off;
        let hdr = self.header();
        let count = (hdr.node_count as usize).min(MAX_NODES);
        // SAFETY: NODES_OFFSET is within each SCENE_SIZE buffer. Node is repr(C).
        let ptr = unsafe { (self.buf as *const u8).add(off + NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }

    /// Interpret a DataRef from the claimed buffer as ShapedGlyph array.
    pub fn front_shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.front_data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();

        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }

        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);

        // SAFETY: ShapedGlyph is #[repr(C)] with no padding.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }

    /// Returns the dirty bitmap from the claimed buffer.
    /// Each bit corresponds to a node slot: bit `i` is set if node `i`
    /// was modified since the last frame.
    pub fn dirty_bits(&self) -> &[u64; DIRTY_BITMAP_WORDS] {
        &self.header().dirty_bits
    }

    /// Signal that the reader has finished reading. Releases the buffer
    /// back to the free pool so the writer can acquire it.
    ///
    /// Note: `Drop` handles cleanup automatically if this is not called.
    pub fn finish_read(&self, generation: u32) {
        triple_write_ctrl_release(self.buf, CTRL_READER_DONE_GEN, generation);
        // Release: pairs with writer's Acquire in select_free_buffer.
        triple_write_ctrl_release(self.buf, CTRL_READER_BUF, NO_READER);
    }
}

impl Drop for TripleReader {
    fn drop(&mut self) {
        triple_write_ctrl_release(self.buf, CTRL_READER_DONE_GEN, self.read_gen);
        // Release: pairs with writer's Acquire in select_free_buffer.
        triple_write_ctrl_release(self.buf, CTRL_READER_BUF, NO_READER);
    }
}

// ── Generation helpers ──────────────────────────────────────────────

/// Read the generation counter from a scene buffer at the given byte
/// offset within the parent buffer. Uses AtomicU32 for correct
/// cross-process shared memory semantics.
///
/// Takes `*const u8` — reads only, no write provenance needed.
fn read_generation(buf: *const u8, offset: usize) -> u32 {
    // SAFETY: Caller guarantees `buf + offset` points to a valid SceneHeader.
    // Generation is the first u32, 4-byte aligned. AtomicU32 is the correct
    // model for cross-process shared memory.
    unsafe {
        let ptr = buf.add(offset) as *mut u32;

        core::sync::atomic::AtomicU32::from_ptr(ptr).load(core::sync::atomic::Ordering::Acquire)
    }
}

/// Write a generation counter to a scene buffer at the given offset.
/// Release ordering ensures all prior writes (node data, text content)
/// are visible before the generation update is published.
///
/// Takes `*mut u8` — write provenance flows from the raw pointer.
fn write_generation(buf: *mut u8, offset: usize, value: u32) {
    // SAFETY: Caller guarantees `buf + offset` points to a valid SceneHeader.
    // Generation is the first u32, 4-byte aligned.
    unsafe {
        let ptr = buf.add(offset) as *mut u32;

        core::sync::atomic::AtomicU32::from_ptr(ptr)
            .store(value, core::sync::atomic::Ordering::Release)
    }
}
