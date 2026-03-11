// AUDIT: 2026-03-11 — 1 unsafe block verified, 6-category checklist applied.
// SMP isolation verified: PERCPU uses AtomicBool with Release/Acquire ordering.
// core_id() reads MPIDR_EL1 (read-only CPU identification register, nomem correct).
// No data races possible — all shared state is atomic. No bugs found.
//! Per-CPU data structures.
//!
//! Each core has a `PerCpu` slot indexed by its MPIDR affinity (core ID).
//! TPIDR_EL1 continues to point at the current Thread's Context — PerCpu is
//! a side table accessed via `core_id()`, not through TPIDR_EL1.

use core::sync::atomic::{AtomicBool, Ordering};

/// Maximum number of CPU cores supported.
pub const MAX_CORES: usize = 8;

static PERCPU: [PerCpu; MAX_CORES] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: PerCpu = PerCpu {
        online: AtomicBool::new(false),
    };
    [INIT; MAX_CORES]
};

pub struct PerCpu {
    pub online: AtomicBool,
}

/// Read the current core's MPIDR affinity (bits [7:0]).
#[inline(always)]
pub fn core_id() -> u32 {
    let mpidr: u64;

    // SAFETY: MPIDR_EL1 is a read-only CPU identification register.
    // It does not access memory (nomem correct) and has no side effects.
    // The register value is stable for the lifetime of the core.
    // nostack is correct — no stack operations in the asm block.
    unsafe {
        core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nostack, nomem));
    }

    (mpidr & 0xFF) as u32
}
/// Mark a core as online. Core identity comes from `core_id()` (MPIDR).
pub fn init_core(id: u32) {
    assert!((id as usize) < MAX_CORES, "core_id exceeds MAX_CORES");

    PERCPU[id as usize].online.store(true, Ordering::Release);
}
/// Check if a core is online.
pub fn is_online(id: u32) -> bool {
    if (id as usize) >= MAX_CORES {
        return false;
    }

    PERCPU[id as usize].online.load(Ordering::Acquire)
}
/// Count the number of online cores.
pub fn online_count() -> u32 {
    let mut count = 0;

    for percpu in &PERCPU {
        if percpu.online.load(Ordering::Acquire) {
            count += 1;
        }
    }

    count
}
