//! ARM GICv3 interrupt controller.
//!
//! Provides the `InterruptController` trait and `GicV3` implementation.
//! Static dispatch only — the global `GIC` instance is a concrete `GicV3`.
//!
//! CPU interface: system registers (ICC_*_EL1)
//! Distributor (GICD): MMIO at base from DTB
//! Redistributor (GICR): MMIO at gicr_base + core_id * 0x20000 (128 KiB per core)
//!
//! Base addresses are set at boot from the DTB. Uses `AtomicUsize` for the
//! bases — written once during init, read on every interrupt.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::{memory::KERNEL_VA_OFFSET, memory_mapped_io, per_core};

// ---------------------------------------------------------------------------
// InterruptController trait — 7 methods, static dispatch
// ---------------------------------------------------------------------------

/// Abstract interrupt controller interface.
///
/// Used via static dispatch on the concrete `GicV3` type.
pub trait InterruptController {
    /// Initialize the distributor (global, core 0 only).
    fn init_distributor(&self);
    /// Initialize the per-core CPU interface and redistributor.
    fn init_per_core(&self, core_id: u32);
    /// Acknowledge an interrupt. Returns `None` for spurious (ID 1023).
    fn acknowledge(&self) -> Option<u32>;
    /// Signal end of interrupt processing.
    fn end_of_interrupt(&self, irq_id: u32);
    /// Enable (unmask) an IRQ.
    fn enable_irq(&self, irq_id: u32);
    /// Disable (mask) an IRQ.
    fn disable_irq(&self, irq_id: u32);
    /// Send an inter-processor interrupt (SGI 0) to a target core.
    fn send_ipi(&self, target_core: u32);
}

// ---------------------------------------------------------------------------
// GICv3 constants
// ---------------------------------------------------------------------------

/// Spurious interrupt ID.
const SPURIOUS: u32 = 1023;

/// GICD register offsets.
const GICD_CTLR: usize = 0x0000;
const GICD_IGROUPR: usize = 0x0080;
const GICD_ISENABLER: usize = 0x0100;
const GICD_ICENABLER: usize = 0x0180;
const GICD_IPRIORITYR: usize = 0x0400;
const GICD_IROUTER: usize = 0x6100;

/// GICD_CTLR bits.
const GICD_CTLR_ENABLE_GRP1_NS: u32 = 1 << 1;
const GICD_CTLR_ARE_NS: u32 = 1 << 4;

/// GICR stride: 128 KiB per core.
const GICR_STRIDE: usize = 0x20000;

/// GICR register offsets (within per-core 128 KiB region).
const GICR_WAKER: usize = 0x0014;
/// SGI/PPI configuration base (offset within GICR region).
const GICR_SGI_BASE: usize = 0x10000;
const GICR_IGROUPR0: usize = GICR_SGI_BASE + 0x0080;
const GICR_ISENABLER0: usize = GICR_SGI_BASE + 0x0100;
const GICR_ICENABLER0: usize = GICR_SGI_BASE + 0x0180;
const GICR_IPRIORITYR: usize = GICR_SGI_BASE + 0x0400;

/// GICR_WAKER bits.
const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

/// Default QEMU `virt` GICv3 addresses — used if DTB is unavailable.
const DEFAULT_GICD_PA: usize = 0x0800_0000;
const DEFAULT_GICR_PA: usize = 0x080A_0000;

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static GICD_BASE: AtomicUsize = AtomicUsize::new(DEFAULT_GICD_PA + KERNEL_VA_OFFSET);
static GICR_BASE: AtomicUsize = AtomicUsize::new(DEFAULT_GICR_PA + KERNEL_VA_OFFSET);

/// Global GICv3 instance. Static dispatch — no vtable.
pub static GIC: GicV3 = GicV3;

// ---------------------------------------------------------------------------
// GicV3 struct
// ---------------------------------------------------------------------------

/// ARM GICv3 interrupt controller.
///
/// Stateless struct — all state lives in MMIO registers and atomics above.
pub struct GicV3;

impl GicV3 {
    fn gicd(&self) -> usize {
        GICD_BASE.load(Ordering::Relaxed)
    }

    fn gicr_for_core(&self, core_id: u32) -> usize {
        GICR_BASE.load(Ordering::Relaxed) + (core_id as usize) * GICR_STRIDE
    }
}

