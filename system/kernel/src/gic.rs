//! ARM GICv2 interrupt controller (QEMU `virt` memory map).

use super::mmio;

// GICv2 memory map on QEMU `virt` (gic-version=2).
// Absolute addresses — base + offset baked in.
const GICC_BASE: usize = 0x0801_0000;
const GICC_CTLR: usize = GICC_BASE + 0x0000;
const GICC_PMR: usize = GICC_BASE + 0x0004;
const GICC_IAR: usize = GICC_BASE + 0x000C;
const GICC_EOIR: usize = GICC_BASE + 0x0010;
const GICD_BASE: usize = 0x0800_0000;
const GICD_CTLR: usize = GICD_BASE + 0x000;
const GICD_ISENABLER0: usize = GICD_BASE + 0x100;
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400;
/// Spurious interrupt ID returned by the GIC when no real interrupt is pending.
const SPURIOUS: u32 = 1023;

/// Read IAR and return the interrupt ID, or None for spurious interrupts.
pub fn acknowledge() -> Option<u32> {
    let iar = mmio::read32(GICC_IAR);
    let id = iar & 0x3FF;

    if id == SPURIOUS {
        None
    } else {
        Some(iar)
    }
}

/// Enable a specific interrupt line (SGI/PPI, id < 32).
///
/// ITARGETSR is not set here — for SGIs/PPIs (0–31) the target registers
/// are read-only (banked per-CPU). SPI support (32+) would need a separate
/// path with ITARGETSR and a different ISENABLER bank.
pub fn enable_irq(id: u32) {
    if id < 32 {
        mmio::write32(GICD_ISENABLER0, 1u32 << id);
        mmio::write8(GICD_IPRIORITYR + id as usize, 0x80);
    }
}

/// Signal end-of-interrupt. Pass the value returned by `acknowledge`.
pub fn end_of_interrupt(iar: u32) {
    mmio::write32(GICC_EOIR, iar);
}

/// Enable the GIC distributor and CPU interface.
pub fn init() {
    mmio::write32(GICD_CTLR, 1);
    mmio::write32(GICC_PMR, 0xFF); // accept all priorities
    mmio::write32(GICC_CTLR, 1);
}
