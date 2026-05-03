//! GICv3 driver — distributor, redistributor, and CPU interface.
//!
//! Initializes the Generic Interrupt Controller for Group 1 non-secure
//! interrupts at EL1. The distributor is shared; the redistributor and CPU
//! interface are per-core.
//!
//! After [`init`], the GIC is ready to deliver interrupts. The caller must
//! still unmask IRQs in PSTATE (clear DAIF.I) and configure an interrupt
//! source (e.g., the timer) to actually generate one.

use super::{mmio, platform, sysreg};

// ---------------------------------------------------------------------------
// INTID constants
// ---------------------------------------------------------------------------

/// Virtual timer PPI (EL1 virtual timer).
pub const INTID_VTIMER: u32 = 27;

/// Spurious interrupt — returned by [`acknowledge`] when no interrupt is
/// pending. The handler should return immediately without calling
/// [`end_of_interrupt`].
pub const INTID_SPURIOUS: u32 = 1023;

// ---------------------------------------------------------------------------
// Distributor registers (GICD_*, shared, MMIO)
// ---------------------------------------------------------------------------

const GICD_CTLR: usize = 0x0000;
const GICD_IGROUPR: usize = 0x0080; // + 4*n, 1 bit/interrupt
#[allow(dead_code)]
const GICD_ISENABLER: usize = 0x0100; // + 4*n, 1 bit/interrupt
const GICD_IPRIORITYR: usize = 0x0400; // + n, 1 byte/interrupt

const GICD_CTLR_ENABLE_GRP1_NS: u32 = 1 << 1;
const GICD_CTLR_ARE_NS: u32 = 1 << 4;

// ---------------------------------------------------------------------------
// Redistributor registers (GICR_*, per-core, MMIO)
// ---------------------------------------------------------------------------

/// Each redistributor frame is 128 KiB: 64 KiB RD_base + 64 KiB SGI_base.
const GICR_STRIDE: usize = 0x20000;

// RD_base offsets
const GICR_WAKER: usize = 0x0014;
const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

// SGI_base offsets (add to RD_base + 0x10000)
const GICR_SGI_BASE: usize = 0x10000;
const GICR_IGROUPR0: usize = GICR_SGI_BASE + 0x0080;
const GICR_IGRPMODR0: usize = GICR_SGI_BASE + 0x0D00;
const GICR_ISENABLER0: usize = GICR_SGI_BASE + 0x0100;
const GICR_IPRIORITYR: usize = GICR_SGI_BASE + 0x0400; // + n, 1 byte/interrupt

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the full GIC for the BSP: redistributor, CPU interface, and
/// distributor.
///
/// The distributor is initialized exactly once (here). Secondary cores call
/// [`init_per_core`] instead.
pub fn init() {
    let core_id = super::cpu::core_id_from_mpidr(sysreg::mpidr_el1());

    init_per_core(core_id);
    init_distributor(platform::GIC_DIST_BASE);
}

/// Initialize per-core GIC state: redistributor + CPU interface.
///
/// Called by every core (BSP via [`init`], secondaries directly). Does not
/// require the distributor — only initializes per-core redistributor and
/// CPU interface state.
pub fn init_per_core(core_id: usize) {
    let redist_base = redist_base_for_core(core_id);

    init_redistributor(redist_base);
    init_cpu_interface();
}

fn redist_base_for_core(core_id: usize) -> usize {
    platform::GIC_REDIST_BASE + core_id * GICR_STRIDE
}

/// Read ICC_IAR1_EL1 to acknowledge the highest-priority pending interrupt.
///
/// Returns the INTID (0–1019 for real interrupts, [`INTID_SPURIOUS`] if none
/// pending). The caller must call [`end_of_interrupt`] after handling, unless
/// the INTID is spurious.
#[inline]
pub fn acknowledge() -> u32 {
    (sysreg::icc_iar1_el1() & 0xFFFF_FF) as u32
}

/// Signal end-of-interrupt for the given INTID. Must be called after
/// handling every non-spurious interrupt acknowledged by [`acknowledge`].
#[inline]
pub fn end_of_interrupt(intid: u32) {
    sysreg::set_icc_eoir1_el1(intid as u64);
}

// ---------------------------------------------------------------------------
// Distributor setup (one-time, core 0)
// ---------------------------------------------------------------------------

