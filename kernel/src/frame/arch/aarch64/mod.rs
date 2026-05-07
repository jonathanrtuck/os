//! AArch64 architecture implementation.

#[cfg(target_os = "none")]
core::arch::global_asm!(include_str!("boot.S"));
#[cfg(target_os = "none")]
core::arch::global_asm!(include_str!("secondary_entry.S"));

pub mod context;
pub mod cpu;
pub mod idle;
pub use cpu::PerCpu;
#[cfg(target_os = "none")]
pub use cpu::{init_percpu_bsp, set_kernel_ptr};
pub mod entropy;
pub mod exception;
pub mod gic;
pub mod hvf_timing;
pub mod page_alloc;
pub mod page_table;
pub mod sync;
pub use gic as interrupts;
mod mmio;
pub mod mmu;
pub mod platform;
pub mod psci;
pub mod register_state;
pub mod serial;
mod sysreg;
pub mod timer;

/// Mask all maskable interrupts.
///
/// Prevents async hardware events (timer deadlines, device IRQs) from
/// interrupting the current execution.
pub fn disable_interrupts() {
    sysreg::disable_irqs();
}

/// Unmask all maskable interrupts.
///
/// Enables delivery of async hardware events. Call only after exception
/// vectors and interrupt controllers are initialized.
pub fn enable_interrupts() {
    sysreg::enable_irqs();
}

/// Print a register dump to the console for crash diagnostics.
///
/// Reads exception-related registers and the link register, printing
/// them for post-mortem debugging. ELR/SPSR/ESR reflect the most recent
/// exception (likely the last timer IRQ), not the panic site — the Rust
/// panic message has the precise source location.
pub fn dump_panic_registers() {
    let lr: u64;

    // SAFETY: Copies the link register (x30) into a general-purpose register.
    // Pure register-to-register move — no memory or system side effects. No
    // `nomem` because the project policy restricts it to an explicit approved
    // list (immutable `mrs`, hint instructions).
    unsafe { core::arch::asm!("mov {lr}, x30", lr = out(reg) lr, options(nostack)) };

    let elr = sysreg::elr_el1();
    let spsr = sysreg::spsr_el1();
    let esr = sysreg::esr_el1();

    crate::println!("  LR:   0x{lr:016x}");
    crate::println!("  ELR:  0x{elr:016x}");
    crate::println!("  SPSR: 0x{spsr:016x}");
    crate::println!("  ESR:  0x{esr:016x}");
}

/// Halt the CPU until an event or interrupt arrives.
#[inline(always)]
pub fn halt() {
    // SAFETY: `wfe` is a hint instruction with no side effects beyond pausing
    // the core until the next event (interrupt, SEV from another core, etc.).
    // It does not modify memory or registers.
    unsafe {
        core::arch::asm!("wfe", options(nomem, nostack));
    }
}

/// Read the virtual counter (CNTVCT_EL0) for timing measurements.
#[inline(always)]
pub fn read_cycle_counter() -> u64 {
    sysreg::cntvct_el0()
}

/// Pair of `nop` hint instructions. Used by the HVF timing sanity bench
/// to measure pure guest cycles with no traps or MMIO. Lives here (not in
/// `bench.rs`) because the framekernel rule forbids `unsafe` outside the
/// `frame/` module.
#[inline(always)]
pub fn nop_pair() {
    // SAFETY: `nop` is a hint instruction with no architectural side
    // effects on registers, memory, or the exception state. `nomem`
    // promises LLVM no memory access, which is true for nop.
    unsafe {
        core::arch::asm!("nop", "nop", options(nomem, nostack));
    }
}

/// Instruction Synchronization Barrier — serializes the pipeline.
#[inline(always)]
pub fn isb() {
    sysreg::isb();
}

/// Issue a null SVC for benchmarking trap overhead.
/// Sends syscall number 255 (invalid), guaranteeing a fast error return.
#[cfg(target_os = "none")]
#[inline(always)]
pub fn svc_null() -> (u64, u64) {
    let error: u64;
    let value: u64;

    // SAFETY: SVC #0 traps to EL1 via the installed exception vector.
    // x8=255 is an invalid syscall number, so the handler returns
    // immediately with an error. No memory side effects.
    unsafe {
        core::arch::asm!(
            "mov x8, #255",
            "svc #0",
            out("x0") error,
            out("x1") value,
            out("x8") _,
            options(nostack),
        );
    }

    (error, value)
}

/// Signal a fatal crash to the hypervisor via the pvpanic device.
///
/// Writes 0x01 to the pvpanic MMIO register, which tells QEMU/HVF that
/// the guest has panicked.
pub fn signal_panic() {
    mmio::write32(platform::device_addr(platform::PVPANIC_BASE), 1);
}
