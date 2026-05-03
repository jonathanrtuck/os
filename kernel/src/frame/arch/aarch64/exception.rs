//! Exception handling for AArch64.
//!
//! The assembly vector table (`exception.S`) saves full register context into a
//! [`TrapFrame`] on the stack and calls [`exception_handler`]. This module
//! decodes the exception, prints diagnostic output for fatal cases, and
//! returns for recoverable ones (e.g., IRQ).

#[cfg(target_os = "none")]
core::arch::global_asm!(include_str!("exception.S"));

use super::sysreg;

// ---------------------------------------------------------------------------
// TrapFrame — must match the assembly layout in exception.S exactly.
// ---------------------------------------------------------------------------

/// Saved CPU state at the point of an exception.
///
/// Created by the assembly vector entry, passed to [`exception_handler`] as a
/// stack pointer. 816 bytes, 16-byte aligned. Includes full FP/SIMD state
/// so that interrupts cannot corrupt the interrupted code's float registers.
#[repr(C)]
pub struct TrapFrame {
    /// General-purpose registers x0–x30.
    pub gprs: [u64; 31],
    /// Exception Link Register — address to return to.
    pub elr: u64,
    /// Saved Processor State Register — PSTATE before the exception.
    pub spsr: u64,
    /// Exception Syndrome Register — exception class and details.
    pub esr: u64,
    /// Fault Address Register — address that caused a data/instruction abort.
    pub far: u64,
    /// Padding for 16-byte alignment of FP register block. The assembly stores
    /// the source ID here temporarily, but it is passed to Rust via the
    /// `source` parameter.
    _pad: u64,
    /// FP/SIMD registers q0–q31 (128-bit each).
    pub fp_regs: [u128; 32],
    /// Floating-point control register.
    pub fpcr: u64,
    /// Floating-point status register.
    pub fpsr: u64,
}

// Offsets must match exception.S — the assembly uses hard-coded immediates for
// STP/LDP/STR/LDR. If any field is reordered, these assertions catch it at
// compile time rather than producing silent context corruption at runtime.
const _: () = {
    assert!(core::mem::offset_of!(TrapFrame, gprs) == 0);
    assert!(core::mem::offset_of!(TrapFrame, elr) == 248);
    assert!(core::mem::offset_of!(TrapFrame, spsr) == 256);
    assert!(core::mem::offset_of!(TrapFrame, esr) == 264);
    assert!(core::mem::offset_of!(TrapFrame, far) == 272);
    assert!(core::mem::offset_of!(TrapFrame, fp_regs) == 288);
    assert!(core::mem::offset_of!(TrapFrame, fpcr) == 800);
    assert!(core::mem::offset_of!(TrapFrame, fpsr) == 808);
    assert!(core::mem::size_of::<TrapFrame>() == 816); // sub sp, sp, #816
};

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Install the exception vector table by writing VBAR_EL1.
pub fn init() {
    unsafe extern "C" {
        static __vectors: u8;
    }

    // __vectors is the assembly vector table, 2KB-aligned by `.align 11`
    // in exception.S. The `unsafe extern` block above covers the access.
    let vbar = (&raw const __vectors) as u64;

    sysreg::set_vbar_el1(vbar);
    sysreg::isb();
}

// ---------------------------------------------------------------------------
// Exception handler entry point (called from assembly)
// ---------------------------------------------------------------------------

/// Main exception dispatch, called from the assembly common handler.
///
/// `source` identifies which of the 16 vector entries was taken (0–15).
/// The assembly performs full context save/restore around this call, so
/// returning normally resumes the interrupted code via `eret`.
#[unsafe(no_mangle)]
extern "C" fn exception_handler(frame: &mut TrapFrame, source: u64) {
    match source {
        // EL1h IRQ — timer deadlines and device interrupts.
        5 => irq_handler(frame),
        // EL0/64 Sync — syscalls (SVC) and faults from userspace.
        8 => el0_sync_handler(frame),
        // EL0/64 IRQ — device interrupt while running userspace code.
        // Same GIC path as EL1h IRQ; only the interrupted context differs.
        9 => irq_handler(frame),
        // Everything else is unhandled.
        _ => fatal_exception(frame, source),
    }
}

// ---------------------------------------------------------------------------
// IRQ handler
// ---------------------------------------------------------------------------

fn irq_handler(_frame: &mut TrapFrame) {
    let intid = super::gic::acknowledge();

    if intid == super::gic::INTID_SPURIOUS {
        return;
    }

    match intid {
        super::gic::INTID_VTIMER => {
            super::timer::handle_deadline();
        }
        // BUG: println! here will deadlock if this IRQ preempted a println!
        // on the same core (serial lock is not interrupt-aware). Acceptable
        // for now — unhandled IRQs during serial output are unlikely. Fix
        // when the serial driver gains interrupt-safe locking.
        _ => {
            crate::println!("IRQ: unhandled INTID {intid}");
        }
    }

    super::gic::end_of_interrupt(intid);
}

