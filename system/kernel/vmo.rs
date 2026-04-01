//! Virtual Memory Objects — the foundational shared-memory abstraction.
//!
//! A VMO is a handle to a set of physical pages. Any process holding a VMO
//! handle can map it into its address space. VMOs are the single memory object
//! type in this kernel — they subsume DMA buffers and memory_share.
//!
//! # Design
//!
//! - **Fixed size** at creation (no resize).
//! - **Lazy backing** by default: pages allocated on first fault (zero-fill).
//!   Contiguous VMOs are eager (all pages allocated at creation).
//! - **Versioned**: COW generation snapshots via `snapshot()`/`restore()`.
//! - **Sealable**: `seal()` permanently freezes content and metadata.
//! - **Content-typed**: immutable `type_tag: u64` for IPC type safety.
//!
//! # Page Tracking
//!
//! Each VMO owns a `BTreeMap<u64, (Pa, u32)>` mapping page offsets to
//! `(physical_address, refcount)`. Refcount > 1 means the page is shared
//! with a snapshot — writes trigger COW in the fault handler.
//!
//! # Novel Features (Beyond Existing Microkernels)
//!
//! 1. Ownership-typed: Rust `Drop` for compile-time use-after-free prevention
//! 2. Versioned: COW generation snapshots (bounded ring)
//! 3. Append-only + Seal permissions (fine-grained beyond RWX)
//! 4. Content-type tag (RedLeaf-inspired, OSDI '20)
//!
//! See `design/kernel-v0.6.md` Phase 3a for full design rationale.

extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};

use super::memory::Pa;
#[cfg(not(test))]
use super::sync::IrqMutex;

// =========================================================================
// Public types
// =========================================================================

/// Identifies a VMO in the global VMO table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmoId(pub u32);

/// Creation flags for `vmo_create`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmoFlags(u32);

impl VmoFlags {
    /// Physically contiguous pages (eager allocation via buddy allocator).
    pub const CONTIGUOUS: Self = Self(1 << 0);

    pub const fn bits(self) -> u32 {
        self.0
    }
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
    pub const fn empty() -> Self {
        Self(0)
    }
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
}

/// Info returned by `vmo_get_info`.
pub const VMO_FLAG_CONTIGUOUS: u64 = 1 << 0;
pub const VMO_FLAG_SEALED: u64 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct VmoInfo {
    pub size_pages: u64,
    pub flags: u64,
    pub type_tag: u64,
    pub generation: u64,
    pub committed_pages: u64,
}

// =========================================================================
// Snapshot
// =========================================================================

/// A COW snapshot of a VMO's page list at a specific generation.
struct VmoSnapshot {
    generation: u64,
    /// Clone of the page list at snapshot time. Pages shared with the
    /// current generation have refcount > 1.
    pages: BTreeMap<u64, (Pa, u32)>,
}

// =========================================================================
// Vmo — the core type
// =========================================================================

/// Default maximum snapshot depth (undo ring size).
const DEFAULT_MAX_SNAPSHOTS: usize = 64;

/// A Virtual Memory Object.
///
/// Owns physical pages and tracks their commitment state. When dropped,
/// all pages with refcount 1 are freed. Pages shared with snapshots have
/// their refcount decremented.
pub struct Vmo {
    /// Per-page tracking: offset → (physical address, refcount).
    /// Absent = uncommitted (zero-fill on fault or zero-return for vmo_read).
    /// Refcount > 1 = shared with snapshots (COW on write).
    pages: BTreeMap<u64, (Pa, u32)>,
    /// Fixed at creation, in pages.
    size_pages: u64,
    /// Creation flags (CONTIGUOUS, etc.).
    flags: VmoFlags,
    /// Opaque content-type tag. Set at creation, immutable.
    type_tag: u64,
    /// Current generation number. Incremented by snapshot().
    generation: u64,
    /// COW snapshot ring. Bounded by `max_snapshots`.
    snapshots: Vec<VmoSnapshot>,
    /// Maximum snapshot depth. Oldest dropped when exceeded.
    max_snapshots: usize,
    /// True after seal(). All mutating operations rejected.
    sealed: bool,
}

