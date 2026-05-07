//! Reader for the paravirtual HVF timing counter page.
//!
//! When running under our macOS hypervisor, a shared memory page exposes
//! per-vCPU counters that split execution time between guest and host
//! (Hypervisor.framework + dispatch) and classify VMEXITs by ESR exception
//! class. The hypervisor stamps a magic value in the page header so the
//! kernel can detect when it is running under an HVF-aware hypervisor (and
//! skip silently otherwise).
//!
//! Wire format must match `~/Sites/hypervisor/Sources/HVFTiming.swift`. If
//! the layout ever changes, bump both ends and the DTB compatible string
//! ("arts,hvf-timing-v1") together.
//!
//! # Why volatile reads
//!
//! The hypervisor writes counter slots from a different host CPU thread (the
//! vCPU thread). The kernel reads them while running inside the same vCPU,
//! so the writer and reader are not concurrent — but the compiler does not
//! know the page is observable to another agent. Volatile reads block the
//! optimizer from caching previously-read values across reads, which is the
//! invariant we need to expose to the bench (each call returns fresh
//! counters, not a register copy).
//!
//! # Why no atomicity beyond u64 stores
//!
//! Aarch64 64-bit aligned stores are single-copy atomic. The kernel reads
//! whole u64s and tolerates skewed snapshots — bench callers compute deltas
//! around an interval, so skew up to one in-flight VMEXIT is captured by the
//! delta. No CAS, no fences.

#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicUsize, Ordering};

/// Magic value at offset 0 of the counter page. Matches the hypervisor.
const HVF_TIMING_MAGIC: u32 = 0x4856_4654;

/// Wire format version. Mismatched versions disable the reader.
const HVF_TIMING_VERSION: u32 = 1;

/// Bytes per per-vCPU slot on the wire.
const SLOT_STRIDE: usize = 64;

/// Bytes from the start of the page to the first slot.
const HEADER_SIZE: usize = 16;

/// Field offsets inside a per-vCPU slot.
const FIELD_GUEST_TICKS: usize = 0;
const FIELD_HOST_TICKS: usize = 8;
const FIELD_EXITS_TOTAL: usize = 16;
const FIELD_EXITS_DATA_ABORT: usize = 24;
const FIELD_EXITS_HVC: usize = 32;
const FIELD_EXITS_SYSREG: usize = 40;
const FIELD_EXITS_WFX: usize = 48;
const FIELD_EXITS_VTIMER: usize = 56;

/// Snapshot of one vCPU's counters at a point in time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HvfTiming {
    /// `mach_absolute_time` ticks spent inside `hv_vcpu_run` (i.e., guest
    /// instructions + nested EL2/EL3 transitions). 24 MHz units on Apple
    /// Silicon, directly comparable to `read_cycle_counter()`.
    pub guest_ticks: u64,
    /// `mach_absolute_time` ticks spent in the hypervisor's exit handlers
    /// before the next `hv_vcpu_run` entry — register I/O, decode, MMIO
    /// emulation, virtio dispatch, etc. The "true" cost of a VMEXIT.
    pub host_ticks: u64,
    /// Total VMEXITs counted on this vCPU.
    pub exits_total: u64,
    /// Exits classified as guest-mode data aborts to MMIO regions.
    pub exits_data_abort: u64,
    /// Exits classified as HVC instructions (PSCI calls etc).
    pub exits_hvc: u64,
    /// Exits classified as MSR/MRS traps (e.g., emulated PMU registers).
    pub exits_sysreg: u64,
    /// Exits classified as WFI/WFE traps.
    pub exits_wfx: u64,
    /// Exits classified as virtual timer firings.
    pub exits_vtimer: u64,
}

