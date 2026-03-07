//! ARM GICv2 interrupt controller (QEMU `virt` memory map).

use super::mmio;

// GICv2 memory map on QEMU `virt` (gic-version=2)
const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;
// GICD registers
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER0: usize = 0x100;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;
// GICC registers
const GICC_CTLR: usize = 0x0000;
const GICC_PMR: usize = 0x0004;
const GICC_IAR: usize = 0x000C;
const GICC_EOIR: usize = 0x0010;

/// Spurious interrupt ID returned by the GIC when no real interrupt is pending.
const SPURIOUS: u32 = 1023;

/// Enable the GIC distributor and CPU interface.
pub fn init() {
    mmio::write32(GICD_BASE + GICD_CTLR, 1);
    mmio::write32(GICC_BASE + GICC_PMR, 0xFF); // accept all priorities
    mmio::write32(GICC_BASE + GICC_CTLR, 1);
}

/// Enable a specific interrupt line (SGI/PPI, id < 32).
pub fn enable_irq(id: u32) {
    if id < 32 {
        mmio::write32(GICD_BASE + GICD_ISENABLER0, 1u32 << id);
        mmio::write8(GICD_BASE + GICD_IPRIORITYR + id as usize, 0x80); // middle priority
        mmio::write8(GICD_BASE + GICD_ITARGETSR + id as usize, 0x01); // route to CPU 0
    }
}

/// Read IAR and return the interrupt ID, or None for spurious interrupts.
pub fn acknowledge() -> Option<u32> {
    let iar = mmio::read32(GICC_BASE + GICC_IAR);
    let id = iar & 0x3FF;

    if id == SPURIOUS {
        None
    } else {
        Some(iar)
    }
}

/// Signal end-of-interrupt. Pass the value returned by `acknowledge`.
pub fn end_of_interrupt(iar: u32) {
    mmio::write32(GICC_BASE + GICC_EOIR, iar);
}
