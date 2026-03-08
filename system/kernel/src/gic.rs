//! ARM GICv2 interrupt controller (QEMU `virt` memory map).
//!
//! `init_distributor()` configures the shared distributor (core 0 only).
//! `init_cpu_interface()` configures the per-core CPU interface (every core).

use super::memory::KERNEL_VA_OFFSET;
use super::mmio;

const GICC_BASE: usize = 0x0801_0000 + KERNEL_VA_OFFSET;
const GICC_CTLR: usize = GICC_BASE;
const GICC_PMR: usize = GICC_BASE + 0x0004;
const GICC_IAR: usize = GICC_BASE + 0x000C;
const GICC_EOIR: usize = GICC_BASE + 0x0010;
const GICD_BASE: usize = 0x0800_0000 + KERNEL_VA_OFFSET;
const GICD_CTLR: usize = GICD_BASE;
const GICD_ISENABLER: usize = GICD_BASE + 0x100;
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
    let reg_offset = (id / 32) as usize * 4;
    let bit = 1u32 << (id % 32);

    mmio::write32(GICD_ISENABLER + reg_offset, bit);
    mmio::write8(GICD_IPRIORITYR + id as usize, 0x80);
}
pub fn end_of_interrupt(iar: u32) {
    mmio::write32(GICC_EOIR, iar);
}
/// Initialize both distributor and CPU interface (convenience for core 0).
pub fn init() {
    init_distributor();
    init_cpu_interface();
}
/// Initialize the GIC CPU interface (per-core, call on every core).
pub fn init_cpu_interface() {
    mmio::write32(GICC_PMR, 0xFF);
    mmio::write32(GICC_CTLR, 1);
}
/// Initialize the GIC distributor (global, core 0 only).
pub fn init_distributor() {
    mmio::write32(GICD_CTLR, 1);
}