impl HvfTiming {
    /// Element-wise difference: `self - earlier`. Wraps on subtraction so a
    /// quick interval that crosses a counter wrap returns a sane delta.
    pub fn diff(&self, earlier: &HvfTiming) -> HvfTiming {
        HvfTiming {
            guest_ticks: self.guest_ticks.wrapping_sub(earlier.guest_ticks),
            host_ticks: self.host_ticks.wrapping_sub(earlier.host_ticks),
            exits_total: self.exits_total.wrapping_sub(earlier.exits_total),
            exits_data_abort: self.exits_data_abort.wrapping_sub(earlier.exits_data_abort),
            exits_hvc: self.exits_hvc.wrapping_sub(earlier.exits_hvc),
            exits_sysreg: self.exits_sysreg.wrapping_sub(earlier.exits_sysreg),
            exits_wfx: self.exits_wfx.wrapping_sub(earlier.exits_wfx),
            exits_vtimer: self.exits_vtimer.wrapping_sub(earlier.exits_vtimer),
        }
    }
}

#[cfg(target_os = "none")]
static PAGE_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_os = "none")]
static SLOT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialize the reader from DTB-discovered values. Validates the magic
/// and version stamped at the page header — on mismatch the reader stays
/// disabled and `read()` returns a zero snapshot.
///
/// Safe to call before [`crate::frame::arch::platform::set_mmu_active`]:
/// during early boot the address is the raw PA, after MMU enable callers
/// must instead use [`reinit`] with the upper-half VA.
#[cfg(target_os = "none")]
pub fn init_phys(pa: usize, size: usize) {
    if pa == 0 || size < HEADER_SIZE + SLOT_STRIDE {
        return;
    }

    // SAFETY: pa is reported by the hypervisor in the DTB. The reader only
    // touches HEADER_SIZE + N*SLOT_STRIDE bytes; the validity of those bytes
    // is enforced by checking magic/version below, which both come from the
    // same page. If the page does not actually exist or the magic is wrong,
    // we leave PAGE_VA at zero and the rest of the code falls into the
    // "no-op" branch on every read.
    let magic = unsafe { core::ptr::read_volatile(pa as *const u32) };
    let version = unsafe { core::ptr::read_volatile((pa + 4) as *const u32) };
    let nr_vcpus = unsafe { core::ptr::read_volatile((pa + 8) as *const u32) };
    let slot_stride = unsafe { core::ptr::read_volatile((pa + 12) as *const u32) };

    if magic != HVF_TIMING_MAGIC
        || version != HVF_TIMING_VERSION
        || slot_stride as usize != SLOT_STRIDE
    {
        return;
    }

    let max_slots = (size - HEADER_SIZE) / SLOT_STRIDE;
    let slots = (nr_vcpus as usize).min(max_slots);

    PAGE_VA.store(pa, Ordering::Relaxed);
    SLOT_COUNT.store(slots, Ordering::Release);
}

/// Re-resolve the page address against the upper-half VA after MMU enable.
/// Called from `kernel_main_upper` before any reader use.
#[cfg(target_os = "none")]
pub fn reinit_to_va() {
    let pa = PAGE_VA.load(Ordering::Relaxed);

    if pa == 0 {
        return;
    }

    PAGE_VA.store(super::platform::phys_to_virt(pa), Ordering::Release);
}

#[cfg(not(target_os = "none"))]
pub fn init_phys(_pa: usize, _size: usize) {}

#[cfg(not(target_os = "none"))]
pub fn reinit_to_va() {}

#[cfg(not(target_os = "none"))]
pub fn print_info() {}

/// Print discovered counter page status. Called once from
/// `kernel_main_upper::print_info`-equivalent during boot diagnostics.
#[cfg(target_os = "none")]
pub fn print_info() {
    let slots = SLOT_COUNT.load(Ordering::Acquire);

    if slots == 0 {
        crate::println!("hvf-timing: not advertised by hypervisor");
    } else {
        let va = PAGE_VA.load(Ordering::Relaxed);

        crate::println!("hvf-timing: counter page va={va:#x} ({slots} slot(s))");
    }
}

/// Whether the reader was able to find a valid counter page. Used by the
/// bench to decide whether to print the per-bench split column.
#[cfg(target_os = "none")]
pub fn enabled() -> bool {
    SLOT_COUNT.load(Ordering::Acquire) > 0
}

#[cfg(not(target_os = "none"))]
pub fn enabled() -> bool {
    false
}

