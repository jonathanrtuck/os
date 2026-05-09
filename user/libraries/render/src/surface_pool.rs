//! Pool-based offscreen buffer management for the compositor.
//!
//! `SurfacePool` allocates, caches, and reuses temporary `Vec<u8>` buffers
//! for offscreen rendering (group opacity, rounded-corner clipping, blur).
//!
//! # Design
//!
//! Buffers are matched by size (width × height). When a buffer of a
//! given size is requested:
//!
//! 1. If a free buffer of that size exists in the pool, it is returned
//!    (cleared to transparent first).
//! 2. Otherwise, a new buffer is allocated — provided the pool's total
//!    memory usage stays within the configured budget (default 32 MiB).
//!
//! After use, buffers are returned to the pool via `release()`. The pool
//! holds them for reuse on subsequent frames. At frame boundaries, the
//! caller should call `end_frame()` to reclaim buffers that were not
//! requested during the frame (shrinking the pool when the scene becomes
//! simpler).
//!
//! # Memory budget
//!
//! The pool enforces a hard cap on total allocated bytes. When the cap
//! is reached, `acquire()` returns `None` rather than allocating. This
//! prevents a scene with many blurred layers from exhausting the
//! compositor's 32 MiB heap.

use alloc::vec::Vec;

/// Bytes per pixel (BGRA8888).
const BPP: u32 = 4;

/// Default memory budget: 32 MiB (compositor heap limit).
/// Three full-screen buffers at 1024×768×4 = ~9.4 MiB, well within budget.
pub const DEFAULT_BUDGET: usize = 32 * 1024 * 1024;

/// Maximum number of pooled buffers. Prevents unbounded growth of the
/// pool's metadata even when many different sizes are requested.
const MAX_ENTRIES: usize = 32;

/// A pooled buffer entry.
struct PoolEntry {
    /// Width in pixels.
    width: u32,
    /// Height in pixels.
    height: u32,
    /// The pixel data (width × height × 4 bytes, BGRA8888).
    data: Vec<u8>,
    /// Whether this buffer is currently lent out (in use).
    in_use: bool,
    /// Whether this buffer was requested during the current frame.
    /// Used by `end_frame()` to identify stale entries.
    used_this_frame: bool,
}

/// Pool-based offscreen surface allocator.
///
/// Call `acquire(w, h)` to get a buffer, `release(handle)` to return it.
/// At the end of each frame, call `end_frame()` to free unused entries.
pub struct SurfacePool {
    entries: Vec<PoolEntry>,
    /// Hard cap on total allocated bytes across all entries.
    budget: usize,
    /// Running total of allocated bytes (sum of all entry data lengths).
    total_bytes: usize,
    /// Cumulative count of new allocations (for testing).
    alloc_count: usize,
}

/// Opaque handle returned by `acquire()`. Pass to `release()` when done.
///
/// Handles are indices into the internal entries Vec. Entries are cleared
/// in-place (not removed) by `end_frame()`, so indices stay valid. A stale
/// handle from a prior frame may index a cleared or reused slot — the caller
/// must not retain handles across frames. A generation counter would catch
/// this, but the current single-frame usage pattern makes it unnecessary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolHandle(usize);

impl SurfacePool {
    /// Create a new pool with the given memory budget (bytes).
    pub fn new(budget: usize) -> Self {
        Self {
            entries: Vec::new(),
            budget,
            total_bytes: 0,
            alloc_count: 0,
        }
    }

    /// Acquire an offscreen buffer of the given dimensions.
    ///
    /// Returns `Some((handle, &mut data))` where `data` is a zeroed
    /// BGRA8888 pixel buffer of `width × height × 4` bytes. The caller
    /// must call `release(handle)` when done.
    ///
    /// Returns `None` if:
    /// - The allocation would exceed the memory budget.
    /// - The pool has reached its maximum entry count.
    /// - Width or height is zero.
    pub fn acquire(&mut self, width: u32, height: u32) -> Option<(PoolHandle, &mut [u8])> {
        if width == 0 || height == 0 {
            return None;
        }

        let needed = (width as usize) * (height as usize) * (BPP as usize);
        // First, look for a free entry of the exact same size.
        let mut reuse_idx: Option<usize> = None;

        for i in 0..self.entries.len() {
            let e = &self.entries[i];

            if !e.in_use && e.width == width && e.height == height {
                reuse_idx = Some(i);
                break;
            }
        }

        if let Some(i) = reuse_idx {
            let entry = &mut self.entries[i];

            clear_buffer(&mut entry.data);

            entry.in_use = true;
            entry.used_this_frame = true;

            return Some((PoolHandle(i), &mut entry.data));
        }

        // No reusable entry found. Allocate a new one if within budget.
        if self.total_bytes + needed > self.budget {
            return None;
        }

        let data = alloc::vec![0u8; needed];

        self.total_bytes += needed;
        self.alloc_count += 1;

        // Reuse a cleared slot (from end_frame) to keep indices stable.
        let mut reuse_slot: Option<usize> = None;

        for i in 0..self.entries.len() {
            if self.entries[i].width == 0 && !self.entries[i].in_use {
                reuse_slot = Some(i);
                break;
            }
        }

        let idx = if let Some(i) = reuse_slot {
            let entry = &mut self.entries[i];

            entry.width = width;
            entry.height = height;
            entry.data = data;
            entry.in_use = true;
            entry.used_this_frame = true;
            i
        } else {
            if self.entries.len() >= MAX_ENTRIES {
                self.total_bytes -= needed;
                self.alloc_count -= 1;
                return None;
            }

            let i = self.entries.len();

            self.entries.push(PoolEntry {
                width,
                height,
                data,
                in_use: true,
                used_this_frame: true,
            });
            i
        };

        Some((PoolHandle(idx), &mut self.entries[idx].data))
    }

    /// Release a buffer back to the pool.
    ///
    /// The buffer remains allocated and available for reuse on subsequent
    /// `acquire()` calls with the same dimensions.
    pub fn release(&mut self, handle: PoolHandle) {
        if let Some(entry) = self.entries.get_mut(handle.0) {
            entry.in_use = false;
        }
    }

    /// Mark the end of a frame. Frees entries that were not used during
    /// this frame (they are no longer needed by the scene).
    ///
    /// Entries that were used this frame are kept for potential reuse
    /// next frame. The `used_this_frame` flags are then reset.
    pub fn end_frame(&mut self) {
        // Clear stale entries in-place (not removed) so that outstanding
        // PoolHandle indices remain valid. Cleared slots (width == 0)
        // are reused by acquire() for new allocations.
        for entry in &mut self.entries {
            if !entry.used_this_frame && !entry.in_use {
                self.total_bytes -= entry.data.len();
                entry.data = Vec::new();
                entry.width = 0;
                entry.height = 0;
            }

            entry.used_this_frame = false;
        }
    }

    /// Total bytes currently allocated by the pool.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Number of active entries in the pool (excludes cleared slots).
    pub fn entry_count(&self) -> usize {
        self.entries.iter().filter(|e| e.width > 0).count()
    }

    /// Cumulative count of new allocations (not reuses).
    pub fn alloc_count(&self) -> usize {
        self.alloc_count
    }
}

/// Clear a BGRA8888 buffer to fully transparent (all zeros).
fn clear_buffer(buf: &mut [u8]) {
    // SAFETY: Writing zeros to a u8 slice is always safe.
    // Using ptr::write_bytes for efficiency on large buffers.
    unsafe {
        core::ptr::write_bytes(buf.as_mut_ptr(), 0, buf.len());
    }
}
