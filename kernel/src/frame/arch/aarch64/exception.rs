//! Exception handling for AArch64.
//!
//! The assembly vector table (`exception.S`) saves full register context into a
//! [`TrapFrame`] on the stack and calls [`exception_handler`]. This module
//! decodes the exception, prints diagnostic output for fatal cases, and
//! returns for recoverable ones (e.g., IRQ).

#[cfg(all(target_os = "none", not(feature = "profile")))]
core::arch::global_asm!(include_str!("exception.S"));
#[cfg(all(target_os = "none", feature = "profile"))]
core::arch::global_asm!(concat!(".set PROFILE, 1\n", include_str!("exception.S")));

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::sysreg;

/// Per-core fault recovery address for LDTR/STTR user memory access.
/// When non-zero, an EL1 data abort during copy_from_user/copy_to_user
/// jumps to this address instead of panicking. Cleared after the copy
/// completes (success or fault).
///
/// Indexed by core_id. Must be per-core: concurrent user memory copies on
/// different cores must not clobber each other's recovery addresses.
pub static COPY_FAULT_RECOVERY: [AtomicU64; crate::config::MAX_CORES] =
    [const { AtomicU64::new(0) }; crate::config::MAX_CORES];

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
    /// User stack pointer (SP_EL0) — saved on exception from EL0.
    pub sp_el0: u64,
    /// Whether FP/SIMD registers were saved at exception entry.
    /// Set by assembly: non-zero if saved, zero if skipped (lazy FP trap).
    /// The restore path checks this instead of re-reading CPACR, which
    /// handle_fp_trap may have changed mid-exception.
    pub fp_saved: u64,
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
    assert!(core::mem::offset_of!(TrapFrame, sp_el0) == 816);
    assert!(core::mem::offset_of!(TrapFrame, fp_saved) == 824);
    assert!(core::mem::size_of::<TrapFrame>() == 832); // sub sp, sp, #832
};

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Install the exception vector table by writing VBAR_EL1.
///
/// Before MMU enable, adrp resolves __vectors to a physical address.
/// After MMU enable, call [`reinit_vbar`] to update VBAR to the
/// upper-half VA so exceptions resolve correctly when TTBR0 is
/// switched to a user page table.
pub fn init() {
    // SAFETY: __vectors is the 2KB-aligned vector table defined in
    // exception.S. We only take its address for VBAR_EL1.
    unsafe extern "C" {
        static __vectors: u8;
    }

    let vbar = (&raw const __vectors) as u64;

    sysreg::set_vbar_el1(vbar);
    sysreg::isb();
}