fn init_distributor(dist_base: usize) {
    // Disable the distributor while configuring. This prevents spurious
    // interrupt delivery if the distributor was left enabled by firmware.
    mmio::write32(dist_base + GICD_CTLR, 0);
    sysreg::dsb_sy();
    sysreg::isb();

    // Set all SPIs (INTIDs 32–1019) to Group 1 non-secure.
    // GICD_IGROUPR[0] covers INTIDs 0–31 (SGIs/PPIs, handled by redistributor).
    // GICD_IGROUPR[1..] covers SPIs.
    for n in 1..32 {
        mmio::write32(dist_base + GICD_IGROUPR + 4 * n, 0xFFFF_FFFF);
    }

    // Set all SPI priorities to 0xA0 (mid-range, below default PMR of 0xFF).
    // IPRIORITYR is byte-indexed (one byte per INTID).
    for intid in 32..1020usize {
        mmio::write8(dist_base + GICD_IPRIORITYR + intid, 0xA0);
    }

    // Enable the distributor: Group 1 non-secure, affinity routing.
    mmio::write32(
        dist_base + GICD_CTLR,
        GICD_CTLR_ARE_NS | GICD_CTLR_ENABLE_GRP1_NS,
    );

    // Ensure the enable is visible to the GIC before returning.
    sysreg::dsb_sy();
    sysreg::isb();
}

// ---------------------------------------------------------------------------
// Redistributor setup (per-core)
// ---------------------------------------------------------------------------

fn init_redistributor(redist_base: usize) {
    // Wake the redistributor.
    let waker = mmio::read32(redist_base + GICR_WAKER);

    mmio::write32(
        redist_base + GICR_WAKER,
        waker & !GICR_WAKER_PROCESSOR_SLEEP,
    );

    // Wait for ChildrenAsleep to clear (redistributor is awake).
    // spin_loop() emits a YIELD hint for power-efficient polling.
    let mut timeout = 1_000_000u32;

    while mmio::read32(redist_base + GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP != 0 {
        core::hint::spin_loop();

        timeout -= 1;

        if timeout == 0 {
            crate::println!("gic: redistributor wake timeout");

            break;
        }
    }

    // Set all SGIs/PPIs (INTIDs 0–31) to Group 1 non-secure.
    mmio::write32(redist_base + GICR_IGROUPR0, 0xFFFF_FFFF);
    mmio::write32(redist_base + GICR_IGRPMODR0, 0x0000_0000);

    // Set SGI/PPI priorities to 0xA0 (byte-indexed, one per INTID).
    for intid in 0..32usize {
        mmio::write8(redist_base + GICR_IPRIORITYR + intid, 0xA0);
    }

    // Enable the virtual timer PPI (INTID 27).
    mmio::write32(redist_base + GICR_ISENABLER0, 1 << INTID_VTIMER);

    // Ensure all redistributor writes complete before CPU interface setup.
    sysreg::dsb_sy();
    sysreg::isb();
}

// ---------------------------------------------------------------------------
// CPU interface setup (per-core, ICC_* system registers)
// ---------------------------------------------------------------------------

fn init_cpu_interface() {
    // Enable the system register interface.
    let sre = sysreg::icc_sre_el1();

    sysreg::set_icc_sre_el1(sre | (1 << 0)); // SRE bit
    sysreg::isb();

    // Set the priority mask to accept all priorities.
    sysreg::set_icc_pmr_el1(0xFF);

    // Set binary point (group/subgroup split). Default is fine.
    sysreg::set_icc_bpr1_el1(0);

    // Enable Group 1 non-secure interrupts.
    sysreg::set_icc_igrpen1_el1(1);
    sysreg::isb();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redist_core_0_equals_base() {
        assert_eq!(redist_base_for_core(0), platform::GIC_REDIST_BASE);
    }

    #[test]
    fn redist_core_3() {
        assert_eq!(
            redist_base_for_core(3),
            platform::GIC_REDIST_BASE + 3 * GICR_STRIDE,
        );
    }

    #[test]
    fn redist_stride_is_128k() {
        let diff = redist_base_for_core(1) - redist_base_for_core(0);

        assert_eq!(diff, 0x20000);
    }
}
