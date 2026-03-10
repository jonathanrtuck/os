//! ARM GICv2 interrupt controller.
//!
//! Base addresses are set at boot from the DTB (or hardcoded defaults for
//! QEMU `virt`). Uses `AtomicUsize` for the bases — written once during init,
//! read on every interrupt.
//!
//! `init_distributor()` configures the shared distributor (core 0 only).
//! `init_cpu_interface()` configures the per-core CPU interface (every core).

use super::memory::KERNEL_VA_OFFSET;
use super::memory_mapped_io;
use core::sync::atomic::{AtomicUsize, Ordering};

/// GIC register offsets from distributor / CPU interface base.
const CTLR: usize = 0x0000;
const PMR: usize = 0x0004;
const IAR: usize = 0x000C;
const EOIR: usize = 0x0010;
const ICENABLER: usize = 0x180;
const ISENABLER: usize = 0x100;
const IPRIORITYR: usize = 0x400;
const ITARGETSR: usize = 0x800;
const SPURIOUS: u32 = 1023;
/// Default QEMU `virt` addresses — used if DTB is unavailable.
const DEFAULT_GICC_PA: usize = 0x0801_0000;
const DEFAULT_GICD_PA: usize = 0x0800_0000;

static GICC_BASE: AtomicUsize = AtomicUsize::new(DEFAULT_GICC_PA + KERNEL_VA_OFFSET);
static GICD_BASE: AtomicUsize = AtomicUsize::new(DEFAULT_GICD_PA + KERNEL_VA_OFFSET);

fn gicc() -> usize {
    GICC_BASE.load(Ordering::Relaxed)
}
fn gicd() -> usize {
    GICD_BASE.load(Ordering::Relaxed)
}

pub fn acknowledge() -> Option<u32> {
    // DSB SY before reading IAR: ensure all previous memory accesses
    // (including device writes) complete before acknowledging the interrupt.
    // GIC MMIO is outer-shareable device memory, requiring full system barrier.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };

    let iar = memory_mapped_io::read32(gicc() + IAR);
    let id = iar & 0x3FF;

    if id == SPURIOUS {
        None
    } else {
        Some(iar)
    }
}
/// Disable (mask) an IRQ at the distributor.
///
/// Writes to ICENABLER — clears the enable bit for a single IRQ without
/// affecting others. Used by interrupt forwarding to prevent re-triggering
/// until the userspace driver acknowledges.
pub fn disable_irq(id: u32) {
    let base = gicd();
    let reg_offset = (id / 32) as usize * 4;
    let bit = 1u32 << (id % 32);

    memory_mapped_io::write32(base + ICENABLER + reg_offset, bit);

    // DSB SY + ISB after distributor write: ensure the disable takes effect
    // before returning. Prevents a race where the IRQ fires again while the
    // kernel is still in the forwarding path.
    unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };
}
pub fn enable_irq(id: u32) {
    let base = gicd();
    let reg_offset = (id / 32) as usize * 4;
    let bit = 1u32 << (id % 32);

    // Route SPIs to CPU 0. PPIs (id < 32) have read-only ITARGETSR.
    if id >= 32 {
        memory_mapped_io::write8(base + ITARGETSR + id as usize, 0x01);
    }

    memory_mapped_io::write32(base + ISENABLER + reg_offset, bit);
    memory_mapped_io::write8(base + IPRIORITYR + id as usize, 0x80);

    // DSB SY + ISB after distributor writes: ensure enable takes effect
    // before any subsequent interrupt handling. Full system barrier because
    // GIC MMIO is outer-shareable device memory.
    unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)) };
}
pub fn end_of_interrupt(iar: u32) {
    memory_mapped_io::write32(gicc() + EOIR, iar);

    // DSB SY after EOIR write: ensure the end-of-interrupt is visible to
    // the GIC before we return. Prevents a stale interrupt from re-firing.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}
/// Initialize both distributor and CPU interface (convenience for core 0).
pub fn init() {
    init_distributor();
    init_cpu_interface();
}
/// Initialize the GIC CPU interface (per-core, call on every core).
pub fn init_cpu_interface() {
    let base = gicc();

    memory_mapped_io::write32(base + PMR, 0xFF);
    memory_mapped_io::write32(base + CTLR, 1);
}
/// Initialize the GIC distributor (global, core 0 only).
pub fn init_distributor() {
    memory_mapped_io::write32(gicd() + CTLR, 1);
}
/// Override GIC base addresses from the DTB.
///
/// Must be called before `init()` and before booting secondary cores.
/// Both arguments are physical addresses.
pub fn set_base_addresses(gicd_pa: u64, gicc_pa: u64) {
    GICD_BASE.store(gicd_pa as usize + KERNEL_VA_OFFSET, Ordering::Relaxed);
    GICC_BASE.store(gicc_pa as usize + KERNEL_VA_OFFSET, Ordering::Relaxed);
}