/// Re-set VBAR_EL1 to the upper-half VA after MMU enable.
///
/// Must be called from the upper-half VA context (after the MMU
/// trampoline branches to kernel_main_upper). At that point, adrp
/// resolves __vectors to the upper-half VA directly.
pub fn reinit_vbar() {
    // SAFETY: Same symbol as init(). Address taken, never dereferenced.
    unsafe extern "C" {
        static __vectors: u8;
    }

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
// SAFETY: no_mangle is required so exception.S can call this symbol via `bl`.
// The ABI matches: x0 = &mut TrapFrame (sp), x1 = source ID.
#[unsafe(no_mangle)]
extern "C" fn exception_handler(frame: &mut TrapFrame, source: u64) {
    match source {
        // EL1h Sync — SVC from kernel mode (benchmarks) or unexpected fault.
        4 => el1_sync_handler(frame),
        // EL1h IRQ — timer deadlines and device interrupts.
        5 => irq_handler(frame),
        // EL0/64 Sync — syscalls (SVC) and faults from userspace.
        8 => el0_sync_handler(frame),
        // EL0/64 IRQ — device interrupt while running userspace code.
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
        // SGI 0 — reschedule IPI from another core.
        super::gic::SGI_RESCHEDULE => {
            #[cfg(target_os = "none")]
            // SAFETY: percpu_mut() requires init_percpu to have been called.
            // IRQ handlers only fire after boot completes.
            unsafe {
                super::cpu::percpu_mut().reschedule_pending = 1;
            }
        }
        // SGIs 1–15 — reserved, ignore.
        1..=15 => {}
        super::gic::INTID_VTIMER => {
            #[cfg(target_os = "none")]
            // SAFETY: percpu() requires init_percpu to have been called.
            // IRQ handlers only fire after boot completes.
            let core = unsafe { super::cpu::percpu().core_id as usize };
            #[cfg(not(target_os = "none"))]
            let core = 0;

            super::timer::handle_deadline(core);

            for entry in super::timer::drain_expired(core) {
                let Some(tid) = entry else { break };

                if let Some(mut t) = crate::frame::state::threads().write(tid.0)
                    && t.state() == crate::thread::ThreadRunState::Blocked
                {
                    t.set_wakeup_error(crate::types::SyscallError::TimedOut);

                    drop(t);

                    crate::sched::wake(tid, core);
                }
            }

            #[cfg(all(debug_assertions, target_os = "none"))]
            // SAFETY: percpu() valid — IRQ handlers fire after boot.
            unsafe {
                watchdog_check(super::cpu::percpu());
            }
        }
        32.. => {
            let handler_addr = DEVICE_IRQ_HANDLER.load(Ordering::Acquire);

            if handler_addr != 0 {
                // SAFETY: set_device_irq_handler stores a valid fn pointer.
                let handler: fn(u32) = unsafe { core::mem::transmute(handler_addr) };

                handler(intid);

                super::gic::mask_spi(intid);
            } else {
                crate::println!("IRQ: unhandled device INTID {intid}");
            }
        }
        _ => {
            crate::println!("IRQ: unhandled INTID {intid}");
        }
    }

    super::gic::end_of_interrupt(intid);
}

// ---------------------------------------------------------------------------
// Watchdog — detects syscalls that take longer than the threshold
// ---------------------------------------------------------------------------

#[cfg(all(debug_assertions, target_os = "none"))]
fn watchdog_check(pc: &super::cpu::PerCpu) {
    const WATCHDOG_THRESHOLD_TICKS: u64 = 10_000_000;
    let entry = pc.last_syscall_entry;

    if entry == 0 {
        return;
    }

    let now = super::sysreg::cntvct_el0();
    let elapsed = now.wrapping_sub(entry);

    if elapsed > WATCHDOG_THRESHOLD_TICKS {
        let freq = super::sysreg::cntfrq_el0();
        let elapsed_us = (elapsed * 1_000_000).checked_div(freq).unwrap_or(elapsed);

        crate::println!(
            "WATCHDOG: core {} syscall stalled for {}us ({} ticks, thread {})",
            pc.core_id,
            elapsed_us,
            elapsed,
            pc.current_thread,
        );

        panic!("kernel watchdog: syscall exceeded time threshold");
    }
}

// ---------------------------------------------------------------------------
// Registerable syscall and fault handlers
// ---------------------------------------------------------------------------

/// Syscall handler function pointer.
static SYSCALL_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Fault handler function pointer.
static FAULT_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Device IRQ handler function pointer (SPI, INTID >= 32).
static DEVICE_IRQ_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Register the syscall dispatch function. Called once during kernel init.
pub fn set_syscall_handler(handler: fn(u64, &[u64; 6]) -> (u64, u64)) {
    SYSCALL_HANDLER.store(handler as usize, Ordering::Release);
}

/// Register the fault dispatch function. Called once during kernel init.
pub fn set_fault_handler(handler: fn(u64, bool, u64) -> FaultAction) {
    FAULT_HANDLER.store(handler as usize, Ordering::Release);
}

/// Register the device IRQ dispatch function. Called once during kernel init.
/// The handler receives the INTID (>= 32) and performs binding lookup + event
/// signaling. The exception handler masks the IRQ at the GIC redistributor
/// after calling this, and `irq_ack` unmasks it.
pub fn set_device_irq_handler(handler: fn(u32)) {
    DEVICE_IRQ_HANDLER.store(handler as usize, Ordering::Release);
}

/// Result of handling a data abort from EL0.
pub enum FaultAction {
    /// Fault resolved (e.g., COW copy completed). Return to EL0 and retry.
    Resolved,
    /// Unrecoverable fault — kill the thread.
    Kill,
}

/// Register fault and IRQ dispatch functions. Must be called after the
/// kernel has transitioned to upper-half VAs so function pointers resolve
/// to TTBR1 addresses.
#[cfg(target_os = "none")]
pub fn register_handlers() {
    set_fault_handler(fault_dispatch);
    set_device_irq_handler(device_irq_dispatch);
    #[cfg(target_os = "none")]
    set_syscall_handler(syscall_slow_dispatch);
}

#[cfg(target_os = "none")]
fn syscall_slow_dispatch(syscall_num: u64, args: &[u64; 6]) -> (u64, u64) {
    // SAFETY: percpu_mut() valid — syscalls only arrive after boot.
    let (current_thread, space_id, core_id) = unsafe {
        let pc = super::cpu::percpu_mut();

        pc.mark_syscall_entry();

        let space = if pc.current_space == super::cpu::PerCpu::NO_SPACE {
            None
        } else {
            Some(crate::types::AddressSpaceId(pc.current_space))
        };

        (
            crate::types::ThreadId(pc.current_thread),
            space,
            pc.core_id as usize,
        )
    };
    let result = crate::syscall::dispatch(current_thread, space_id, core_id, syscall_num, args);

    // SAFETY: percpu_mut() valid — same lifetime as above.
    unsafe {
        super::cpu::percpu_mut().clear_syscall_entry();
    }

    result
}

fn device_irq_dispatch(intid: u32) {
    let signal = crate::frame::state::irqs().lock().handle_irq(intid);

    if let Some(sig) = signal {
        let core_id = {
            #[cfg(target_os = "none")]
            // SAFETY: percpu() valid — IRQ handlers fire after boot.
            unsafe {
                super::cpu::percpu().core_id as usize
            }
            #[cfg(not(target_os = "none"))]
            0
        };

        let woken = crate::frame::state::events()
            .write(sig.event_id.0)
            .map(|mut evt| evt.signal(sig.signal_bits));

        if let Some(woken) = woken {
            for info in woken.as_slice() {
                crate::sched::wake(info.thread_id, core_id);
            }
        }
    }
}

#[cfg(target_os = "none")]
fn fault_dispatch(fault_addr: u64, is_write: bool, _esr: u64) -> FaultAction {
    // SAFETY: percpu() requires init_percpu to have been called.
    let current = unsafe {
        let pc = super::cpu::percpu();

        crate::types::ThreadId(pc.current_thread)
    };

    match crate::fault::handle_data_abort(current, fault_addr as usize, is_write) {
        crate::fault::FaultAction::Resolved => FaultAction::Resolved,
        crate::fault::FaultAction::Kill => FaultAction::Kill,
    }
}

// ---------------------------------------------------------------------------
// SVC fast path handler (called from minimal-save assembly entry)
// ---------------------------------------------------------------------------

/// SVC fast path handler — called directly from the minimal-save assembly.
///
/// Arguments arrive in the AArch64 calling convention positions:
/// x0-x5 = syscall args, x6 = syscall number.
/// Returns (error, value) in x0-x1.
#[cfg(target_os = "none")]
// SAFETY: no_mangle is required so exception.S can call this symbol via `bl`.
// The ABI matches the assembly: x0-x5 = args, x6 = syscall number.
#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
extern "C" fn svc_fast_handler(
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    syscall_num: u64,
) -> (u64, u64) {
    crate::frame::profile::stamp(crate::frame::profile::slot::HANDLER_ENTRY);

    let args = [a0, a1, a2, a3, a4, a5];
    // SAFETY: percpu_mut() requires init_percpu_bsp to have been called.
    let (current_thread, space_id, core_id) = unsafe {
        let pc = super::cpu::percpu_mut();

        pc.mark_syscall_entry();

        let space = if pc.current_space == super::cpu::PerCpu::NO_SPACE {
            None
        } else {
            Some(crate::types::AddressSpaceId(pc.current_space))
        };

        (
            crate::types::ThreadId(pc.current_thread),
            space,
            pc.core_id as usize,
        )
    };

    crate::frame::profile::stamp(crate::frame::profile::slot::HANDLER_PERCPU_DONE);

    let result = crate::syscall::dispatch(current_thread, space_id, core_id, syscall_num, &args);

    // SAFETY: same as above — percpu is valid for this core's lifetime.
    unsafe {
        super::cpu::percpu_mut().clear_syscall_entry();
    }

    crate::frame::profile::stamp(crate::frame::profile::slot::HANDLER_EXIT);

    result
}

// ---------------------------------------------------------------------------
// EL1 sync handler — SVC from kernel mode (benchmarks)
// ---------------------------------------------------------------------------

fn el1_sync_handler(frame: &mut TrapFrame) {
    let ec = (frame.esr >> 26) & 0x3F;

    match ec {
        // Data abort from same EL — LDTR/STTR fault during user memory copy.
        0x25 => {
            #[cfg(target_os = "none")]
            // SAFETY: percpu() requires init_percpu to have been called.
            // EL1 sync handlers only fire after boot completes.
            let core = unsafe { super::cpu::percpu().core_id as usize };
            #[cfg(not(target_os = "none"))]
            let core = 0;
            let recovery = COPY_FAULT_RECOVERY[core].swap(0, Ordering::Relaxed);

            if recovery != 0 {
                frame.elr = recovery;
                frame.gprs[0] = 1; // Signal fault to the copy function via x0.

                return;
            }

            fatal_exception(frame, 4);
        }
        // SVC from EL1 — kernel benchmarks use this to measure trap overhead.
        0x15 => {
            #[cfg(target_os = "none")]
            {
                crate::frame::profile::stamp(crate::frame::profile::slot::HANDLER_ENTRY);

                let syscall_num = frame.gprs[8];
                let args: [u64; 6] = [
                    frame.gprs[0],
                    frame.gprs[1],
                    frame.gprs[2],
                    frame.gprs[3],
                    frame.gprs[4],
                    frame.gprs[5],
                ];
                // SAFETY: percpu() requires init_percpu_bsp to have been called.
                let (current, space_id, core_id) = unsafe {
                    let pc = super::cpu::percpu();
                    let space = if pc.current_space == super::cpu::PerCpu::NO_SPACE {
                        None
                    } else {
                        Some(crate::types::AddressSpaceId(pc.current_space))
                    };

                    (
                        crate::types::ThreadId(pc.current_thread),
                        space,
                        pc.core_id as usize,
                    )
                };

                crate::frame::profile::stamp(crate::frame::profile::slot::HANDLER_PERCPU_DONE);

                let (error, value) =
                    crate::syscall::dispatch(current, space_id, core_id, syscall_num, &args);

                crate::frame::profile::stamp(crate::frame::profile::slot::HANDLER_EXIT);

                frame.gprs[0] = error;
                frame.gprs[1] = value;
            }
            #[cfg(not(target_os = "none"))]
            {
                frame.gprs[0] = crate::types::SyscallError::InvalidArgument as u64;
                frame.gprs[1] = 0;
            }
        }
        _ => fatal_exception(frame, 4),
    }
}

// ---------------------------------------------------------------------------
// EL0 sync handler — syscalls and userspace faults
// ---------------------------------------------------------------------------

/// Decode and dispatch synchronous exceptions from EL0 (userspace).
fn el0_sync_handler(frame: &mut TrapFrame) {
    let ec = (frame.esr >> 26) & 0x3F;

    match ec {
        // SVC — syscall entry. NO global lock. Each handler acquires
        // only the per-object locks it needs (Tier 0/1/2 per the
        // synchronization model).
        0x15 => {
            let syscall_num = frame.gprs[8]; // x8 = syscall number
            let args: [u64; 6] = [
                frame.gprs[0],
                frame.gprs[1],
                frame.gprs[2],
                frame.gprs[3],
                frame.gprs[4],
                frame.gprs[5],
            ];
            let handler_addr = SYSCALL_HANDLER.load(Ordering::Acquire);

            if handler_addr == 0 {
                unimplemented_el0(frame, "SVC (no handler registered)");
            }

            // SAFETY: set_syscall_handler stores a valid fn pointer.
            let handler: fn(u64, &[u64; 6]) -> (u64, u64) =
                unsafe { core::mem::transmute(handler_addr) };

            let (error, value) = handler(syscall_num, &args);

            frame.gprs[0] = error; // x0 = error code
            frame.gprs[1] = value; // x1 = return value
        }
        // FP/SIMD trap — lazy FP state switch.
        #[cfg(target_os = "none")]
        0x07 => handle_fp_trap(),
        // Data abort from EL0.
        0x24 => handle_data_abort(frame),
        // Instruction abort from EL0.
        0x20 => handle_instruction_abort(frame),
        _ => fatal_exception(frame, 8),
    }
}

// ---------------------------------------------------------------------------
// Lazy FP/SIMD save
// ---------------------------------------------------------------------------

/// Per-core FP owner tracking. Stores the ThreadId (as u32) of the thread
/// whose FP state is currently live in the FP registers on this core.
/// u32::MAX means no owner (FP registers are don't-care).
#[cfg(target_os = "none")]
static FP_OWNER: [core::sync::atomic::AtomicU32; crate::config::MAX_CORES] =
    [const { core::sync::atomic::AtomicU32::new(super::cpu::PerCpu::IDLE) };
        crate::config::MAX_CORES];

#[cfg(target_os = "none")]
fn handle_fp_trap() {
    // SAFETY: percpu() requires init_percpu to have been called. FP traps
    // only occur from EL0, which is entered well after boot.
    let (core_id, current_tid) = unsafe {
        let pc = super::cpu::percpu();

        (pc.core_id as usize, pc.current_thread)
    };
    let prev_owner = FP_OWNER[core_id].swap(current_tid, core::sync::atomic::Ordering::Relaxed);

    if prev_owner != super::cpu::PerCpu::IDLE
        && prev_owner != current_tid
        && let Some(mut thread) = crate::frame::state::threads().write(prev_owner)
    {
        let rs = thread.init_register_state();

        save_fp_state(rs);
    }

    if let Some(thread) = crate::frame::state::threads().read(current_tid)
        && let Some(rs) = thread.register_state()
    {
        load_fp_state(rs);
    }

    // Re-enable FP access.
    let cpacr = sysreg::cpacr_el1();

    sysreg::set_cpacr_el1(cpacr | (0b11 << 20));
    sysreg::isb();
}

#[cfg(target_os = "none")]
fn save_fp_state(rs: &mut crate::frame::arch::register_state::RegisterState) {
    // SAFETY: Reads FP registers that are currently live on this core.
    // fp_regs is at offset 288 (after 8 bytes padding for u128 alignment).
    unsafe {
        let fp_base = (rs as *mut _ as usize) + 288;

        core::arch::asm!(
            "stp q0, q1, [{base}]",
            "stp q2, q3, [{base}, #32]",
            "stp q4, q5, [{base}, #64]",
            "stp q6, q7, [{base}, #96]",
            "stp q8, q9, [{base}, #128]",
            "stp q10, q11, [{base}, #160]",
            "stp q12, q13, [{base}, #192]",
            "stp q14, q15, [{base}, #224]",
            "stp q16, q17, [{base}, #256]",
            "stp q18, q19, [{base}, #288]",
            "stp q20, q21, [{base}, #320]",
            "stp q22, q23, [{base}, #352]",
            "stp q24, q25, [{base}, #384]",
            "stp q26, q27, [{base}, #416]",
            "stp q28, q29, [{base}, #448]",
            "stp q30, q31, [{base}, #480]",
            "mrs {tmp}, fpcr",
            "str {tmp}, [{base}, #512]",
            "mrs {tmp}, fpsr",
            "str {tmp}, [{base}, #520]",
            base = in(reg) fp_base,
            tmp = out(reg) _,
            options(nostack),
        );
    }
}

#[cfg(target_os = "none")]
fn load_fp_state(rs: &crate::frame::arch::register_state::RegisterState) {
    // SAFETY: Writes FP registers from saved state.
    unsafe {
        let fp_base = (rs as *const _ as usize) + 288;

        core::arch::asm!(
            "ldp q0, q1, [{base}]",
            "ldp q2, q3, [{base}, #32]",
            "ldp q4, q5, [{base}, #64]",
            "ldp q6, q7, [{base}, #96]",
            "ldp q8, q9, [{base}, #128]",
            "ldp q10, q11, [{base}, #160]",
            "ldp q12, q13, [{base}, #192]",
            "ldp q14, q15, [{base}, #224]",
            "ldp q16, q17, [{base}, #256]",
            "ldp q18, q19, [{base}, #288]",
            "ldp q20, q21, [{base}, #320]",
            "ldp q22, q23, [{base}, #352]",
            "ldp q24, q25, [{base}, #384]",
            "ldp q26, q27, [{base}, #416]",
            "ldp q28, q29, [{base}, #448]",
            "ldp q30, q31, [{base}, #480]",
            "ldr {tmp}, [{base}, #512]",
            "msr fpcr, {tmp}",
            "ldr {tmp}, [{base}, #520]",
            "msr fpsr, {tmp}",
            base = in(reg) fp_base,
            tmp = out(reg) _,
            options(nostack),
        );
    }
}

fn handle_instruction_abort(frame: &mut TrapFrame) {
    let far = frame.far;
    let esr = frame.esr;
    let handler_addr = FAULT_HANDLER.load(Ordering::Acquire);

    if handler_addr == 0 {
        unimplemented_el0(frame, "instruction abort (no handler)");
    }

    // Instruction fetches are always reads.
    let is_write = false;
    // SAFETY: set_fault_handler stores a valid fn pointer.
    let handler: fn(u64, bool, u64) -> FaultAction = unsafe { core::mem::transmute(handler_addr) };

    match handler(far, is_write, esr) {
        FaultAction::Resolved => {}
        FaultAction::Kill => {
            unimplemented_el0(frame, "instruction abort (killed)");
        }
    }
}

fn handle_data_abort(frame: &mut TrapFrame) {
    let far = frame.far;
    let esr = frame.esr;
    let is_write = (esr >> 6) & 1 != 0; // WnR bit
    let handler_addr = FAULT_HANDLER.load(Ordering::Acquire);

    if handler_addr == 0 {
        unimplemented_el0(frame, "data abort (no handler)");
    }

    // SAFETY: set_fault_handler stores a valid fn pointer.
    let handler: fn(u64, bool, u64) -> FaultAction = unsafe { core::mem::transmute(handler_addr) };

    match handler(far, is_write, esr) {
        FaultAction::Resolved => {} // Return to EL0, retry the instruction.
        FaultAction::Kill => {
            unimplemented_el0(frame, "data abort (killed)");
        }
    }
}

fn unimplemented_el0(frame: &TrapFrame, kind: &str) -> ! {
    sysreg::disable_irqs();

    crate::println!();

    #[cfg(target_os = "none")]
    {
        // SAFETY: percpu is initialized on this core during boot.
        let tid = unsafe { super::cpu::percpu().current_thread };

        crate::println!("EL0 {kind} — thread {tid}");
    }

    #[cfg(not(target_os = "none"))]
    crate::println!("EL0 {kind}");
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
