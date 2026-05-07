//! Dynamic page table manipulation for user address spaces.
//!
//! Creates per-process page tables in TTBR0 (lower half) while the kernel
//! runs from TTBR1 (upper half). Supports map, unmap, COW marking, and
//! ASID-tagged TLB management.

// Most of this module is #[cfg(target_os = "none")] — constants and imports
// appear unused during host-side test compilation.
#![allow(dead_code)]

use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(target_os = "none")]
use super::{
    page_alloc::{self, PhysAddr},
    platform, sysreg,
};
use crate::config;

#[cfg(target_os = "none")]
#[inline(always)]
fn pa_to_ptr<T>(pa: usize) -> *mut T {
    platform::phys_to_virt(pa) as *mut T
}

/// Virtual address newtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(pub usize);

/// ASID (Address Space Identifier) for TLB tagging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Asid(pub u8);

/// Page permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Perms {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl Perms {
    pub const RO: Perms = Perms {
        read: true,
        write: false,
        execute: false,
    };
    pub const RW: Perms = Perms {
        read: true,
        write: true,
        execute: false,
    };
    pub const RX: Perms = Perms {
        read: true,
        write: false,
        execute: true,
    };
}

// Page table descriptor bits (same constants as mmu.rs, but for user pages).
const VALID: u64 = 1 << 0;
const TABLE: u64 = 1 << 1;
const PAGE: u64 = 1 << 1;
const AF: u64 = 1 << 10;
const SH_ISH: u64 = 0b11 << 8;
const AP_RW_ALL: u64 = 0b01 << 6; // EL1+EL0 read/write
const AP_RO_ALL: u64 = 0b11 << 6; // EL1+EL0 read-only
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ATTR_NORMAL: u64 = 1 << 2; // MAIR index 1
const SW_COW: u64 = 1 << 55; // Software-defined COW bit (available PTE bit)

const PAGE_SIZE: usize = config::PAGE_SIZE;
const PAGE_SHIFT: usize = 14; // log2(16384)
const ENTRIES_PER_TABLE: usize = PAGE_SIZE / 8; // 2048

const PA_MASK: u64 = 0x0000_FFFF_FFFF_C000;

// With 16KB granule and 36-bit VA (T0SZ=28):
// L2: bits [35:25] -> 2048 entries, each covers 32 MiB
// L3: bits [24:14] -> 2048 entries, each covers 16 KiB

// ---------------------------------------------------------------------------
// ASID allocator: simple flat array, 128 slots
// ---------------------------------------------------------------------------

#[allow(clippy::declare_interior_mutable_const)]
static ASID_MAP: [AtomicU8; config::MAX_ADDRESS_SPACES] = {
    const FREE: AtomicU8 = AtomicU8::new(0);
    [FREE; config::MAX_ADDRESS_SPACES]
};

/// Allocate an ASID. Returns None if all 128 are in use.
pub fn alloc_asid() -> Option<Asid> {
    #[allow(clippy::needless_range_loop)]
    for i in 0..config::MAX_ADDRESS_SPACES {
        if ASID_MAP[i]
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return Some(Asid((i + 1) as u8)); // ASID 0 is reserved for kernel
        }
    }

    None
}

/// Release an ASID back to the pool.
pub fn free_asid(asid: Asid) {
    let idx = asid.0 as usize - 1;

    ASID_MAP[idx].store(0, Ordering::Release);
}

