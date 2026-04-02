//! AArch64 security features: PAC key management and feature detection.
//!
//! Pointer Authentication Codes (PAC, FEAT_PAuth, ARMv8.3-A):
//! - Signs return addresses on function entry (PACIA LR)
//! - Verifies on return (AUTIA LR); forged pointers fault
//! - Per-process keys set via APIA/APDA/APIB/APDB/APG key registers
//! - Keys must be switched on context switch (same as TTBR0)
//!
//! Branch Target Identification (BTI, FEAT_BTI, ARMv8.5-A):
//! - Restricts indirect branch targets to BTI-marked instructions
//! - Enabled per-page via GP bit in page table descriptors
//! - Zero runtime overhead (BTI is NOP on unsupported cores)
//!
//! Both features are available on Apple Silicon M1+.

/// PAC keys: 5 × 128-bit keys (stored as pairs of u64).
///
/// APIA — Instruction Address auth key A (return addresses)
/// APDA — Data Address auth key A
/// APIB — Instruction Address auth key B
/// APDB — Data Address auth key B
/// APG  — Generic auth key (arbitrary data signing)
#[derive(Clone)]
pub struct PacKeys {
    pub apia: [u64; 2],
    pub apda: [u64; 2],
    pub apib: [u64; 2],
    pub apdb: [u64; 2],
    pub apga: [u64; 2],
}

impl PacKeys {
    /// Generate random PAC keys from a PRNG.
    pub fn generate(prng: &mut crate::random::Prng) -> Self {
        Self {
            apia: [prng.next_u64(), prng.next_u64()],
            apda: [prng.next_u64(), prng.next_u64()],
            apib: [prng.next_u64(), prng.next_u64()],
            apdb: [prng.next_u64(), prng.next_u64()],
            apga: [prng.next_u64(), prng.next_u64()],
        }
    }

    /// Zero keys — used when PAC is not available.
    pub fn zero() -> Self {
        Self {
            apia: [0; 2],
            apda: [0; 2],
            apib: [0; 2],
            apdb: [0; 2],
            apga: [0; 2],
        }
    }
}

/// Check if Pointer Authentication (FEAT_PAuth) is available.
///
/// Reads `ID_AA64ISAR1_EL1` and checks APA/API fields.
pub fn pac_supported() -> bool {
    let isar1: u64;

    // SAFETY: Reading an identification register. Read-only, no side effects.
    // No `nomem` — system register reads have implicit ordering requirements.
    unsafe {
        core::arch::asm!(
            "mrs {}, id_aa64isar1_el1",
            out(reg) isar1,
            options(nostack),
        );
    }

    // APA field: bits [7:4]. API field: bits [11:8].
    // Either non-zero means some form of PAC is supported.
    let apa = (isar1 >> 4) & 0xF;
    let api = (isar1 >> 8) & 0xF;
    apa > 0 || api > 0
}

/// Check if Branch Target Identification (FEAT_BTI) is available.
///
/// Reads `ID_AA64PFR1_EL1` and checks BT field (bits [3:0]).
pub fn bti_supported() -> bool {
    let pfr1: u64;

    // SAFETY: Reading an identification register. Read-only.
    unsafe {
        core::arch::asm!(
            "mrs {}, id_aa64pfr1_el1",
            out(reg) pfr1,
            options(nostack),
        );
    }

    // BT field: bits [3:0]. 0b0001 = BTI supported.
    (pfr1 & 0xF) >= 1
}

/// Load PAC keys into the EL1 key registers.
///
/// Called during context switch to set the current process's PAC keys.
/// Each key register is 128 bits, split across two 64-bit system registers
/// (KEY_HI and KEY_LO).
pub fn set_pac_keys(keys: &PacKeys) {
    // SAFETY: Writing PAC key registers. These control pointer authentication
    // for EL0 code. No `nomem` — system register writes affect subsequent
    // pointer authentication operations.
    //
    // Raw system register encodings (LLVM doesn't recognize the friendly names
    // without +pauth target feature, which we don't want to enable globally):
    //   APIAKeyLo_EL1 = S3_0_C2_C1_0    APIAKeyHi_EL1 = S3_0_C2_C1_1
    //   APDAKeyLo_EL1 = S3_0_C2_C2_0    APDAKeyHi_EL1 = S3_0_C2_C2_1
    //   APIBKeyLo_EL1 = S3_0_C2_C1_2    APIBKeyHi_EL1 = S3_0_C2_C1_3
    //   APDBKeyLo_EL1 = S3_0_C2_C2_2    APDBKeyHi_EL1 = S3_0_C2_C2_3
    //   APGAKeyLo_EL1 = S3_0_C2_C3_0    APGAKeyHi_EL1 = S3_0_C2_C3_1
    unsafe {
        core::arch::asm!(
            "msr s3_0_c2_c1_0, {0}",
            "msr s3_0_c2_c1_1, {1}",
            in(reg) keys.apia[0],
            in(reg) keys.apia[1],
            options(nostack),
        );
        core::arch::asm!(
            "msr s3_0_c2_c2_0, {0}",
            "msr s3_0_c2_c2_1, {1}",
            in(reg) keys.apda[0],
            in(reg) keys.apda[1],
            options(nostack),
        );
        core::arch::asm!(
            "msr s3_0_c2_c1_2, {0}",
            "msr s3_0_c2_c1_3, {1}",
            in(reg) keys.apib[0],
            in(reg) keys.apib[1],
            options(nostack),
        );
        core::arch::asm!(
            "msr s3_0_c2_c2_2, {0}",
            "msr s3_0_c2_c2_3, {1}",
            in(reg) keys.apdb[0],
            in(reg) keys.apdb[1],
            options(nostack),
        );
        core::arch::asm!(
            "msr s3_0_c2_c3_0, {0}",
            "msr s3_0_c2_c3_1, {1}",
            in(reg) keys.apga[0],
            in(reg) keys.apga[1],
            options(nostack),
        );
    }
}