impl Vmo {
    /// Create a new VMO. Pages are not allocated (lazy).
    fn new(size_pages: u64, flags: VmoFlags, type_tag: u64) -> Self {
        Self {
            pages: BTreeMap::new(),
            size_pages,
            flags,
            type_tag,
            generation: 0,
            snapshots: Vec::new(),
            max_snapshots: DEFAULT_MAX_SNAPSHOTS,
            sealed: false,
        }
    }

    /// Collect all physical addresses owned by this VMO (current pages +
    /// all snapshot pages) that would need to be freed on destroy.
    /// Returns only pages whose effective refcount within this VMO is the
    /// last reference (refcount == 1 in the only copy that holds them).
    ///
    /// For simplicity and correctness, this returns ALL unique Pa values —
    /// the caller (destroy) is responsible for freeing each exactly once.
    fn collect_all_pages(&self) -> Vec<Pa> {
        let mut all = BTreeMap::<usize, ()>::new(); // Use Pa.0 as key for dedup

        for &(pa, _) in self.pages.values() {
            all.insert(pa.0, ());
        }

        for snap in &self.snapshots {
            for &(pa, _) in snap.pages.values() {
                all.insert(pa.0, ());
            }
        }

        all.keys().map(|&addr| Pa(addr)).collect()
    }
    /// Decrement refcounts for pages referenced by an evicted snapshot.
    /// Does not free pages — just adjusts refcounts in the current page list.
    fn decrement_snapshot_refcounts(&mut self, snap: &VmoSnapshot) {
        for (&offset, &(snap_pa, _)) in &snap.pages {
            // Decrement in the current page list if the same PA is still there.
            if let Some(entry) = self.pages.get_mut(&offset) {
                if entry.0 == snap_pa && entry.1 > 1 {
                    entry.1 -= 1;
                }
            }

            // Also decrement in other snapshots that share the same page.
            for other_snap in &mut self.snapshots {
                if let Some(entry) = other_snap.pages.get_mut(&offset) {
                    if entry.0 == snap_pa && entry.1 > 1 {
                        entry.1 -= 1;
                    }
                }
            }
        }
    }

