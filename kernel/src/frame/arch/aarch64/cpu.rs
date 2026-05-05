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

// ---------------------------------------------------------------------------
// Per-CPU data — accessed via TPIDR_EL1 for O(1) "who am I?" queries.
// ---------------------------------------------------------------------------

/// Per-CPU data stored at the address in TPIDR_EL1.
///
/// 128-byte aligned to own one M4 Pro cache line. Accessed from
/// exception handlers via MRS TPIDR_EL1 (2 cycles).
#[derive(Clone, Copy)]
#[repr(C, align(128))]
pub struct PerCpu {
    pub core_id: u32,
    pub current_thread: u32,
    pub kernel_ptr: usize,
    pub reschedule_pending: u32,
    _pad0: u32,
    pub last_syscall_entry: u64,
    _pad: [u8; 96],
}

impl PerCpu {
    pub const IDLE: u32 = u32::MAX;

    pub const fn new(core_id: u32) -> Self {
        PerCpu {
            core_id,
            current_thread: Self::IDLE,
            kernel_ptr: 0,
            reschedule_pending: 0,
            _pad0: 0,
            last_syscall_entry: 0,
            _pad: [0; 96],
        }
    }

    pub fn mark_syscall_entry(&mut self) {
        self.last_syscall_entry = super::sysreg::cntvct_el0();
    }

    pub fn clear_syscall_entry(&mut self) {
        self.last_syscall_entry = 0;
    }
}

const _: () = {
    assert!(core::mem::size_of::<PerCpu>() == 128);
};

#[cfg(target_os = "none")]
struct PerCpuArray(UnsafeCell<[PerCpu; config::MAX_CORES]>);

// SAFETY: Each core exclusively owns its PerCpu slot, indexed by core_id.
// Cross-core writes only happen during single-threaded boot (set_kernel_ptr).
#[cfg(target_os = "none")]
unsafe impl Sync for PerCpuArray {}

#[cfg(target_os = "none")]
static PER_CPU_DATA: PerCpuArray = PerCpuArray(UnsafeCell::new({
    let mut arr = [PerCpu::new(0); config::MAX_CORES];
    let mut i = 0;

    while i < config::MAX_CORES {
        arr[i] = PerCpu::new(i as u32);
        i += 1;
    }

    arr
}));

/// Initialize per-CPU data for core 0 (BSP). Call early in kernel_main,
/// before any exception can fire.
#[cfg(target_os = "none")]
pub fn init_percpu_bsp() {
    // SAFETY: PER_CPU_DATA is initialized at compile time. We're accessing
    // slot 0 to get its address for TPIDR_EL1. No concurrent access during boot.
    let ptr = unsafe { &(*PER_CPU_DATA.0.get())[0] as *const PerCpu as u64 };

    sysreg::set_tpidr_el1(ptr);
}

/// Re-set TPIDR_EL1 to the upper-half VA after MMU enable.
///
/// Must be called from the upper-half VA context. At that point, adrp
/// resolves PER_CPU_DATA to its upper-half VA directly.
#[cfg(target_os = "none")]
pub fn reinit_percpu_bsp() {
    // SAFETY: PER_CPU_DATA is initialized at compile time. Slot 0 belongs to
    // the BSP. No concurrent access — secondaries haven't started yet.
    let ptr = unsafe { &(*PER_CPU_DATA.0.get())[0] as *const PerCpu as u64 };

    sysreg::set_tpidr_el1(ptr);
}

/// Initialize per-CPU data for a secondary core. Called from secondary_main.
#[cfg(target_os = "none")]
pub fn init_percpu(core_id: usize) {
    // SAFETY: core_id is validated by the caller (secondary_main). Each core
    // initializes only its own slot.
    let ptr = unsafe { &(*PER_CPU_DATA.0.get())[core_id] as *const PerCpu as u64 };

    sysreg::set_tpidr_el1(ptr);
}

/// Read this core's PerCpu data from TPIDR_EL1.
///
/// # Safety
/// Must only be called after init_percpu_bsp/init_percpu has been called
/// on this core.
#[cfg(target_os = "none")]
pub unsafe fn percpu() -> &'static PerCpu {
    let ptr = sysreg::tpidr_el1() as *const PerCpu;

    // SAFETY: TPIDR_EL1 was set to point to this core's PerCpu slot
    // during boot. The slot lives in a static with 'static lifetime.
    unsafe { &*ptr }
}

