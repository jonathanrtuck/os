// AUDIT: 2026-03-11 — 0 unsafe blocks (arch-specific code moved to arch::per_core).
// SMP isolation verified: PERCPU uses AtomicBool with Release/Acquire ordering.
// No data races possible — all shared state is atomic. No bugs found.
//! Per-CPU data structures.
//!
//! Each core has a `PerCpu` slot indexed by its hardware core ID.
//! The core ID is read via `arch::per_core::core_id()` (MPIDR on aarch64).
//! TPIDR_EL1 continues to point at the current Thread's Context — PerCpu is
//! a side table accessed via `core_id()`, not through TPIDR_EL1.

use core::sync::atomic::{AtomicBool, Ordering};

/// Maximum number of CPU cores supported (from system_config via paging).
pub const MAX_CORES: usize = super::paging::MAX_CORES as usize;

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

/// Read the current core's hardware ID.
///
/// Delegates to arch-specific register read (MPIDR_EL1 on aarch64).
#[inline(always)]
pub fn core_id() -> u32 {
    super::arch::per_core::core_id()
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
