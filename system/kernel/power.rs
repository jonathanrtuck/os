// AUDIT: 2026-03-11 — 1 unsafe block verified. 6-category checklist applied.
// No bugs found. HVC #0 with PSCI CPU_ON follows SMCCC calling convention
// (x0=func_id, x1=target, x2=entry, x3=context). options(nostack) correct —
// HVC traps to hypervisor, no caller stack usage. No nomem — HVC has massive
// side effects (boots a secondary core). Return codes properly handled
// (SUCCESS + ALREADY_ON = Ok, else Err). SAFETY comment present and accurate.

//! PSCI (Power State Coordination Interface) wrapper.
//!
//! Uses HVC as the conduit (QEMU virt default). PSCI function IDs follow
//! the ARM SMCCC / PSCI specification.

/// PSCI CPU_ON (SMC64 variant): function ID 0xC400_0003.
const PSCI_CPU_ON: u64 = 0xC400_0003;
/// PSCI return codes.
const PSCI_SUCCESS: i64 = 0;
const PSCI_ALREADY_ON: i64 = -4;

/// Boot a secondary core.
///
/// - `target_cpu`: MPIDR affinity value (e.g., 1, 2, 3).
/// - `entry_point`: physical address of the secondary entry trampoline.
/// - `context_id`: value passed in x0 to the entry point (we use core_id).
///
/// Returns `Ok(())` on success or if the core is already on.
pub fn cpu_on(target_cpu: u64, entry_point: u64, context_id: u64) -> Result<(), i64> {
    let ret: i64;

    // SAFETY: HVC #0 with PSCI CPU_ON is the standard way to bring up
    // secondary cores. The entry_point must be a valid physical address
    // with MMU setup code. x0-x3 are the SMCCC argument registers.
    unsafe {
        // No nomem: HVC traps to the hypervisor and boots a secondary core.
        // Massive side effects — LLVM must treat this as a full barrier.
        core::arch::asm!(
            "hvc #0",
            inout("x0") PSCI_CPU_ON => ret,
            in("x1") target_cpu,
            in("x2") entry_point,
            in("x3") context_id,
            options(nostack)
        );
    }

    match ret {
        PSCI_SUCCESS | PSCI_ALREADY_ON => Ok(()),
        err => Err(err),
    }
}
