//! Per-CPU data structures.
//!
//! Each core has a `PerCpu` slot indexed by its MPIDR affinity (core ID).
//! TPIDR_EL1 continues to point at the current Thread's Context — PerCpu is
//! a side table accessed via `core_id()`, not through TPIDR_EL1.

use core::sync::atomic::{AtomicBool, Ordering};

/// Maximum number of CPU cores supported.
pub const MAX_CORES: usize = 8;

pub struct PerCpu {
    pub online: AtomicBool,
}

static PERCPU: [PerCpu; MAX_CORES] = {
    const INIT: PerCpu = PerCpu {
        online: AtomicBool::new(false),
    };
    [INIT; MAX_CORES]
};

/// Read the current core's MPIDR affinity (bits [7:0]).
#[inline(always)]
pub fn core_id() -> u32 {
    let mpidr: u64;

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

    for i in 0..MAX_CORES {
        if PERCPU[i].online.load(Ordering::Acquire) {
            count += 1;
        }
    }

    count
}