    /// Commit a page at the given offset. Unconditional — does not check
    /// seal or bounds beyond size_pages. Used by the fault handler after
    /// external validation.
    ///
    /// Ignores offsets >= size_pages (caller bug, but safe).
    pub fn commit_page(&mut self, offset: u64, pa: Pa) {
        if offset >= self.size_pages {
            return;
        }

        self.pages.insert(offset, (pa, 1));
    }
    pub fn committed_pages(&self) -> u64 {
        self.pages.len() as u64
    }
    /// COW-replace a page: insert `new_pa` at refcount=1, decrement the
    /// old page's refcount. Returns the old Pa if its refcount hit 0
    /// (caller should free it), or None if the old page is still
    /// referenced by snapshots.
    ///
    /// Used by the fault handler when writing to a page with refcount > 1.
    pub fn cow_replace_page(&mut self, offset: u64, new_pa: Pa) -> Option<Pa> {
        let old_entry = self.pages.insert(offset, (new_pa, 1));

        match old_entry {
            Some((old_pa, 1)) => {
                // Refcount was 1 — the old page is now unreferenced.
                Some(old_pa)
            }
            Some((old_pa, rc)) => {
                // Decrement the refcount in all snapshots that share this page.
                // The old page stays alive in snapshots.
                for snap in &mut self.snapshots {
                    if let Some(entry) = snap.pages.get_mut(&offset) {
                        if entry.0 == old_pa && entry.1 > 1 {
                            entry.1 = rc - 1;
                        }
                    }
                }
                None // Old page still referenced by snapshots
            }
            None => None, // No old page (was uncommitted)
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Return structured info for the `vmo_get_info` syscall.
    pub fn info(&self) -> VmoInfo {
        let mut flags = 0u64;

        if self.is_contiguous() {
            flags |= VMO_FLAG_CONTIGUOUS;
        }
        if self.sealed {
            flags |= VMO_FLAG_SEALED;
        }

        VmoInfo {
            size_pages: self.size_pages,
            flags,
            type_tag: self.type_tag,
            generation: self.generation,
            committed_pages: self.committed_pages(),
        }
    }
    pub fn is_contiguous(&self) -> bool {
        self.flags.contains(VmoFlags::CONTIGUOUS)
    }
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }
    /// Look up a page by offset. Returns `(Pa, refcount)` or None if
    /// uncommitted.
    pub fn lookup_page(&self, offset: u64) -> Option<(Pa, u32)> {
        self.pages.get(&offset).copied()
    }
    pub fn max_snapshots(&self) -> usize {
        self.max_snapshots
    }
    /// Check if a page needs COW (refcount > 1).
    pub fn page_needs_cow(&self, offset: u64) -> bool {
        self.pages.get(&offset).map_or(false, |&(_, rc)| rc > 1)
    }
    /// Restore the VMO to a previous snapshot generation.
    /// Returns a list of Pa values whose refcount hit 0 (caller should free).
    /// Returns None if generation not found, or VMO is sealed.
    pub fn restore(&mut self, target_gen: u64) -> Option<Vec<Pa>> {
        if self.sealed {
            return None;
        }

        // Find the snapshot.
        let snap_idx = self
            .snapshots
            .iter()
            .position(|s| s.generation == target_gen)?;
        let snap = self.snapshots.remove(snap_idx);
        let mut freed = Vec::new();

        // Decrement refcounts in the current page list for pages that
        // won't be in the restored state.
        for (&offset, &(pa, rc)) in &self.pages {
            if let Some(&(snap_pa, _)) = snap.pages.get(&offset) {
                if snap_pa == pa {
                    continue; // Same page in both — no change needed
                }
            }

            // Page is in current but not in snapshot (or different Pa).
            // Decrement its refcount; if it hits 0, free it.
            if rc <= 1 {
                freed.push(pa);
            }
            // If rc > 1, other snapshots still reference it — don't free.
        }

        // Replace current page list with snapshot's.
        self.pages = snap.pages;

        Some(freed)
    }
    /// Seal the VMO. Irreversible.
    pub fn seal(&mut self) {
        self.sealed = true;
    }
    pub fn size_pages(&self) -> u64 {
        self.size_pages
    }
    /// Create a COW snapshot of the current page list.
    /// Returns the new generation number, or None if sealed or contiguous.
    pub fn snapshot(&mut self) -> Option<u64> {
        if self.sealed || self.is_contiguous() {
            return None;
        }

        // Clone the page list — shared pages get refcount incremented.
        let mut snap_pages = self.pages.clone();

        // Increment refcount for all committed pages (they're now shared).
        for (offset, (_pa, rc)) in &mut self.pages {
            *rc += 1;

            // Also update the snapshot's copy to match.
            if let Some(entry) = snap_pages.get_mut(offset) {
                entry.1 = *rc;
            }
        }

        self.generation += 1;

        let snap = VmoSnapshot {
            generation: self.generation - 1, // snapshot captures pre-increment state
            pages: snap_pages,
        };
        // Evict oldest if at capacity.
        let evicted = if self.snapshots.len() >= self.max_snapshots {
            Some(self.snapshots.remove(0))
        } else {
            None
        };

        self.snapshots.push(snap);

        // Decrement refcounts for pages in evicted snapshot.
        if let Some(evicted) = evicted {
            self.decrement_snapshot_refcounts(&evicted);
        }

        Some(self.generation)
    }
    /// Try to commit a page, respecting seal. Returns false if sealed or
    /// out of bounds.
    pub fn try_commit_page(&mut self, offset: u64, pa: Pa) -> bool {
        if self.sealed || offset >= self.size_pages {
            return false;
        }

        self.pages.insert(offset, (pa, 1));

        true
    }
    pub fn type_tag(&self) -> u64 {
        self.type_tag
    }
}

// =========================================================================
// VmoTable — global storage (analogous to channel::State)
// =========================================================================

/// Global VMO table. In the kernel, wrapped in IrqMutex.
/// Here exposed directly for testing.
pub struct VmoTable {
    vmos: Vec<Option<Vmo>>,
}

impl VmoTable {
    pub const fn new() -> Self {
        Self { vmos: Vec::new() }
    }

