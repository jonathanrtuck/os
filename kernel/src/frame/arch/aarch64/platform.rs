//! Platform constants and DTB-discovered values.
//!
//! Fixed addresses (GIC, UART, pvpanic) are consts — they're either not in
//! the DTB or needed before the DTB scan runs. RAM layout and core count
//! start with compiled defaults and are overridden by [`update_from_dtb`]
//! during early boot.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Fixed constants (not in DTB or needed before DTB scan)
// ---------------------------------------------------------------------------

/// GICv3 distributor base address.
pub const GIC_DIST_BASE: usize = 0x0800_0000;

/// GICv3 redistributor base address.
pub const GIC_REDIST_BASE: usize = 0x080A_0000;

/// Kernel physical load address. — link.ld sync: `PHYS_BASE`
pub const KERNEL_BASE: usize = 0x4008_0000;

/// Offset from physical to virtual address for kernel memory.
///
/// With T1SZ=28 the TTBR1 range starts at 0xFFFF_FFF0_0000_0000. The kernel
/// is identity-mapped within that range: VA = PA + VA_OFFSET.
///
///   PA 0x4008_0000  →  VA 0xFFFF_FFF0_4008_0000
///
/// This constant is the single source of truth for the PA↔VA relationship.
/// Every pointer cast from a physical address MUST go through phys_to_virt().
pub const KERNEL_VA_OFFSET: usize = 0xFFFF_FFF0_0000_0000;

static MMU_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_mmu_active() {
    MMU_ACTIVE.store(true, Ordering::Release);
}

#[inline(always)]
pub const fn phys_to_virt(pa: usize) -> usize {
    pa.wrapping_add(KERNEL_VA_OFFSET)
}

#[inline(always)]
pub const fn virt_to_phys(va: usize) -> usize {
    va.wrapping_sub(KERNEL_VA_OFFSET)
}

/// Return a device MMIO address usable as a pointer. Before MMU enable
/// this is the raw physical address; after, it is the TTBR1 upper-half VA.
#[inline(always)]
pub fn device_addr(pa: usize) -> usize {
    if MMU_ACTIVE.load(Ordering::Relaxed) {
        phys_to_virt(pa)
    } else {
        pa
    }
}

/// pvpanic device base address (QEMU pvpanic-mmio spec).
pub const PVPANIC_BASE: usize = 0x0902_0000;

/// PL011 UART base address.
pub const UART_BASE: usize = 0x0900_0000;

// ---------------------------------------------------------------------------
// Runtime-discovered values (from DTB, with compiled defaults)
// ---------------------------------------------------------------------------

// Ordering::Relaxed is correct: the BSP writes these values before calling
// activate_secondaries(), and the PSCI CPU_ON mechanism provides an implicit
// memory ordering barrier — the hypervisor ensures each secondary core's view
// is coherent with the BSP's stores at the point of CPU_ON.
static CORE_COUNT: AtomicUsize = AtomicUsize::new(1);
static RAM_BASE_VAL: AtomicUsize = AtomicUsize::new(0x4000_0000);
static RAM_SIZE_VAL: AtomicUsize = AtomicUsize::new(256 * 1024 * 1024);

pub fn core_count() -> usize {
    CORE_COUNT.load(Ordering::Relaxed)
}

pub fn ram_base() -> usize {
    RAM_BASE_VAL.load(Ordering::Relaxed)
}

pub fn ram_size() -> usize {
    RAM_SIZE_VAL.load(Ordering::Relaxed)
}

#[cfg(target_os = "none")]
/// Scan the device tree and override defaults with discovered values.
///
/// Called once during early boot, before the MMU or any consumer reads
/// these values. Falls back to compiled defaults if the DTB is absent
/// or malformed.
pub fn init(dtb_ptr: usize) {
    if let Some(info) = crate::frame::firmware::dtb::scan(dtb_ptr) {
        if info.ram_base != 0 {
            let expected = RAM_BASE_VAL.load(Ordering::Relaxed);

            if info.ram_base != expected {
                // Can't use println! before MMU enable (vtable dispatch
                // uses upper-half VAs). Just halt — the mismatch means
                // the hypervisor and kernel disagree on memory layout.
                loop {
                    core::hint::spin_loop();
                }
            }
        }
        if info.ram_size != 0 {
            RAM_SIZE_VAL.store(info.ram_size, Ordering::Relaxed);
        }
        if info.core_count != 0 {
            CORE_COUNT.store(info.core_count, Ordering::Relaxed);
        }
    }
}

/// Print the DTB scan results. Called after MMU enable.
pub fn print_info() {
    crate::println!(
        "dtb: ram {:#x}+{:#x}, {} core(s)",
        ram_base(),
        ram_size(),
        core_count(),
    );
}