/// Read this core's PerCpu data mutably.
///
/// # Safety
/// Must only be called after init_percpu. Caller must ensure no concurrent
/// mutation (IRQs disabled or single-threaded context).
#[cfg(target_os = "none")]
pub unsafe fn percpu_mut() -> &'static mut PerCpu {
    let ptr = sysreg::tpidr_el1() as *mut PerCpu;

    // SAFETY: TPIDR_EL1 points to this core's exclusive slot. Caller
    // guarantees no concurrent access.
    unsafe { &mut *ptr }
}

/// Store the Kernel pointer in all per-CPU data slots.
///
/// Called once during boot, before secondary cores start.
#[cfg(target_os = "none")]
pub fn set_kernel_ptr(ptr: *mut u8) {
    // SAFETY: Called during single-threaded boot, before secondaries.
    // UnsafeCell permits interior mutation.
    let data = unsafe { &mut *PER_CPU_DATA.0.get() };

    for slot in data.iter_mut() {
        slot.kernel_ptr = ptr as usize;
    }
}

/// Set the current thread ID on this core's PerCpu data.
///
/// Called during boot to set the init thread as current before
/// entering userspace.
#[cfg(target_os = "none")]
pub fn set_current_thread(thread_id: u32) {
    // SAFETY: percpu_mut requires init_percpu to have been called.
    // Single-threaded context during boot.
    unsafe {
        percpu_mut().current_thread = thread_id;
    }
}

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

// SAFETY: __secondary_entry is the assembly entry stub in secondary_entry.S.
// We only take its address to pass to PSCI CPU_ON as the entry point.
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

    // PSCI CPU_ON needs physical addresses. Since the BSP now runs at
    // upper-half VAs, adrp gives VAs — convert to PAs.
    let entry = platform::virt_to_phys(__secondary_entry as *const () as usize) as u64;
    let stacks_base = platform::virt_to_phys(CORE_STACKS.0.get() as usize);

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

    let raw = CORES_ONLINE.load(Ordering::Acquire);
    let expected = count - 1;

    if raw != expected {
        crate::println!(
            "cpu: CORES_ONLINE anomaly: got {:#x}, expected {} (bug #21)",
            raw,
            expected,
        );
    }

    crate::println!("{}/{} cores online", raw + 1, count);
}

/// Phase 1: secondary core boot at physical address. Sets up exception
/// vectors and enables the MMU, which branches to secondary_main_upper.
///
/// Called from secondary_entry.S via `bl secondary_main`.
#[cfg(target_os = "none")]
// SAFETY: no_mangle is required so secondary_entry.S can call this symbol.
// The function signature matches the assembly calling convention (x0 = core_id).
#[unsafe(no_mangle)]
extern "C" fn secondary_main(core_id: usize) -> ! {
    super::exception::init();

    init_percpu(core_id);

    // init_secondary enables MMU and branches to secondary_main_upper
    // at the upper-half VA. Never returns.
    super::mmu::init_secondary();

    unreachable!();
}

/// Phase 2: secondary core boot at upper-half VA.
///
/// Branched to by the MMU trampoline in configure_and_enable.
#[cfg(target_os = "none")]
// SAFETY: no_mangle is required so mmu.rs can take its address via
// `unsafe extern "C" { fn secondary_main_upper(); }`.
#[unsafe(no_mangle)]
extern "C" fn secondary_main_upper() -> ! {
    super::exception::reinit_vbar();

    let core_id = super::sysreg::mpidr_el1() as usize & 0xFF;

    reinit_percpu(core_id);

    super::gic::init_per_core(core_id);

    crate::println!("core {}: alive", core_id);

    CORES_ONLINE.fetch_add(1, Ordering::Release);

    loop {
        super::halt();
    }
}

#[cfg(target_os = "none")]
fn reinit_percpu(core_id: usize) {
    // SAFETY: core_id was extracted from MPIDR_EL1 by the caller. Each
    // core reinitializes only its own slot. Called after MMU enable, so the
    // address resolves to the upper-half VA.
    let ptr = unsafe { &(*PER_CPU_DATA.0.get())[core_id] as *const PerCpu as u64 };

    sysreg::set_tpidr_el1(ptr);
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- PerCpu layout --

    #[test]
    fn percpu_is_one_cache_line() {
        assert_eq!(core::mem::size_of::<PerCpu>(), 128);
        assert_eq!(core::mem::align_of::<PerCpu>(), 128);
    }

    #[test]
    fn percpu_idle_sentinel() {
        let p = PerCpu::new(3);

        assert_eq!(p.core_id, 3);
        assert_eq!(p.current_thread, PerCpu::IDLE);
        assert_eq!(p.kernel_ptr, 0);
        assert_eq!(p.reschedule_pending, 0);
    }

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