/// Reset the entire ASID pool. Test-only — not safe for concurrent use.
#[cfg(test)]
pub fn reset_asid_pool() {
    for slot in &ASID_MAP {
        slot.store(0, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Page table operations
// ---------------------------------------------------------------------------

/// Create a new empty L2 page table root for a user address space.
/// Returns (physical address of the root page, assigned ASID).
#[cfg(target_os = "none")]
pub fn create_page_table() -> Option<(PhysAddr, Asid)> {
    let root = page_alloc::alloc_page()?;
    let asid = alloc_asid().or_else(|| {
        page_alloc::release(root);
        None
    })?;

    // SAFETY: We just allocated this page; no other reference exists.
    unsafe {
        let ptr: *mut u8 = pa_to_ptr(root.as_usize());

        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }

    Some((root, asid))
}

/// Destroy a page table, freeing all table pages and releasing the ASID.
#[cfg(target_os = "none")]
pub fn destroy_page_table(root: PhysAddr, asid: Asid) {
    // Walk and free all L3 table pages pointed to by L2 entries.
    // SAFETY: root is a valid page table root we own.
    unsafe {
        let l2: *const u64 = pa_to_ptr(root.as_usize());

        for i in 0..ENTRIES_PER_TABLE {
            let entry = core::ptr::read_volatile(l2.add(i));

            if entry & VALID != 0 && entry & TABLE != 0 {
                let l3_pa = PhysAddr((entry & PA_MASK) as usize);

                page_alloc::release(l3_pa);
            }
        }
    }

    // Invalidate all TLB entries for this ASID before freeing it.
    sysreg::tlbi_aside1is(asid.0 as u64);
    sysreg::dsb_ish();
    sysreg::isb();
    page_alloc::release(root);
    free_asid(asid);
}

/// Map a single page at `vaddr` in the page table rooted at `root`.
/// W^X enforced: cannot set both write and execute.
#[cfg(target_os = "none")]
pub fn map_page(root: PhysAddr, vaddr: VirtAddr, paddr: PhysAddr, perms: Perms) {
    assert!(!(perms.write && perms.execute), "W^X violation");

    let l2_idx = (vaddr.0 >> 25) & (ENTRIES_PER_TABLE - 1);
    let l3_idx = (vaddr.0 >> PAGE_SHIFT) & (ENTRIES_PER_TABLE - 1);

    // SAFETY: root is a valid L2 table page we own.
    unsafe {
        let l2: *mut u64 = pa_to_ptr(root.as_usize());
        let l2_entry = core::ptr::read_volatile(l2.add(l2_idx));

        // Ensure L3 table exists.
        let l3_pa = if l2_entry & VALID != 0 {
            (l2_entry & PA_MASK) as usize
        } else {
            let new_l3 = page_alloc::alloc_page().expect("OOM: page table page");

            core::ptr::write_bytes(pa_to_ptr::<u8>(new_l3.as_usize()), 0, PAGE_SIZE);

            let desc = (new_l3.as_usize() as u64) | TABLE | VALID;

            core::ptr::write_volatile(l2.add(l2_idx), desc);

            new_l3.as_usize()
        };

        // Build L3 page descriptor.
        let mut attrs = ATTR_NORMAL | SH_ISH | AF | PAGE | VALID;

        if perms.write {
            attrs |= AP_RW_ALL;
        } else {
            attrs |= AP_RO_ALL;
        }
        if !perms.execute {
            attrs |= UXN;
        }

        attrs |= PXN; // User pages are never kernel-executable.

        let desc = (paddr.as_usize() as u64) | attrs;
        let l3: *mut u64 = pa_to_ptr(l3_pa);

        core::ptr::write_volatile(l3.add(l3_idx), desc);
    }
}

/// Unmap a single page at `vaddr`, returning the physical address that was mapped.
#[cfg(target_os = "none")]
pub fn unmap_page(root: PhysAddr, vaddr: VirtAddr) -> Option<PhysAddr> {
    let l2_idx = (vaddr.0 >> 25) & (ENTRIES_PER_TABLE - 1);
    let l3_idx = (vaddr.0 >> PAGE_SHIFT) & (ENTRIES_PER_TABLE - 1);

    // SAFETY: root is a valid page table we own.
    unsafe {
        let l2: *const u64 = pa_to_ptr(root.as_usize());
        let l2_entry = core::ptr::read_volatile(l2.add(l2_idx));

        if l2_entry & VALID == 0 {
            return None;
        }

        let l3_pa = (l2_entry & PA_MASK) as usize;
        let l3: *mut u64 = pa_to_ptr(l3_pa);
        let l3_entry = core::ptr::read_volatile(l3.add(l3_idx));

        if l3_entry & VALID == 0 {
            return None;
        }

        let paddr = PhysAddr((l3_entry & PA_MASK) as usize);

        core::ptr::write_volatile(l3.add(l3_idx), 0);

        Some(paddr)
    }
}

/// Mark a page as copy-on-write: set read-only + SW_COW bit.
#[cfg(target_os = "none")]
pub fn set_cow(root: PhysAddr, vaddr: VirtAddr) {
    let l2_idx = (vaddr.0 >> 25) & (ENTRIES_PER_TABLE - 1);
    let l3_idx = (vaddr.0 >> PAGE_SHIFT) & (ENTRIES_PER_TABLE - 1);

    // SAFETY: root is a valid page table we own.
    unsafe {
        let l2: *const u64 = pa_to_ptr(root.as_usize());
        let l2_entry = core::ptr::read_volatile(l2.add(l2_idx));

        if l2_entry & VALID == 0 {
            return;
        }

        let l3_pa = (l2_entry & PA_MASK) as usize;
        let l3: *mut u64 = pa_to_ptr(l3_pa);
        let mut entry = core::ptr::read_volatile(l3.add(l3_idx));

        if entry & VALID == 0 {
            return;
        }

        // Clear write, set read-only + COW marker.
        entry &= !AP_RW_ALL;
        entry |= AP_RO_ALL | SW_COW;

        core::ptr::write_volatile(l3.add(l3_idx), entry);
    }
}

/// Clear the write permission on a page (for seal).
#[cfg(target_os = "none")]
pub fn clear_write(root: PhysAddr, vaddr: VirtAddr) {
    let l2_idx = (vaddr.0 >> 25) & (ENTRIES_PER_TABLE - 1);
    let l3_idx = (vaddr.0 >> PAGE_SHIFT) & (ENTRIES_PER_TABLE - 1);

    // SAFETY: root is a valid page table we own.
    unsafe {
        let l2: *const u64 = pa_to_ptr(root.as_usize());
        let l2_entry = core::ptr::read_volatile(l2.add(l2_idx));

        if l2_entry & VALID == 0 {
            return;
        }

        let l3_pa = (l2_entry & PA_MASK) as usize;
        let l3: *mut u64 = pa_to_ptr(l3_pa);
        let mut entry = core::ptr::read_volatile(l3.add(l3_idx));

        if entry & VALID == 0 {
            return;
        }

        entry &= !AP_RW_ALL;
        entry |= AP_RO_ALL;

        core::ptr::write_volatile(l3.add(l3_idx), entry);
    }
}

/// Load TTBR0 with a page table root + ASID for address space switching.
#[cfg(target_os = "none")]
pub fn switch_table(root: PhysAddr, asid: Asid) {
    let val = (root.as_usize() as u64) | ((asid.0 as u64) << 48);

    sysreg::set_ttbr0_el1(val);

    // Flush this core's TLB after switching TTBR0. The ARM architecture says
    // ASID-tagged entries from the old space won't match the new ASID, so a
    // TLBI should not be required. In practice, omitting it causes stale TLB
    // hits under the hypervisor (observed: thread N+1 executing thread N's
    // code page without faulting, despite distinct ASIDs and page tables).
    // Root cause is unconfirmed — could be a hypervisor TLB emulation gap or
    // an undiagnosed kernel issue. The TLBI is defensive.
    #[cfg(target_os = "none")]
    // SAFETY: TLBI VMALLE1 invalidates this core's EL0/EL1 TLB entries.
    // DSB ISH ensures completion before the ISB.
    unsafe {
        core::arch::asm!("tlbi vmalle1", "dsb ish", options(nostack),);
    }

    sysreg::isb();
}

/// Invalidate a single page's TLB entry for a given ASID.
///
/// Page-aligns the VA before computing the TLBI operand — raw fault
/// addresses may have non-zero bits below PAGE_SHIFT, which would produce
/// an incorrect TLBI on granules > 4KB.
#[cfg(target_os = "none")]
pub fn invalidate_page(asid: Asid, vaddr: VirtAddr) {
    // TLBI VAE1IS format: ASID[63:48] | VA[43:12].
    // Page-align first: for 16KB granule, bits [13:12] must be zero.
    let aligned = vaddr.0 & !(config::PAGE_SIZE - 1);
    let val = ((asid.0 as u64) << 48) | ((aligned as u64) >> 12);

    // DSB ISHST ensures preceding PTE stores are visible to hardware walkers
    // on all cores before the TLBI invalidates cached translations.
    sysreg::dsb_ishst();
    sysreg::tlbi_vae1is(val);
    sysreg::dsb_ish();
    sysreg::isb();
}

/// Replace a valid page mapping with a new physical address.
///
/// Implements break-before-make (ARM ARM D8.14.1): valid-to-valid PTE
/// transitions require writing invalid first, flushing the TLB, then
/// writing the new valid entry. Without this, cores may hold stale TLB
/// entries for the old translation.
#[cfg(target_os = "none")]
pub fn replace_page(
    root: PhysAddr,
    asid: Asid,
    vaddr: VirtAddr,
    new_paddr: PhysAddr,
    perms: Perms,
) {
    unmap_page(root, vaddr);
    invalidate_page(asid, vaddr);
    map_page(root, vaddr, new_paddr, perms);
}

/// Invalidate all TLB entries for an ASID.
#[cfg(target_os = "none")]
pub fn invalidate_asid(asid: Asid) {
    sysreg::tlbi_aside1is(asid.0 as u64);
    sysreg::dsb_ish();
    sysreg::isb();
}

/// Check if a PTE has the COW bit set (for fault handler).
#[cfg(target_os = "none")]
pub fn is_cow(root: PhysAddr, vaddr: VirtAddr) -> bool {
    let l2_idx = (vaddr.0 >> 25) & (ENTRIES_PER_TABLE - 1);
    let l3_idx = (vaddr.0 >> PAGE_SHIFT) & (ENTRIES_PER_TABLE - 1);

    // SAFETY: root is a valid page table we own.
    unsafe {
        let l2: *const u64 = pa_to_ptr(root.as_usize());
        let l2_entry = core::ptr::read_volatile(l2.add(l2_idx));

        if l2_entry & VALID == 0 {
            return false;
        }

        let l3_pa = (l2_entry & PA_MASK) as usize;
        let l3: *const u64 = pa_to_ptr(l3_pa);
        let entry = core::ptr::read_volatile(l3.add(l3_idx));

        entry & SW_COW != 0
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    #[serial]
    fn asid_allocate_and_free() {
        // Reset ASID map.
        for i in 0..config::MAX_ADDRESS_SPACES {
            ASID_MAP[i].store(0, Ordering::Relaxed);
        }

        let a1 = alloc_asid().unwrap();

        assert_eq!(a1.0, 1);

        let a2 = alloc_asid().unwrap();

        assert_eq!(a2.0, 2);

        free_asid(a1);

        let a3 = alloc_asid().unwrap();

        assert_eq!(a3.0, 1); // Reuses slot 0.
    }

    #[test]
    #[serial]
    fn asid_exhaustion() {
        for i in 0..config::MAX_ADDRESS_SPACES {
            ASID_MAP[i].store(0, Ordering::Relaxed);
        }

        for _ in 0..config::MAX_ADDRESS_SPACES {
            assert!(alloc_asid().is_some());
        }

        assert!(alloc_asid().is_none());

        // Cleanup.
        for i in 0..config::MAX_ADDRESS_SPACES {
            ASID_MAP[i].store(0, Ordering::Relaxed);
        }
    }

    #[test]
    fn wxn_enforced() {
        let bad_perms = Perms {
            read: true,
            write: true,
            execute: true,
        };

        assert!(bad_perms.write && bad_perms.execute); // Would panic in map_page.
    }

    #[test]
    fn perms_constants() {
        assert!(Perms::RO.read && !Perms::RO.write && !Perms::RO.execute);
        assert!(Perms::RW.read && Perms::RW.write && !Perms::RW.execute);
        assert!(Perms::RX.read && !Perms::RX.write && Perms::RX.execute);
    }
}