/// Read a per-vCPU snapshot. Returns a zero-valued snapshot if the reader
/// is disabled or `core_id` is out of range.
#[cfg(target_os = "none")]
pub fn read(core_id: usize) -> HvfTiming {
    let slots = SLOT_COUNT.load(Ordering::Acquire);

    if core_id >= slots {
        return HvfTiming::default();
    }

    let base = PAGE_VA.load(Ordering::Relaxed);

    if base == 0 {
        return HvfTiming::default();
    }

    let slot_base = base + HEADER_SIZE + core_id * SLOT_STRIDE;

    // SAFETY: `slot_base` is the kernel-mode VA (or PA, pre-MMU) of a slot
    // in the counter page validated by `init_phys`. We only read 8-byte
    // fields at fixed offsets within the slot. Volatile reads ensure each
    // call returns a fresh value (the hypervisor is the only writer; bench
    // measurement intervals tolerate skew up to one in-flight VMEXIT).
    unsafe {
        HvfTiming {
            guest_ticks: core::ptr::read_volatile((slot_base + FIELD_GUEST_TICKS) as *const u64),
            host_ticks: core::ptr::read_volatile((slot_base + FIELD_HOST_TICKS) as *const u64),
            exits_total: core::ptr::read_volatile((slot_base + FIELD_EXITS_TOTAL) as *const u64),
            exits_data_abort: core::ptr::read_volatile(
                (slot_base + FIELD_EXITS_DATA_ABORT) as *const u64,
            ),
            exits_hvc: core::ptr::read_volatile((slot_base + FIELD_EXITS_HVC) as *const u64),
            exits_sysreg: core::ptr::read_volatile((slot_base + FIELD_EXITS_SYSREG) as *const u64),
            exits_wfx: core::ptr::read_volatile((slot_base + FIELD_EXITS_WFX) as *const u64),
            exits_vtimer: core::ptr::read_volatile((slot_base + FIELD_EXITS_VTIMER) as *const u64),
        }
    }
}

#[cfg(not(target_os = "none"))]
pub fn read(_core_id: usize) -> HvfTiming {
    HvfTiming::default()
}

/// HVC immediate for the snapshot fence. PSCI uses HVC #0 with x0 selecting
/// the function (e.g., 0x8400_0008 = SYSTEM_OFF). Our hypervisor returns -1
/// for any unrecognized function ID, so picking a value outside the SMC
/// Calling Convention's owning ranges is safe — the side effect we want is
/// just the VMEXIT itself.
const HVF_TIMING_SNAPSHOT_FN: u64 = 0xC500_0001;

/// Force the hypervisor to flush its in-flight `mach_absolute_time` deltas
/// into the shared counter page. The HVF backend updates `guest_ticks`
/// only at VMEXIT boundaries, so a long-running benchmark that never exits
/// would see no movement between snapshots. Issuing an unrecognized HVC
/// triggers an HVC-class VMEXIT, the hypervisor adds the elapsed
/// `(t1 - t0)` to `guest_ticks` for this vCPU, then resumes. The HVC also
/// counts as one exit in the `exits_total` and `exits_hvc` buckets — bench
/// callers already pair this with a snapshot read, so the fence cost is
/// captured in `host_ticks` and visible in the printed split.
///
/// No-op when the reader is disabled (no advertisement in DTB).
#[cfg(target_os = "none")]
#[inline(never)]
pub fn force_snapshot() {
    if !enabled() {
        return;
    }

    // SAFETY: HVC #0 with an unrecognized function ID. Per SMCCC (DEN0028),
    // HVC may clobber x0-x17 — clobber_abi("C") covers the full caller-
    // saved register set including condition flags. Marked not-nomem
    // because the HVC has externally-visible side effects (VMEXIT, host
    // time observation).
    unsafe {
        core::arch::asm!(
            "hvc #0",
            inout("x0") HVF_TIMING_SNAPSHOT_FN => _,
            clobber_abi("C"),
            options(nostack),
        );
    }
}

#[cfg(not(target_os = "none"))]
pub fn force_snapshot() {}
