//! ARM Power State Coordination Interface (PSCI) — DEN0022E v1.3.
//!
//! Provides the HVC calling convention for PSCI operations. Only CPU_ON
//! is implemented; other operations (CPU_OFF, CPU_SUSPEND, SYSTEM_RESET)
//! can be added as needed.

// Flat constants rather than an enum: HVC returns a raw i64 that may include
// codes we haven't enumerated. TryFrom would add a new failure mode. Revisit
// when a second PSCI operation is added.

/// PSCI CPU_ON function ID (SMC64/HVC64 encoding).
pub const CPU_ON: u32 = 0xC400_0003;

/// PSCI SYSTEM_OFF function ID (SMC32 encoding, per PSCI spec DEN0022E §5.5).
pub const SYSTEM_OFF: u32 = 0x8400_0008;

/// PSCI return codes (signed i32 per spec).
pub const SUCCESS: i32 = 0;
pub const NOT_SUPPORTED: i32 = -1;
pub const INVALID_PARAMETERS: i32 = -2;
pub const DENIED: i32 = -3;
pub const ALREADY_ON: i32 = -4;
pub const ON_PENDING: i32 = -5;
pub const INTERNAL_FAILURE: i32 = -6;
pub const NOT_PRESENT: i32 = -9;
pub const DISABLED: i32 = -10;

/// Issue PSCI CPU_ON via HVC to activate a secondary core.
///
/// - `target_cpu`: MPIDR affinity value for the target core.
/// - `entry_point`: Physical address of the secondary entry stub.
/// - `context_id`: Opaque value passed to the target core in x0.
///
/// Returns `Ok(())` on `PSCI_SUCCESS`, `Err(code)` on failure.
#[cfg(target_os = "none")]
#[inline(never)]
pub fn cpu_on(target_cpu: u64, entry_point: u64, context_id: u64) -> Result<(), i32> {
    let ret: i64;

    // SAFETY: HVC #0 traps to EL2 (the hypervisor) with PSCI CPU_ON in x0.
    // The hypervisor starts the target core at entry_point. This has global
    // side effects (a core powers on) — no `nomem`.
    // Register convention per PSCI spec DEN0022E §5.1.3:
    //   x0 = function_id, x1 = target_cpu, x2 = entry_point, x3 = context_id.
    //   Return value in x0. SMCCC: x4-x17 may be clobbered.
    unsafe {
        core::arch::asm!(
            "hvc #0",
            inout("x0") CPU_ON as u64 => ret,
            inout("x1") target_cpu => _,
            inout("x2") entry_point => _,
            inout("x3") context_id => _,
            out("x4") _,
            out("x5") _,
            out("x6") _,
            out("x7") _,
            out("x8") _,
            out("x9") _,
            out("x10") _,
            out("x11") _,
            out("x12") _,
            out("x13") _,
            out("x14") _,
            out("x15") _,
            out("x16") _,
            out("x17") _,
            options(nostack),
        );
    }

    let ret32 = ret as i32;

    if ret32 >= SUCCESS { Ok(()) } else { Err(ret32) }
}

/// Shut down the system via PSCI SYSTEM_OFF.
///
/// The hypervisor exits immediately. This call does not return.
#[cfg(target_os = "none")]
pub fn system_off() -> ! {
    // SAFETY: HVC #0 traps to EL2 (the hypervisor) with PSCI SYSTEM_OFF.
    // The hypervisor terminates the VM. This has global side effects —
    // no `nomem`.
    unsafe {
        core::arch::asm!(
            "hvc #0",
            in("x0") SYSTEM_OFF as u64,
            options(noreturn, nostack),
        );
    }
}

/// Describe a PSCI error code for diagnostics.
pub fn error_name(code: i32) -> &'static str {
    match code {
        SUCCESS => "SUCCESS",
        NOT_SUPPORTED => "NOT_SUPPORTED",
        INVALID_PARAMETERS => "INVALID_PARAMETERS",
        DENIED => "DENIED",
        ALREADY_ON => "ALREADY_ON",
        ON_PENDING => "ON_PENDING",
        INTERNAL_FAILURE => "INTERNAL_FAILURE",
        NOT_PRESENT => "NOT_PRESENT",
        DISABLED => "DISABLED",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_on_function_id_matches_spec() {
        // SMC64/HVC64 encoding: bit 30 set (SMC64), function number 3.
        assert_eq!(CPU_ON, 0xC400_0003);
    }

    #[test]
    fn system_off_function_id_matches_spec() {
        // SMC32 encoding, function number 8 (PSCI spec DEN0022E §5.5).
        assert_eq!(SYSTEM_OFF, 0x8400_0008);
    }

    #[test]
    fn return_codes_match_spec() {
        assert_eq!(SUCCESS, 0);
        assert_eq!(NOT_SUPPORTED, -1);
        assert_eq!(INVALID_PARAMETERS, -2);
        assert_eq!(DENIED, -3);
        assert_eq!(ALREADY_ON, -4);
        assert_eq!(ON_PENDING, -5);
        assert_eq!(INTERNAL_FAILURE, -6);
        assert_eq!(NOT_PRESENT, -9);
        assert_eq!(DISABLED, -10);
    }

    #[test]
    fn error_name_covers_all_codes() {
        assert_eq!(error_name(SUCCESS), "SUCCESS");
        assert_eq!(error_name(NOT_SUPPORTED), "NOT_SUPPORTED");
        assert_eq!(error_name(ALREADY_ON), "ALREADY_ON");
        assert_eq!(error_name(42), "UNKNOWN");
    }
}
