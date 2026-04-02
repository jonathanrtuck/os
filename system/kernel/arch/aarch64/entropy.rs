//! AArch64 hardware entropy sources.
//!
//! Provides the arch-specific entropy interface for the generic PRNG module.
//! Three entropy sources:
//!
//! - **RNDR** (FEAT_RNG, ARMv8.5-A): Hardware random number generator.
//!   Available on Apple Silicon M1+. Returns 64 bits per call; may fail
//!   if the hardware entropy pool is temporarily depleted.
//!
//! - **RNDRRS** (FEAT_RNG): Like RNDR but guarantees reseeding since the
//!   last call. Slower but stronger. Used for critical seeding operations.
//!
//! - **CNTVCT_EL0**: Generic timer counter. Not a direct entropy source,
//!   but deltas between interrupt arrivals carry jitter bits. At 24 MHz
//!   (Apple Silicon) this gives ~42ns resolution — sufficient for jitter
//!   extraction.
//!
//! Detection: Probe `ID_AA64ISAR0_EL1` bits [63:60] at boot. Value
//! 0b0001 = FEAT_RNG supported.

/// Check if hardware RNG (FEAT_RNG) is available.
///
/// Reads `ID_AA64ISAR0_EL1` and checks RNDR field (bits [63:60]).
/// Value 0b0001 indicates RNDR/RNDRRS support.
pub fn has_hardware_rng() -> bool {
    let isar0: u64;

    // SAFETY: Reading an identification register. This is a read-only
    // system register that describes the CPU's feature set. No `nomem` —
    // system register reads have implicit ordering requirements.
    unsafe {
        core::arch::asm!(
            "mrs {}, id_aa64isar0_el1",
            out(reg) isar0,
            options(nostack),
        );
    }

    // RNDR field: bits [63:60]. 0b0001 = FEAT_RNG supported.
    ((isar0 >> 60) & 0xF) >= 1
}

/// Read 64 bits from the hardware RNG (RNDR instruction).
///
/// Returns `Some(value)` on success, `None` if the hardware entropy pool
/// is temporarily depleted. Callers should retry or fall back to jitter.
///
/// # Panics
///
/// Must not be called if `has_hardware_rng()` returns false (the RNDR
/// instruction would be UNDEFINED).
pub fn hardware_random() -> Option<u64> {
    let val: u64;
    let success: u64;

    // SAFETY: RNDR reads from the hardware RNG. It sets NZCV flags:
    // on success, Z=0 and the register contains random data.
    // On failure (entropy depleted), Z=1 and the register is zero.
    // No `nomem` — the instruction has side effects on the hardware RNG state.
    unsafe {
        core::arch::asm!(
            "mrs {val}, s3_3_c2_c4_0", // RNDR
            "cset {ok}, ne",            // ok = (Z == 0) ? 1 : 0
            val = out(reg) val,
            ok = out(reg) success,
            options(nostack),
        );
    }

    if success != 0 {
        Some(val)
    } else {
        None
    }
}

/// Read 64 bits from the hardware RNG with reseed guarantee (RNDRRS).
///
/// Like `hardware_random()` but guarantees the hardware RNG has been
/// reseeded since the last call. Slower but provides stronger guarantee
/// for critical seeding operations (e.g., initial PRNG seeding at boot).
///
/// Returns `None` if entropy is unavailable.
pub fn hardware_random_reseeded() -> Option<u64> {
    let val: u64;
    let success: u64;

    // SAFETY: RNDRRS reads from the reseeded hardware RNG. Same flag
    // semantics as RNDR. No `nomem` — hardware RNG state side effects.
    unsafe {
        core::arch::asm!(
            "mrs {val}, s3_3_c2_c4_1", // RNDRRS
            "cset {ok}, ne",
            val = out(reg) val,
            ok = out(reg) success,
            options(nostack),
        );
    }

    if success != 0 {
        Some(val)
    } else {
        None
    }
}

/// Collect jitter entropy by measuring execution time variation.
///
/// Performs a fixed workload (memory reads + arithmetic) and measures the
/// elapsed timer ticks. The low bits of the elapsed count carry entropy
/// from cache state, branch predictor state, and pipeline effects — these
/// are genuinely nondeterministic even in a VM.
///
/// Based on the jitterentropy technique (Stephan Mueller, 2014). Each call
/// returns 8 bytes of raw jitter data; the caller should credit ~4-8 bits
/// of entropy per call (conservative estimate for a 24 MHz counter).
pub fn collect_jitter(scratch: &mut [u8; 64]) -> [u8; 8] {
    let start = timing_counter();

    // Workload: memory access pattern that creates variable cache/TLB latency.
    // The scratch buffer creates real memory traffic; each iteration depends
    // on the previous value to prevent optimization.
    let mut acc: u64 = start;
    for i in 0..64 {
        scratch[i] = scratch[i].wrapping_add(acc as u8);
        acc = acc
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(scratch[i] as u64);
    }

    let end = timing_counter();
    let delta = end.wrapping_sub(start) ^ acc;

    delta.to_le_bytes()
}

/// Read the generic timer counter (CNTVCT_EL0).
///
/// Returns the current value of the virtual count register. Used for
/// interrupt timing jitter extraction — the low bits of deltas between
/// interrupt arrivals contain genuine entropy from cache/branch-predictor
/// nondeterminism.
///
/// Frequency: CNTFRQ_EL0 (typically 24 MHz on Apple Silicon, 62.5 MHz
/// on QEMU). At 24 MHz, resolution is ~42ns.
#[inline(always)]
pub fn timing_counter() -> u64 {
    let val: u64;

    // SAFETY: CNTVCT_EL0 is the virtual timer count — a monotonic counter
    // readable from EL1 and EL0 (CNTKCTL_EL1.EL0VCTEN=1 set by timer::init).
    // `nomem` is correct — this is a pure counter read with no memory effects.
    // (Different from TPIDR: the counter is hardware-maintained, not software-set.)
    unsafe {
        core::arch::asm!(
            "mrs {}, cntvct_el0",
            out(reg) val,
            options(nostack, nomem),
        );
    }

    val
}
