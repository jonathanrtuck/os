//! Platform constants and DTB-discovered values.
//!
//! Fixed addresses (GIC, UART, pvpanic) are consts — they're either not in
//! the DTB or needed before the DTB scan runs. RAM layout and core count
//! start with compiled defaults and are overridden by [`update_from_dtb`]
//! during early boot.

use core::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Fixed constants (not in DTB or needed before DTB scan)
// ---------------------------------------------------------------------------

/// GICv3 distributor base address.
pub const GIC_DIST_BASE: usize = 0x0800_0000;

/// GICv3 redistributor base address.
pub const GIC_REDIST_BASE: usize = 0x080A_0000;

/// Kernel load address. — link.ld sync: `PHYS_BASE`
pub const KERNEL_BASE: usize = 0x4008_0000;

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
    match crate::firmware::dtb::scan(dtb_ptr) {
        Some(info) => {
            if info.ram_base != 0 {
                // The kernel binary is position-dependent (linked at
                // KERNEL_BASE). The MMU identity map assumes RAM starts at
                // the compiled default. A mismatch means the DTB describes
                // a different platform than the one we were compiled for —
                // the MMU tables would not cover the kernel, and enabling
                // the MMU would fault immediately.
                let expected = RAM_BASE_VAL.load(Ordering::Relaxed);

                if info.ram_base != expected {
                    panic!(
                        "dtb ram_base {:#x} != expected {:#x}",
                        info.ram_base, expected,
                    );
                }
            }
            if info.ram_size != 0 {
                RAM_SIZE_VAL.store(info.ram_size, Ordering::Relaxed);
            }
            if info.core_count != 0 {
                CORE_COUNT.store(info.core_count, Ordering::Relaxed);
            }

            crate::println!(
                "dtb: ram {:#x}+{:#x}, {} core(s)",
                info.ram_base,
                info.ram_size,
                info.core_count,
            );
        }
        None => {
            crate::println!("dtb: not found, using defaults");
        }
    }
}
