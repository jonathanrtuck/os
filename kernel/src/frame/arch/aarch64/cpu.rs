//! Per-core management: ID extraction, stack allocation, secondary core init.
//!
//! Pure functions in this module are host-testable. Hardware-dependent
//! functions are gated behind `#[cfg(target_os = "none")]`.

#[cfg(target_os = "none")]
use core::cell::UnsafeCell;
#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(target_os = "none")]
use super::{platform, sysreg};
use crate::config;

// ---------------------------------------------------------------------------
// Per-core stacks
// ---------------------------------------------------------------------------

/// Stack storage for all cores, 16-byte aligned per AArch64 ABI.
///
/// `UnsafeCell` forces the compiler to place this in `.bss` (writable)
/// rather than `.rodata`. Each core exclusively owns its slot.
///
/// Slot 0 is unused — the BSP uses the linker-script stack from `.bss.stack`.
/// Switching the BSP mid-boot would require an assembly trampoline; 64 KiB of
/// .bss is a cheaper trade than that complexity.
#[cfg(target_os = "none")]
#[repr(C, align(16))]
struct CoreStacks(UnsafeCell<[[u8; config::KERNEL_STACK_SIZE]; config::MAX_CORES]>);

// SAFETY: Each core exclusively owns its stack slot, indexed by core_id.
// No cross-core stack access occurs. The array is written only during
// early boot (stack pointer setup in assembly) and used exclusively by
// its owning core thereafter.
#[cfg(target_os = "none")]
unsafe impl Sync for CoreStacks {}

#[cfg(target_os = "none")]
static CORE_STACKS: CoreStacks = CoreStacks(UnsafeCell::new(
    [[0; config::KERNEL_STACK_SIZE]; config::MAX_CORES],
));

/// Number of secondary cores that have completed initialization.
#[cfg(target_os = "none")]
static CORES_ONLINE: AtomicUsize = AtomicUsize::new(0);

/// Extract the linear core ID from an MPIDR_EL1 value.
///
/// On QEMU virt and Apple HVF, Aff0 (bits [7:0]) gives a linear core index.
/// Higher affinity levels are zero for the first 256 cores.
pub fn core_id_from_mpidr(mpidr: u64) -> usize {
    (mpidr & 0xFF) as usize
}

/// Compute the stack top address for a given core.
///
/// Stacks grow downward on AArch64, so the "top" is the highest address.
/// Returns `stacks_base + (core_id + 1) * KERNEL_STACK_SIZE`.
///
/// Panics if `core_id >= MAX_CORES`.
pub fn stack_top_for_core(core_id: usize, stacks_base: usize) -> usize {
    assert!(core_id < config::MAX_CORES);

    stacks_base + (core_id + 1) * config::KERNEL_STACK_SIZE
}

// ---------------------------------------------------------------------------
// Secondary core lifecycle (bare-metal only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "none")]
unsafe extern "C" {
    fn __secondary_entry();
}

/// Activate all secondary cores discovered in the DTB.
///
/// Issues PSCI CPU_ON for each core (1..core_count), passing the per-core
/// stack top as context_id. Blocks until all secondaries report online.
#[cfg(target_os = "none")]
pub fn activate_secondaries() {
    let count = platform::core_count().min(config::MAX_CORES);

    if count <= 1 {
        return;
    }

    let entry = __secondary_entry as *const () as usize as u64;
    let stacks_base = CORE_STACKS.0.get() as usize;

    for core_id in 1..count {
        let stack_top = stack_top_for_core(core_id, stacks_base) as u64;
        let target_mpidr = core_id as u64;

        if let Err(code) = super::psci::cpu_on(target_mpidr, entry, stack_top) {
            crate::println!(
                "psci: CPU_ON core {} failed: {} ({})",
                core_id,
                code,
                super::psci::error_name(code),
            );
        }
    }

    let deadline = sysreg::cntpct_el0() + sysreg::cntfrq_el0() / 2; // 500ms

    while CORES_ONLINE.load(Ordering::Acquire) < count - 1 {
        core::hint::spin_loop();

        if sysreg::cntpct_el0() >= deadline {
            let online = CORES_ONLINE.load(Ordering::Acquire) + 1;

            crate::println!(
                "cpu: timeout waiting for secondaries ({}/{} online)",
                online,
                count
            );

            break;
        }
    }

    let online = CORES_ONLINE.load(Ordering::Acquire) + 1;

    crate::println!("{}/{} cores online", online, count);
}

/// Rust entry point for secondary cores, called from secondary_entry.S.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
extern "C" fn secondary_main(core_id: usize) -> ! {
    super::exception::init();
    super::mmu::init_secondary();
    super::gic::init_per_core(core_id);
    super::sysreg::set_tpidr_el1(core_id as u64);

    crate::println!("core {}: alive", core_id);

    CORES_ONLINE.fetch_add(1, Ordering::Release);

    loop {
        super::halt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- core_id_from_mpidr --

    #[test]
    fn mpidr_core_0_up_bit_set() {
        // Bit 31 (UP) is set on single-core or typical QEMU/HVF configs.
        assert_eq!(core_id_from_mpidr(0x8000_0000), 0);
    }

    #[test]
    fn mpidr_core_0_up_bit_clear() {
        assert_eq!(core_id_from_mpidr(0x0000_0000), 0);
    }

    #[test]
    fn mpidr_core_3() {
        assert_eq!(core_id_from_mpidr(0x8000_0003), 3);
    }

    #[test]
    fn mpidr_core_7() {
        assert_eq!(core_id_from_mpidr(0x8000_0007), 7);
    }

    #[test]
    fn mpidr_ignores_higher_affinity_levels() {
        // Aff1 = 1, Aff0 = 2 — we extract only Aff0.
        assert_eq!(core_id_from_mpidr(0x8000_0102), 2);
    }

    #[test]
    fn mpidr_max_aff0() {
        assert_eq!(core_id_from_mpidr(0x0000_00FF), 255);
    }

    // -- stack_top_for_core --

    #[test]
    fn stack_top_core_0() {
        let base = 0x1000_0000;

        assert_eq!(
            stack_top_for_core(0, base),
            base + config::KERNEL_STACK_SIZE,
        );
    }

    #[test]
    fn stack_top_core_7() {
        let base = 0x1000_0000;

        assert_eq!(
            stack_top_for_core(7, base),
            base + 8 * config::KERNEL_STACK_SIZE,
        );
    }

    #[test]
    fn stack_tops_are_contiguous_and_non_overlapping() {
        let base = 0x1000_0000;

        for core_id in 0..config::MAX_CORES {
            let top = stack_top_for_core(core_id, base);
            let bottom = top - config::KERNEL_STACK_SIZE;

            assert_eq!(bottom, base + core_id * config::KERNEL_STACK_SIZE);
        }
    }

    #[test]
    fn stack_top_is_16_byte_aligned() {
        let base = 0x1000_0000;

        for core_id in 0..config::MAX_CORES {
            assert_eq!(stack_top_for_core(core_id, base) % 16, 0);
        }
    }

    #[test]
    #[should_panic]
    fn stack_top_panics_on_invalid_core() {
        stack_top_for_core(config::MAX_CORES, 0x1000_0000);
    }
}