    /// Create a new VMO. Returns None if size is 0.
    /// For contiguous VMOs, the caller must pre-commit pages after creation.
    pub fn create(&mut self, size_pages: u64, flags: VmoFlags, type_tag: u64) -> Option<VmoId> {
        if size_pages == 0 {
            return None;
        }

        let vmo = Vmo::new(size_pages, flags, type_tag);

        // Find a free slot or append.
        for (i, slot) in self.vmos.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(vmo);

                return Some(VmoId(i as u32));
            }
        }

        let id = self.vmos.len() as u32;

        self.vmos.push(Some(vmo));

        Some(VmoId(id))
    }
    /// Destroy a VMO. Returns all unique physical addresses that should be
    /// freed by the caller (page_allocator::free_frame for each).
    pub fn destroy(&mut self, id: VmoId) -> Vec<Pa> {
        let slot = match self.vmos.get_mut(id.0 as usize) {
            Some(slot) => slot,
            None => return Vec::new(),
        };

        match slot.take() {
            Some(vmo) => vmo.collect_all_pages(),
            None => Vec::new(),
        }
    }
    /// Get a reference to a VMO.
    pub fn get(&self, id: VmoId) -> Option<&Vmo> {
        self.vmos.get(id.0 as usize)?.as_ref()
    }
    /// Get a mutable reference to a VMO.
    pub fn get_mut(&mut self, id: VmoId) -> Option<&mut Vmo> {
        self.vmos.get_mut(id.0 as usize)?.as_mut()
    }
}

// =========================================================================
// Global state (kernel-only, not compiled in test)
// =========================================================================

#[cfg(not(test))]
pub static STATE: IrqMutex<VmoTable> = IrqMutex::new(VmoTable::new_const());

#[cfg(not(test))]
impl VmoTable {
    /// Const constructor for static initialization.
    const fn new_const() -> Self {
        Self { vmos: Vec::new() }
    }
}

// =========================================================================
// Public module API (called from syscall.rs, scheduler.rs)
// =========================================================================

