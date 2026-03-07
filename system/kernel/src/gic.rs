//! ARM GICv2 interrupt controller (QEMU `virt` memory map).

use super::memory::KERNEL_VA_OFFSET;
use super::mmio;

const GICC_BASE: usize = 0x0801_0000 + KERNEL_VA_OFFSET;
const GICC_CTLR: usize = GICC_BASE;
const GICC_PMR: usize = GICC_BASE + 0x0004;
const GICC_IAR: usize = GICC_BASE + 0x000C;
const GICC_EOIR: usize = GICC_BASE + 0x0010;
const GICD_BASE: usize = 0x0800_0000 + KERNEL_VA_OFFSET;
const GICD_CTLR: usize = GICD_BASE;
const GICD_ISENABLER0: usize = GICD_BASE + 0x100;
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400;
const SPURIOUS: u32 = 1023;

pub fn acknowledge() -> Option<u32> {
    let iar = mmio::read32(GICC_IAR);
    let id = iar & 0x3FF;

    if id == SPURIOUS {
        None
    } else {
        Some(iar)
    }
}
pub fn enable_irq(id: u32) {
    if id < 32 {
        mmio::write32(GICD_ISENABLER0, 1u32 << id);
        mmio::write8(GICD_IPRIORITYR + id as usize, 0x80);
    }
}
pub fn end_of_interrupt(iar: u32) {
    mmio::write32(GICC_EOIR, iar);
}
pub fn init() {
    mmio::write32(GICD_CTLR, 1);
    mmio::write32(GICC_PMR, 0xFF);
    mmio::write32(GICC_CTLR, 1);
}