impl InterruptController for GicV3 {
    /// Initialize the GICv3 distributor (global, core 0 only).
    ///
    /// Disables the distributor, configures SPI priorities and affinity
    /// routing via IROUTER, then re-enables with ARE_NS=1.
    fn init_distributor(&self) {
        let base = self.gicd();

        // Disable distributor while configuring.
        memory_mapped_io::write32(base + GICD_CTLR, 0);

        // SAFETY: DSB SY ensures the disable is visible before configuration.
        // ISB flushes the pipeline. nomem intentionally omitted — DSB/ISB
        // have observable side effects (barrier semantics). nostack correct.
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };

        // Set all SPIs to Group 1 Non-Secure (IGROUPR). Each bit = 1 interrupt.
        // SPIs start at IRQ 32; IGROUPR[1] covers IRQs 32-63, etc.
        // Set all bits to 1 (Group 1 NS) so they're delivered via ICC_IAR1_EL1.
        for i in 1..8usize {
            memory_mapped_io::write32(base + GICD_IGROUPR + i * 4, 0xFFFF_FFFF);
        }

        // Configure SPI priorities (IRQs 32+). Set all to 0x80 (medium).
        // SPIs start at IPRIORITYR offset 32 (byte-indexed).
        for i in 32..256u32 {
            memory_mapped_io::write8(base + GICD_IPRIORITYR + i as usize, 0x80);
        }

        // Configure SPI routing via IROUTER (affinity-based, GICv3).
        // Route all SPIs to core 0 (Aff3:Aff2:Aff1:Aff0 = 0:0:0:0).
        for irq in 32..256u32 {
            let offset = GICD_IROUTER + (irq as usize - 32) * 8;
            // Write lower 32 bits (Aff0=0, Aff1=0) and upper 32 bits (Aff2=0, Aff3=0).
            memory_mapped_io::write32(base + offset, 0);
            memory_mapped_io::write32(base + offset + 4, 0);
        }

        // Enable distributor with affinity routing (ARE_NS=1) and Group 1 NS.
        memory_mapped_io::write32(
            base + GICD_CTLR,
            GICD_CTLR_ENABLE_GRP1_NS | GICD_CTLR_ARE_NS,
        );