/// Create a new VMO. Returns `(VmoId, VmoFlags)` on success, None if size is 0.
#[cfg(not(test))]
pub fn create(size_pages: u64, flags: VmoFlags, type_tag: u64) -> Option<VmoId> {
    STATE.lock().create(size_pages, flags, type_tag)
}
/// Destroy a VMO and return all physical pages to free.
/// Called from handle_close and close_handle_categories.
#[cfg(not(test))]
pub fn destroy(id: VmoId) -> Vec<Pa> {
    STATE.lock().destroy(id)
}
/// Get VMO info for the `vmo_get_info` syscall.
#[cfg(not(test))]
pub fn get_info(id: VmoId) -> Option<VmoInfo> {
    STATE.lock().get(id).map(|v| v.info())
}
/// Read from a VMO into a kernel buffer. Returns bytes read.
///
/// For uncommitted pages, writes zeros to the buffer (no allocation).
/// For committed pages, copies from the physical frame.
#[cfg(not(test))]
pub fn read(id: VmoId, offset: u64, buf: &mut [u8]) -> Option<u64> {
    let state = STATE.lock();
    let vmo = state.get(id)?;
    let page_size = super::paging::PAGE_SIZE;

    let vmo_size_bytes = vmo.size_pages() * page_size;
    if offset >= vmo_size_bytes {
        return Some(0);
    }

    let available = (vmo_size_bytes - offset) as usize;
    let to_read = buf.len().min(available);

    let mut bytes_done = 0usize;
    while bytes_done < to_read {
        let current_offset = offset + bytes_done as u64;
        let page_idx = current_offset / page_size;
        let page_off = (current_offset % page_size) as usize;
        let chunk = (page_size as usize - page_off).min(to_read - bytes_done);

        if let Some((pa, _)) = vmo.lookup_page(page_idx) {
            // Committed page — copy from physical memory.
            // SAFETY: pa is a valid allocated frame. phys_to_virt produces
            // a valid kernel VA. page_off + chunk <= PAGE_SIZE.
            unsafe {
                let src = (super::memory::phys_to_virt(pa) as *const u8).add(page_off);
                core::ptr::copy_nonoverlapping(src, buf[bytes_done..].as_mut_ptr(), chunk);
            }
        } else {
            // Uncommitted — return zeros without allocating.
            buf[bytes_done..bytes_done + chunk].fill(0);
        }

        bytes_done += chunk;
    }

    Some(bytes_done as u64)
}
/// Get the size of a VMO in pages.
#[cfg(not(test))]
pub fn size_pages(id: VmoId) -> Option<u64> {
    STATE.lock().get(id).map(|v| v.size_pages())
}
/// Write to a VMO from a kernel buffer. Returns bytes written.
///
/// Commits pages on first write. Respects seal (returns None if sealed).
/// If `append_only` is true, only allows writes at offset >= committed frontier.
#[cfg(not(test))]
pub fn write(id: VmoId, offset: u64, data: &[u8], append_only: bool) -> Option<u64> {
    let mut state = STATE.lock();
    let vmo = state.get_mut(id)?;
    let page_size = super::paging::PAGE_SIZE;

    if vmo.is_sealed() {
        return None; // Sealed — reject all writes
    }

    let vmo_size_bytes = vmo.size_pages() * page_size;
    if offset >= vmo_size_bytes {
        return Some(0);
    }

    // Append-only check: reject writes before the committed frontier.
    // The frontier is the byte after the last committed page's end.
    if append_only {
        let committed_frontier = vmo.committed_pages() * page_size;
        if offset < committed_frontier {
            return None; // Would overwrite existing data
        }
    }

    let available = (vmo_size_bytes - offset) as usize;
    let to_write = data.len().min(available);

    let mut bytes_done = 0usize;
    while bytes_done < to_write {
        let current_offset = offset + bytes_done as u64;
        let page_idx = current_offset / page_size;
        let page_off = (current_offset % page_size) as usize;
        let chunk = (page_size as usize - page_off).min(to_write - bytes_done);

        // Ensure the page is committed (allocate if needed).
        let pa = if let Some((existing_pa, refcount)) = vmo.lookup_page(page_idx) {
            if refcount > 1 {
                // COW: allocate, copy, replace.
                let new_pa = super::page_allocator::alloc_frame()?;
                unsafe {
                    let src = super::memory::phys_to_virt(existing_pa) as *const u8;
                    let dst = super::memory::phys_to_virt(new_pa) as *mut u8;
                    core::ptr::copy_nonoverlapping(src, dst, page_size as usize);
                }
                if let Some(freed) = vmo.cow_replace_page(page_idx, new_pa) {
                    super::page_allocator::free_frame(freed);
                }
                new_pa
            } else {
                existing_pa
            }
        } else {
            // Uncommitted — allocate and commit.
            let new_pa = super::page_allocator::alloc_frame()?;
            // alloc_frame returns zeroed memory.
            vmo.commit_page(page_idx, new_pa);
            new_pa
        };

        // Write data to the physical frame.
        // SAFETY: pa is a valid frame, phys_to_virt valid, page_off+chunk <= PAGE_SIZE.
        unsafe {
            let dst = (super::memory::phys_to_virt(pa) as *mut u8).add(page_off);
            core::ptr::copy_nonoverlapping(data[bytes_done..].as_ptr(), dst, chunk);
        }

        bytes_done += chunk;
    }

    Some(bytes_done as u64)
}