// ---------------------------------------------------------------------------
// EL0 sync handler — syscalls and userspace faults
// ---------------------------------------------------------------------------

/// Decode and dispatch synchronous exceptions from EL0 (userspace).
///
/// NOTE: SP_EL0 is not yet saved in the TrapFrame (tracked in exception.S
/// checklist). Adding SP_EL0 save/restore is required before any handler
/// here returns to EL0 rather than halting.
fn el0_sync_handler(frame: &mut TrapFrame) {
    let ec = (frame.esr >> 26) & 0x3F;

    match ec {
        // SVC — syscall entry point for the 25 syscalls in the kernel spec.
        0x15 => unimplemented_el0(frame, "SVC (syscall)"),
        // Data abort from EL0 — will handle COW faults (vmo_snapshot) and
        // pager requests (vmo_set_pager).
        0x24 => unimplemented_el0(frame, "data abort"),
        // Instruction abort from EL0.
        0x20 => unimplemented_el0(frame, "instruction abort"),
        _ => fatal_exception(frame, 8),
    }
}

fn unimplemented_el0(frame: &TrapFrame, kind: &str) -> ! {
    sysreg::disable_irqs();

    crate::println!();
    crate::println!("EL0 {kind} — not yet implemented");
    crate::println!("  ELR:  0x{:016x}", frame.elr);
    crate::println!("  ESR:  0x{:016x}", frame.esr);
    crate::println!("  FAR:  0x{:016x}", frame.far);
    crate::println!();

    super::signal_panic();

    loop {
        crate::frame::arch::halt();
    }
}

// ---------------------------------------------------------------------------
// Fatal exception — dump state and halt
// ---------------------------------------------------------------------------

fn fatal_exception(frame: &TrapFrame, source: u64) -> ! {
    // Mask IRQs to prevent timer ticks from interleaving diagnostic output.
    sysreg::disable_irqs();

    let ec = (frame.esr >> 26) & 0x3F;

    crate::println!();
    crate::println!(
        "EXCEPTION: {} — {} (EC 0x{ec:02x})",
        source_name(source),
        ec_name(ec),
    );
    crate::println!("  ELR:  0x{:016x}", frame.elr);
    crate::println!("  ESR:  0x{:016x}", frame.esr);
    crate::println!("  FAR:  0x{:016x}", frame.far);
    crate::println!("  SPSR: 0x{:016x}", frame.spsr);
    crate::println!();

    // Print GPRs, two per line.
    for i in (0..31).step_by(2) {
        if i + 1 < 31 {
            crate::println!(
                "  x{i:<2} = 0x{:016x}  x{:<2} = 0x{:016x}",
                frame.gprs[i],
                i + 1,
                frame.gprs[i + 1],
            );
        } else {
            crate::println!("  x{i:<2} = 0x{:016x}", frame.gprs[i]);
        }
    }

    crate::println!();

    // Signal the hypervisor so it knows the kernel crashed (same as panic).
    super::signal_panic();

    loop {
        crate::frame::arch::halt();
    }
}

// ---------------------------------------------------------------------------
// ESR exception class decoding
// ---------------------------------------------------------------------------

fn ec_name(ec: u64) -> &'static str {
    match ec {
        0x00 => "Unknown",
        0x01 => "WFI/WFE trap",
        0x0E => "Illegal execution state",
        0x15 => "SVC (AArch64)",
        0x18 => "MSR/MRS trap",
        0x20 => "Instruction abort (lower EL)",
        0x21 => "Instruction abort (same EL)",
        0x22 => "PC alignment fault",
        0x24 => "Data abort (lower EL)",
        0x25 => "Data abort (same EL)",
        0x26 => "SP alignment fault",
        0x2C => "FP/SIMD exception",
        0x2F => "SError",
        0x30 => "Breakpoint (lower EL)",
        0x31 => "Breakpoint (same EL)",
        0x32 => "Software step (lower EL)",
        0x33 => "Software step (same EL)",
        0x34 => "Watchpoint (lower EL)",
        0x35 => "Watchpoint (same EL)",
        0x3C => "BRK (AArch64)",
        _ => "Reserved",
    }
}

fn source_name(source: u64) -> &'static str {
    match source {
        0 => "EL1t Sync",
        1 => "EL1t IRQ",
        2 => "EL1t FIQ",
        3 => "EL1t SError",
        4 => "EL1h Sync",
        5 => "EL1h IRQ",
        6 => "EL1h FIQ",
        7 => "EL1h SError",
        8 => "EL0/64 Sync",
        9 => "EL0/64 IRQ",
        10 => "EL0/64 FIQ",
        11 => "EL0/64 SError",
        12 => "EL0/32 Sync",
        13 => "EL0/32 IRQ",
        14 => "EL0/32 FIQ",
        15 => "EL0/32 SError",
        _ => "Unknown",
    }
}