        // SAFETY: DSB SY + ISB after enabling the distributor ensures the
        // CTLR write (with ARE_NS=1) is visible to the GIC before any IRQ
        // configuration. nomem intentionally omitted — barriers have side
        // effects. nostack correct.
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };
    }

    /// Initialize the per-core GICv3 CPU interface and redistributor.
    ///
    /// CRITICAL ORDERING: ICC_SRE_EL1 (SRE=1) must be written FIRST with
    /// ISB before any other ICC register access. This switches the CPU
    /// interface from MMIO (GICv2 compat) to system register mode.
    fn init_per_core(&self, core_id: u32) {
        // Step 1: Enable system register interface (SRE=1).
        // MUST be first — all other ICC register accesses require SRE mode.
        // SAFETY: Writing ICC_SRE_EL1 to enable system register access.
        // This is a hardware side-effect (switches CPU interface mode).
        // nomem intentionally omitted — MSR to ICC registers has side effects
        // that affect subsequent system register accesses. nostack correct.
        unsafe {
            core::arch::asm!(
                "mrs {tmp}, s3_0_c12_c12_5", // ICC_SRE_EL1
                "orr {tmp}, {tmp}, #1",       // SRE = bit 0
                "msr s3_0_c12_c12_5, {tmp}",  // ICC_SRE_EL1
                tmp = out(reg) _,
                options(nostack)
            );
        }

        // SAFETY: ISB is required after ICC_SRE_EL1 write before any other
        // ICC register access. Without this, subsequent MSR/MRS to ICC
        // registers may use the old (MMIO) interface. nostack correct.
        unsafe { core::arch::asm!("isb", options(nostack)) };

        // Step 2: Configure CPU interface registers (now in system register mode).

        // Set priority mask to accept all priorities.
        // SAFETY: Writing ICC_PMR_EL1 sets the priority threshold — a hardware
        // side effect. nomem intentionally omitted. nostack correct.
        unsafe {
            core::arch::asm!(
                "mov {tmp}, #0xFF",
                "msr s3_0_c4_c6_0, {tmp}",   // ICC_PMR_EL1
                tmp = out(reg) _,
                options(nostack)
            );
        }

        // Enable Group 1 interrupts at the CPU interface.
        // SAFETY: Writing ICC_CTLR_EL1 configures interrupt handling behavior.
        // nomem intentionally omitted — MSR has side effects. nostack correct.
        unsafe {
            core::arch::asm!(
                "mov {tmp}, #0",
                "msr s3_0_c12_c12_4, {tmp}",  // ICC_CTLR_EL1 (EOImode=0)
                tmp = out(reg) _,
                options(nostack)
            );
        }

        // Enable Group 1 Non-secure interrupts.
        // SAFETY: Writing ICC_IGRPEN1_EL1 enables interrupt delivery — a
        // hardware side effect. nomem intentionally omitted. nostack correct.
        unsafe {
            core::arch::asm!(
                "mov {tmp}, #1",
                "msr s3_0_c12_c12_7, {tmp}",  // ICC_IGRPEN1_EL1
                tmp = out(reg) _,
                options(nostack)
            );
        }

        // SAFETY: ISB ensures all CPU interface configuration is committed
        // before proceeding to GICR configuration. nostack correct.
        unsafe { core::arch::asm!("isb", options(nostack)) };

        // Step 3: Configure the per-core redistributor (GICR).
        let gicr = self.gicr_for_core(core_id);

        // Wake the redistributor: clear ProcessorSleep, poll ChildrenAsleep.
        let waker = memory_mapped_io::read32(gicr + GICR_WAKER);

        if waker & GICR_WAKER_PROCESSOR_SLEEP != 0 {
            memory_mapped_io::write32(gicr + GICR_WAKER, waker & !GICR_WAKER_PROCESSOR_SLEEP);

            // Poll until ChildrenAsleep clears (redistributor is awake).
            while memory_mapped_io::read32(gicr + GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP != 0 {
                core::hint::spin_loop();
            }
        }

        // Set all SGIs/PPIs to Group 1 Non-Secure (IGROUPR0).
        // All 32 bits = 1 (Group 1 NS) so they're delivered via ICC_IAR1_EL1.
        memory_mapped_io::write32(gicr + GICR_IGROUPR0, 0xFFFF_FFFF);

        // Configure SGI/PPI priorities (IRQs 0-31, byte-indexed).
        for i in 0..32u32 {
            memory_mapped_io::write8(gicr + GICR_IPRIORITYR + i as usize, 0x80);
        }

        // Enable SGI 0 (IPI) at the redistributor. The virtual timer PPI (27)
        // is enabled later by timer::init() via enable_irq(). PPI 30 (physical
        // timer) is also enabled here for legacy reasons but is unused — the
        // kernel uses the virtual timer (CNTV_*).
        // GICR_ISENABLER0 covers IRQs 0-31 (SGIs and PPIs).
        let enable_bits = (1u32 << 0) | (1u32 << 30); // SGI 0 + PPI 30 (legacy)
        memory_mapped_io::write32(gicr + GICR_ISENABLER0, enable_bits);

        // SAFETY: DSB SY + ISB after redistributor configuration ensures all
        // GICR writes are visible before returning. nostack correct.
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };
    }

    /// Acknowledge an interrupt by reading ICC_IAR1_EL1.
    ///
    /// Returns `None` for spurious interrupts (ID 1023).
    fn acknowledge(&self) -> Option<u32> {
        let iar: u64;

        // SAFETY: Reading ICC_IAR1_EL1 via MRS acknowledges the highest-priority
        // pending interrupt. This is a hardware side-effect (changes GIC state).
        // nomem intentionally omitted — MRS from ICC_IAR1_EL1 has observable
        // side effects (pops the interrupt from the pending queue, changes
        // running priority). LLVM must not reorder this past memory operations.
        // nostack correct.
        unsafe {
            core::arch::asm!(
                "mrs {iar}, s3_0_c12_c12_0", // ICC_IAR1_EL1
                iar = out(reg) iar,
                options(nostack)
            );
        }

        let id = iar as u32;

        if id == SPURIOUS {
            None
        } else {
            Some(id)
        }
    }

    /// Signal end of interrupt processing by writing ICC_EOIR1_EL1.
    fn end_of_interrupt(&self, irq_id: u32) {
        let id = irq_id as u64;

        // SAFETY: Writing ICC_EOIR1_EL1 signals EOI to the GIC — a hardware
        // side-effect (deactivates the interrupt, drops running priority).
        // nomem intentionally omitted — MSR to ICC register has side effects.
        // nostack correct.
        unsafe {
            core::arch::asm!(
                "msr s3_0_c12_c12_1, {id}", // ICC_EOIR1_EL1
                id = in(reg) id,
                options(nostack)
            );
        }

        // SAFETY: DSB SY after EOI write ensures the end-of-interrupt is
        // visible to the GIC before returning. Prevents a stale interrupt
        // from re-firing. nostack correct.
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    }

    /// Enable (unmask) an IRQ.
    ///
    /// For PPIs/SGIs (id < 32): configures via GICR on the current core.
    /// For SPIs (id >= 32): configures via GICD, routes to core 0.
    fn enable_irq(&self, id: u32) {
        if id < 32 {
            // PPI/SGI: configure at the redistributor of the current core.
            let core_id = per_core::core_id();
            let gicr = self.gicr_for_core(core_id);
            let bit = 1u32 << id;

            memory_mapped_io::write8(gicr + GICR_IPRIORITYR + id as usize, 0x80);
            memory_mapped_io::write32(gicr + GICR_ISENABLER0, bit);
        } else {
            // SPI: configure at the distributor.
            let base = self.gicd();
            let reg_offset = (id / 32) as usize * 4;
            let bit = 1u32 << (id % 32);

            // Set priority.
            memory_mapped_io::write8(base + GICD_IPRIORITYR + id as usize, 0x80);

            // Route via IROUTER to core 0 (Aff=0:0:0:0).
            let irouter_offset = GICD_IROUTER + (id as usize - 32) * 8;
            memory_mapped_io::write32(base + irouter_offset, 0);
            memory_mapped_io::write32(base + irouter_offset + 4, 0);

            // Enable the IRQ.
            memory_mapped_io::write32(base + GICD_ISENABLER + reg_offset, bit);
        }

        // SAFETY: DSB SY + ISB after distributor/redistributor writes ensures
        // the enable takes effect before any subsequent interrupt handling.
        // nostack correct.
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };
    }

    /// Disable (mask) an IRQ.
    ///
    /// For PPIs/SGIs (id < 32): disables via GICR on the current core.
    /// For SPIs (id >= 32): disables via GICD.
    fn disable_irq(&self, id: u32) {
        if id < 32 {
            // PPI/SGI: disable at the redistributor of the current core.
            let core_id = per_core::core_id();
            let gicr = self.gicr_for_core(core_id);
            let bit = 1u32 << id;

            memory_mapped_io::write32(gicr + GICR_ICENABLER0, bit);
        } else {
            // SPI: disable at the distributor.
            let base = self.gicd();
            let reg_offset = (id / 32) as usize * 4;
            let bit = 1u32 << (id % 32);

            memory_mapped_io::write32(base + GICD_ICENABLER + reg_offset, bit);
        }

        // SAFETY: DSB SY + ISB after disable write ensures the disable takes
        // effect before returning. Prevents a race where the IRQ fires again
        // while the kernel is still in the forwarding path. nostack correct.
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };
    }

    /// Send an inter-processor interrupt (SGI 0) to a target core.
    ///
    /// Uses ICC_SGI1R_EL1 with affinity encoding for the target core.
    /// QEMU virt uses flat affinity: Aff3=0, Aff2=0, Aff1=0, Aff0=core_id.
    fn send_ipi(&self, target_core: u32) {
        // Compute ICC_SGI1R_EL1 value:
        // - Bits [3:0] of target_core → bit position in target list (bits [15:0])
        // - SGI INTID = 0 (bits [27:24], already 0)
        // - Aff1 = 0 (bits [23:16])
        // - Aff2 = 0 (bits [39:32])
        // - Aff3 = 0 (bits [55:48])
        let target_list: u64 = 1u64 << (target_core & 0xF);

        // SAFETY: Writing ICC_SGI1R_EL1 triggers an SGI to the target core —
        // a hardware side-effect. nomem intentionally omitted — MSR to ICC
        // register has observable side effects (generates an interrupt on
        // the target core). nostack correct. DSB SY ensures the SGI write
        // completes before we return (ARM ARM: DSB after SGI generation
        // guarantees the redistributor has accepted the request).
        unsafe {
            core::arch::asm!(
                "msr s3_0_c12_c11_5, {val}", // ICC_SGI1R_EL1
                "dsb sy",
                val = in(reg) target_list,
                options(nostack)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Public API — convenience functions for global GIC instance
// ---------------------------------------------------------------------------

/// Initialize the distributor and per-core interface (convenience for core 0).
pub fn init() {
    GIC.init_distributor();
    GIC.init_per_core(0);
}

/// Override GIC base addresses from the DTB.
///
/// Must be called before `init()` and before booting secondary cores.
/// Both arguments are physical addresses.
pub fn set_base_addresses(gicd_pa: u64, gicr_pa: u64) {
    GICD_BASE.store(gicd_pa as usize + KERNEL_VA_OFFSET, Ordering::Relaxed);
    GICR_BASE.store(gicr_pa as usize + KERNEL_VA_OFFSET, Ordering::Relaxed);
}
