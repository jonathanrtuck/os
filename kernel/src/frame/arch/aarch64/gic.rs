//! GICv3 driver — distributor, redistributor, and CPU interface.
//!
//! Initializes the Generic Interrupt Controller for Group 1 non-secure
//! interrupts at EL1. The distributor is shared; the redistributor and CPU
//! interface are per-core.
//!
//! After [`init`], the GIC is ready to deliver interrupts. The caller must
//! still unmask IRQs in PSTATE (clear DAIF.I) and configure an interrupt
//! source (e.g., the timer) to actually generate one.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::{mmio, platform, sysreg};

static DIST_BASE: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// INTID constants
// ---------------------------------------------------------------------------

/// Reschedule SGI — sent via [`send_sgi`] to trigger a reschedule check
/// on the target core.
pub const SGI_RESCHEDULE: u32 = 0;

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
const GICD_ISENABLER: usize = 0x0100; // + 4*n, 1 bit/interrupt
const GICD_ICENABLER: usize = 0x0180; // + 4*n, 1 bit/interrupt (clear-enable)
const GICD_ICFGR: usize = 0x0C00; // + 4*n, 2 bits/interrupt (level/edge)
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
    init_distributor(platform::device_addr(platform::GIC_DIST_BASE));
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
    platform::device_addr(platform::GIC_REDIST_BASE + core_id * GICR_STRIDE)
}

/// Read ICC_IAR1_EL1 to acknowledge the highest-priority pending interrupt.
///
/// Returns the INTID (0–1019 for real interrupts, [`INTID_SPURIOUS`] if none
/// pending). The caller must call [`end_of_interrupt`] after handling, unless
/// the INTID is spurious.
#[inline]
pub fn acknowledge() -> u32 {
    (sysreg::icc_iar1_el1() & 0x00FF_FFFF) as u32
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
    DIST_BASE.store(dist_base, Ordering::Relaxed);

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

    // Enable SGIs 0–15 (for IPI) and the virtual timer PPI (INTID 27).
    mmio::write32(redist_base + GICR_ISENABLER0, 0xFFFF | (1 << INTID_VTIMER));

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

// ---------------------------------------------------------------------------
// SPI mask/unmask (distributor ISENABLER / ICENABLER)
// ---------------------------------------------------------------------------

/// Compute the register offset and bit position for an SPI INTID.
fn spi_reg_bit(intid: u32) -> (usize, u32) {
    let reg_offset = (intid / 32) as usize * 4;
    let bit = intid % 32;

    (reg_offset, bit)
}

/// Mask (disable) an SPI. Only valid for INTID >= 32.
pub fn mask_spi(intid: u32) {
    debug_assert!(intid >= 32, "mask_spi: PPIs/SGIs not supported");

    let dist = DIST_BASE.load(Ordering::Relaxed);
    let (reg_off, bit) = spi_reg_bit(intid);

    mmio::write32(dist + GICD_ICENABLER + reg_off, 1 << bit);
}

/// Unmask (enable) an SPI. Only valid for INTID >= 32.
pub fn unmask_spi(intid: u32) {
    debug_assert!(intid >= 32, "unmask_spi: PPIs/SGIs not supported");

    let dist = DIST_BASE.load(Ordering::Relaxed);
    let (reg_off, bit) = spi_reg_bit(intid);

    mmio::write32(dist + GICD_ISENABLER + reg_off, 1 << bit);
}

/// Configure an SPI as edge-triggered (rising edge). Default is
/// level-sensitive. GICD_ICFGR uses 2 bits per INTID: bit [2n+1] = 1
/// selects edge-triggered. Must be called before unmasking.
pub fn configure_spi_edge(intid: u32) {
    debug_assert!(intid >= 32, "configure_spi_edge: PPIs/SGIs not supported");

    let dist = DIST_BASE.load(Ordering::Relaxed);
    let reg_index = (intid / 16) as usize;
    let field_shift = (intid % 16) * 2 + 1;
    let reg_addr = dist + GICD_ICFGR + reg_index * 4;
    let val = mmio::read32(reg_addr);

    mmio::write32(reg_addr, val | (1 << field_shift));
}

// ---------------------------------------------------------------------------
// SGI (Software Generated Interrupt) — inter-processor interrupt
// ---------------------------------------------------------------------------

/// Send an SGI to a specific core via GICv3 affinity routing.
///
/// `target_core` is the logical core ID (0–MAX_CORES). `sgi_id` is the
/// SGI number (0–15). The SGI is delivered as a Group 1 non-secure
/// interrupt via ICC_SGI1R_EL1.
///
/// # ICC_SGI1R_EL1 encoding
///
/// - bits [55:48] = Aff3
/// - bits [39:32] = Aff2
/// - bits [23:16] = Aff1
/// - bits [15:0]  = target list (bitmask within the Aff3.Aff2.Aff1 cluster)
/// - bits [27:24] = INTID (SGI number)
/// - bit [40]     = IRM (0 = use target list)
///
/// For this kernel, core_id maps directly to MPIDR Aff0 (Aff1–3 are zero
/// for <256 cores). The target list bitmask selects the core within Aff0.
pub fn send_sgi(target_core: u32, sgi_id: u32) {
    debug_assert!(sgi_id < 16, "send_sgi: SGI ID must be 0–15");
    debug_assert!(
        (target_core as usize) < crate::config::MAX_CORES,
        "send_sgi: target_core out of range"
    );

    // Target list: bit N in the 16-bit field selects Aff0=N within the
    // cluster identified by Aff3.Aff2.Aff1. Since all higher affinities
    // are zero, the target list is simply (1 << core_id).
    let target_list: u64 = 1 << (target_core & 0xF);
    let intid: u64 = (sgi_id as u64 & 0xF) << 24;
    // Aff1/2/3 are all zero for cores 0–255.
    let val = intid | target_list;

    // SAFETY: ICC_SGI1R_EL1 is a write-only system register that sends an
    // SGI. The value encodes the target core and SGI number. ISB ensures
    // the register write takes effect before we continue.
    sysreg::set_icc_sgi1r_el1(val);
    sysreg::isb();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redist_core_0_equals_base() {
        assert_eq!(
            redist_base_for_core(0),
            platform::device_addr(platform::GIC_REDIST_BASE)
        );
    }

    #[test]
    fn redist_core_3() {
        assert_eq!(
            redist_base_for_core(3),
            platform::device_addr(platform::GIC_REDIST_BASE + 3 * GICR_STRIDE),
        );
    }

    #[test]
    fn redist_stride_is_128k() {
        let diff = redist_base_for_core(1) - redist_base_for_core(0);

        assert_eq!(diff, 0x20000);
    }

    #[test]
    fn spi_reg_bit_intid_32() {
        let (reg_off, bit) = spi_reg_bit(32);

        assert_eq!(reg_off, 0x4);
        assert_eq!(bit, 0);
    }

    #[test]
    fn spi_reg_bit_intid_63() {
        let (reg_off, bit) = spi_reg_bit(63);

        assert_eq!(reg_off, 0x4);
        assert_eq!(bit, 31);
    }

    #[test]
    fn spi_reg_bit_intid_64() {
        let (reg_off, bit) = spi_reg_bit(64);

        assert_eq!(reg_off, 0x8);
        assert_eq!(bit, 0);
    }

    #[test]
    fn sgi_reschedule_is_zero() {
        assert_eq!(SGI_RESCHEDULE, 0);
    }
}
