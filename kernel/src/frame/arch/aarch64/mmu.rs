//! MMU setup — upper-half kernel with W^X permissions.
//!
//! Builds page tables for a split address space:
//!
//! - **TTBR0** (lower half): identity map of RAM + device MMIO. Retained
//!   during boot for the trampoline and device access, then switched
//!   per-process for user address spaces.
//! - **TTBR1** (upper half): kernel text, rodata, data, device MMIO, and
//!   free RAM. All kernel code and data runs from TTBR1 after the MMU
//!   enable trampoline branches to the upper-half entry point.
//!
//! 16 KiB granule with 36-bit virtual addresses (T0SZ = T1SZ = 28).
//!
//! ## TTBR1 table structure
//!
//! - **L2 root** (2048 entries, each covers 32 MiB):
//!   - Indices 4–5: device MMIO blocks (GIC, UART, virtio)
//!   - Index 32: table descriptor → L3 table for kernel's 32 MiB block
//!   - Indices 33–39: RAM blocks (normal, RW)
//!
//! - **L3 kernel** (2048 entries, each covers 16 KiB):
//!   - Pages before kernel: RW (DTB, pre-kernel RAM)
//!   - Kernel text: RO + executable (W^X: writable ⊕ executable)
//!   - Kernel rodata: RO, no execute
//!   - Kernel data/bss/stack: RW, no execute
//!
//! The TTBR1 L2 indices are the same as the physical addresses because
//! VA = PA + KERNEL_VA_OFFSET and the L2 index depends only on bits
//! [35:25] which are identical for both (the offset only affects bits
//! above 36).
//!
//! ## Memory attributes (MAIR_EL1)
//!
//! - Index 0: Device-nGnRnE (0x00)
//! - Index 1: Normal, Inner/Outer Write-Back Write-Allocate (0xFF)

use core::cell::UnsafeCell;

use super::{platform, sysreg};

#[cfg(target_os = "none")]
core::arch::global_asm!(include_str!("mmu.S"));

// ---------------------------------------------------------------------------
// Descriptor constants
// ---------------------------------------------------------------------------

const VALID: u64 = 1 << 0;
const TABLE: u64 = 1 << 1; // L2 table descriptor
const PAGE: u64 = 1 << 1; // L3 page descriptor (same bit position)
const AF: u64 = 1 << 10;
const SH_ISH: u64 = 0b11 << 8;
const AP_RW_EL1: u64 = 0b00 << 6;
const AP_RO_EL1: u64 = 0b10 << 6;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ATTR_DEVICE: u64 = 0 << 2; // MAIR index 0
const ATTR_NORMAL: u64 = 1 << 2; // MAIR index 1

// MAIR encodings
const MAIR_DEVICE_NGNRNE: u64 = 0x00;
const MAIR_NORMAL_WB: u64 = 0xFF; // Inner/Outer Write-Back, Write-Allocate

// Page geometry — 16 KiB granule (Apple Silicon native). These stay private
// to the MMU module; the page size does not leak into the kernel interface.
const PAGE_SIZE: usize = 16 * 1024;
#[allow(dead_code)]
const PAGE_SHIFT: usize = 14;

// L2 geometry
const L2_BLOCK_SHIFT: usize = 25;
const L2_BLOCK_SIZE: usize = 1 << L2_BLOCK_SHIFT;
const ENTRIES_PER_TABLE: usize = PAGE_SIZE / 8;

// ---------------------------------------------------------------------------
// Static page tables
// ---------------------------------------------------------------------------

#[repr(C, align(16384))]
struct PageTablePage(UnsafeCell<[u64; ENTRIES_PER_TABLE]>);

// SAFETY: Page tables are only written during single-threaded init before the
// MMU is enabled. After init, they are read-only (the MMU walker reads them
// via the hardware page table walk, not through Rust references).
unsafe impl Sync for PageTablePage {}

// TTBR0: identity map (boot trampoline + device MMIO + RAM).
static L2_TTBR0: PageTablePage = PageTablePage(UnsafeCell::new([0; ENTRIES_PER_TABLE]));
static L3_TTBR0_KERNEL: PageTablePage = PageTablePage(UnsafeCell::new([0; ENTRIES_PER_TABLE]));

// TTBR1: upper-half kernel map.
static L2_TTBR1: PageTablePage = PageTablePage(UnsafeCell::new([0; ENTRIES_PER_TABLE]));
static L3_TTBR1_KERNEL: PageTablePage = PageTablePage(UnsafeCell::new([0; ENTRIES_PER_TABLE]));

// ---------------------------------------------------------------------------
// Descriptor builders
// ---------------------------------------------------------------------------

fn l2_block(pa: usize, attrs: u64) -> u64 {
    (pa as u64 & !((L2_BLOCK_SIZE as u64) - 1)) | attrs | SH_ISH | AF | VALID
}

fn l2_table_desc(table_pa: usize) -> u64 {
    (table_pa as u64 & !((PAGE_SIZE as u64) - 1)) | TABLE | VALID
}

fn l3_page(pa: usize, attrs: u64) -> u64 {
    (pa as u64 & !((PAGE_SIZE as u64) - 1)) | attrs | SH_ISH | AF | PAGE | VALID
}

#[inline]
fn l2_index(va: usize) -> usize {
    (va >> L2_BLOCK_SHIFT) & (ENTRIES_PER_TABLE - 1)
}

#[allow(dead_code)]
#[inline]
fn l3_index(va: usize) -> usize {
    (va >> PAGE_SHIFT) & (ENTRIES_PER_TABLE - 1)
}

// ---------------------------------------------------------------------------
// Linker symbols (defined in link.ld)
// ---------------------------------------------------------------------------

// SAFETY: These symbols are defined by the linker script (link.ld) at
// section boundaries. They are never dereferenced — only their addresses
// are taken via `&raw const` to determine physical layout.
unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

fn linker_addr(sym: *const u8) -> usize {
    sym as usize
}

// ---------------------------------------------------------------------------
// Page table builders (safe — no global state, testable on host)
// ---------------------------------------------------------------------------

/// Physical address ranges of the kernel's ELF sections.
struct KernelLayout {
    text_start: usize,
    text_end: usize,
    rodata_start: usize,
    rodata_end: usize,
    data_start: usize,
    kernel_end: usize,
}

/// W^X permission policy: map a physical address to page attributes.
///
/// Every mapped page is either writable or executable, never both.
/// Pages beyond `kernel_end` are mapped RW/NX for dynamic allocation
/// (page tables, kernel heap, etc.).
#[allow(clippy::if_same_then_else)]
fn page_attrs(pa: usize, layout: &KernelLayout) -> Option<u64> {
    if pa >= layout.text_start && pa < layout.text_end {
        Some(ATTR_NORMAL | AP_RO_EL1 | UXN)
    } else if pa >= layout.rodata_start && pa < layout.rodata_end {
        Some(ATTR_NORMAL | AP_RO_EL1 | PXN | UXN)
    } else if pa >= layout.data_start && pa < layout.kernel_end {
        Some(ATTR_NORMAL | AP_RW_EL1 | PXN | UXN)
    } else if pa < layout.text_start {
        Some(ATTR_NORMAL | AP_RW_EL1 | PXN | UXN)
    } else {
        // Beyond kernel_end: free RAM for dynamic allocation (page tables,
        // kernel objects, heap). Map as RW/NX.
        Some(ATTR_NORMAL | AP_RW_EL1 | PXN | UXN)
    }
}

/// Populate an L2 table with device MMIO, kernel L3 descriptor, and RAM blocks.
#[allow(clippy::needless_range_loop)]
fn build_l2(table: &mut [u64; ENTRIES_PER_TABLE], l3_pa: usize, ram_base: usize, ram_size: usize) {
    let device_attrs = ATTR_DEVICE | AP_RW_EL1 | PXN | UXN;

    for idx in l2_index(platform::GIC_DIST_BASE)..=l2_index(0x0BFF_FFFF) {
        table[idx] = l2_block(idx * L2_BLOCK_SIZE, device_attrs);
    }

    table[l2_index(ram_base)] = l2_table_desc(l3_pa);

    let ram_rw = ATTR_NORMAL | AP_RW_EL1 | PXN | UXN;
    let ram_start_idx = l2_index(ram_base) + 1;
    let ram_end_idx = l2_index(ram_base + ram_size - 1).min(ENTRIES_PER_TABLE - 1);

    for idx in ram_start_idx..=ram_end_idx {
        table[idx] = l2_block(idx * L2_BLOCK_SIZE, ram_rw);
    }
}

/// Populate the L3 table for the kernel's 32 MiB block using [`page_attrs`].
#[allow(clippy::needless_range_loop)]
fn build_l3(table: &mut [u64; ENTRIES_PER_TABLE], block_base: usize, layout: &KernelLayout) {
    for i in 0..ENTRIES_PER_TABLE {
        let pa = block_base + i * PAGE_SIZE;

        if let Some(attrs) = page_attrs(pa, layout) {
            table[i] = l3_page(pa, attrs);
        }
    }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Build page tables and enable the MMU.
///
/// After this returns, the kernel runs from the TTBR1 upper-half address
/// space. TTBR0 retains the identity map for device MMIO access (the page
/// table switch for user address spaces replaces TTBR0 later).
pub fn init() {
    let ram_base = platform::ram_base();
    let ram_size = platform::ram_size();
    // SAFETY: Single-threaded init, tables are written before MMU enable.
    let l2_t0 = unsafe { &mut *L2_TTBR0.0.get() };
    let l3_t0 = unsafe { &mut *L3_TTBR0_KERNEL.0.get() };
    let l2_t1 = unsafe { &mut *L2_TTBR1.0.get() };
    let l3_t1 = unsafe { &mut *L3_TTBR1_KERNEL.0.get() };
    // At boot time, linker_addr uses adrp/add (PC-relative) which resolves
    // to physical addresses — the VA_OFFSET cancels in the subtraction.
    // No virt_to_phys needed; the addresses are already physical.
    let layout = KernelLayout {
        text_start: linker_addr(&raw const __text_start),
        text_end: linker_addr(&raw const __text_end),
        rodata_start: linker_addr(&raw const __rodata_start),
        rodata_end: linker_addr(&raw const __rodata_end),
        data_start: linker_addr(&raw const __data_start),
        kernel_end: linker_addr(&raw const __kernel_end),
    };
    let l3_t0_pa = L3_TTBR0_KERNEL.0.get() as usize;
    let l3_t1_pa = L3_TTBR1_KERNEL.0.get() as usize;

    // TTBR0: identity map for boot trampoline and device MMIO.
    build_l2(l2_t0, l3_t0_pa, ram_base, ram_size);
    build_l3(l3_t0, ram_base, &layout);

    // TTBR1: identical physical mapping, but accessed at upper-half VAs.
    build_l2(l2_t1, l3_t1_pa, ram_base, ram_size);
    build_l3(l3_t1, ram_base, &layout);

    // SAFETY: kernel_main_upper is a #[no_mangle] extern "C" fn defined in
    // main.rs. We only take its address to compute the upper-half continuation.
    unsafe extern "C" {
        fn kernel_main_upper();
    }

    configure_and_enable(kernel_main_upper as *const () as usize);
}

/// Enable the MMU on a secondary core using the BSP's page tables.
///
/// The BSP must have called [`init`] first — this function does NOT build
/// page tables. Branches to `secondary_main_upper` at the upper-half VA
/// after enabling.
pub fn init_secondary() {
    // SAFETY: secondary_main_upper is a #[no_mangle] extern "C" fn defined
    // in cpu.rs. We only take its address to compute the upper-half continuation.
    unsafe extern "C" {
        fn secondary_main_upper();
    }

    configure_and_enable(secondary_main_upper as *const () as usize);
}

/// Program MAIR/TCR/TTBR0/TTBR1, invalidate TLBs, and enable the MMU.
///
/// Shared by both BSP ([`init`]) and secondary cores ([`init_secondary`]).
/// Page tables must already exist before this is called.
fn configure_and_enable(continuation_pa: usize) {
    // -----------------------------------------------------------------------
    // MAIR_EL1: memory attribute definitions
    // -----------------------------------------------------------------------
    let mair = MAIR_DEVICE_NGNRNE | (MAIR_NORMAL_WB << 8);

    sysreg::set_mair_el1(mair);

    // -----------------------------------------------------------------------
    // TCR_EL1: translation control
    // -----------------------------------------------------------------------
    let pa_range = sysreg::id_aa64mmfr0_el1() & 0xF;
    #[allow(clippy::identity_op)]
    #[rustfmt::skip]
    let tcr: u64 =
          (28      <<  0)  // T0SZ = 28: 36-bit VA (64 GiB)
        | (0b01    <<  8)  // IRGN0: Inner Write-Back Write-Allocate
        | (0b01    << 10)  // ORGN0: Outer Write-Back Write-Allocate
        | (0b11    << 12)  // SH0: Inner Shareable
        | (0b10    << 14)  // TG0: 16 KiB granule
        | (28      << 16)  // T1SZ = 28
        // EPD1 cleared (0): TTBR1 walks enabled for kernel upper-half VA.
        | (0b01    << 24)  // IRGN1: Inner Write-Back Write-Allocate
        | (0b01    << 26)  // ORGN1: Outer Write-Back Write-Allocate
        | (0b11    << 28)  // SH1: Inner Shareable
        | (0b01    << 30)  // TG1: 16 KiB granule
        | (pa_range << 32); // IPS: from hardware (ID_AA64MMFR0_EL1.PARange)

    sysreg::set_tcr_el1(tcr);

    // -----------------------------------------------------------------------
    // TTBR0: identity map (boot trampoline, device MMIO, RAM)
    // TTBR1: upper-half kernel
    // -----------------------------------------------------------------------
    let ttbr0_pa = L2_TTBR0.0.get() as u64;
    let ttbr1_pa = L2_TTBR1.0.get() as u64;

    sysreg::set_ttbr0_el1(ttbr0_pa);
    sysreg::set_ttbr1_el1(ttbr1_pa);

    // -----------------------------------------------------------------------
    // Invalidate TLBs and enable MMU
    // -----------------------------------------------------------------------

    sysreg::isb();
    sysreg::dsb_ishst();
    sysreg::tlbi_vmalle1is();
    sysreg::dsb_ish();
    sysreg::isb();

    // Enable MMU, caches, and W^X enforcement.
    let mut sctlr = sysreg::sctlr_el1();

    sctlr |= 1 << 0; // M: MMU enable
    sctlr |= 1 << 2; // C: data cache enable
    sctlr |= 1 << 12; // I: instruction cache enable
    sctlr |= 1 << 19; // WXN: write-implies-XN (hardware W^X)

    // SAFETY: __mmu_enable is defined in mmu.S (.text.boot). It writes
    // SCTLR_EL1 to enable the MMU and returns. Declared here because Rust
    // cannot link to assembly symbols without an extern block.
    unsafe extern "C" {
        fn __mmu_enable(sctlr: u64);
    }

    // SAFETY: Page tables are populated. MAIR/TCR/TTBR are configured.
    // TLBs are invalidated. .text.boot is identity-mapped in TTBR0 so
    // the instruction after MSR SCTLR_EL1 resolves correctly.
    unsafe { __mmu_enable(sctlr) };

    // MMU is now on. We're still executing at the physical (TTBR0) address.
    // Relocate the stack to the upper-half VA, then branch to the upper-half
    // continuation. From that point on, all kernel code runs from TTBR1.
    // We never return to the PA caller — the PA call chain is discarded.
    platform::set_mmu_active();
    relocate_stack();

    let cont_va = platform::phys_to_virt(continuation_pa);

    // SAFETY: cont_va is the upper-half VA of kernel_main_upper, mapped
    // in TTBR1. After this branch, the kernel executes from TTBR1.
    unsafe {
        core::arch::asm!(
            "br {va}",
            va = in(reg) cont_va,
            options(noreturn),
        );
    }
}

fn relocate_stack() {
    let sp: usize;

    // SAFETY: Reads the current stack pointer. No side effects.
    unsafe { core::arch::asm!("mov {sp}, sp", sp = out(reg) sp, options(nostack)) };

    let new_sp = platform::phys_to_virt(sp);

    // SAFETY: The new SP points to the same physical memory via TTBR1.
    // All stack data is preserved.
    unsafe { core::arch::asm!("mov sp, {sp}", sp = in(reg) new_sp, options(nostack)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_layout() -> KernelLayout {
        KernelLayout {
            text_start: 0x4008_0000,
            text_end: 0x400C_0000,
            rodata_start: 0x400C_0000,
            rodata_end: 0x400E_0000,
            data_start: 0x400E_0000,
            kernel_end: 0x4012_0000,
        }
    }

    // -- W^X policy: the central safety property --

    #[test]
    fn wxn_no_page_is_writable_and_executable() {
        let layout = test_layout();
        let block_base = 0x4000_0000;

        for i in 0..ENTRIES_PER_TABLE {
            let pa = block_base + i * PAGE_SIZE;

            if let Some(attrs) = page_attrs(pa, &layout) {
                let writable = (attrs & AP_RO_EL1) == 0;
                let el1_executable = (attrs & PXN) == 0;

                assert!(!(writable && el1_executable), "W^X violation at PA {pa:#x}",);
            }
        }
    }

    // -- Region classification --

    #[test]
    fn text_is_readonly_executable() {
        let layout = test_layout();
        let attrs = page_attrs(layout.text_start, &layout).unwrap();

        assert_ne!(attrs & AP_RO_EL1, 0);
        assert_eq!(attrs & PXN, 0);
        assert_ne!(attrs & UXN, 0);
    }

    #[test]
    fn rodata_is_readonly_noexec() {
        let layout = test_layout();
        let attrs = page_attrs(layout.rodata_start, &layout).unwrap();

        assert_ne!(attrs & AP_RO_EL1, 0);
        assert_ne!(attrs & PXN, 0);
    }

    #[test]
    fn data_is_readwrite_noexec() {
        let layout = test_layout();
        let attrs = page_attrs(layout.data_start, &layout).unwrap();

        assert_eq!(attrs & AP_RO_EL1, 0);
        assert_ne!(attrs & PXN, 0);
    }

    #[test]
    fn pre_kernel_is_readwrite_noexec() {
        let layout = test_layout();
        let attrs = page_attrs(0x4000_0000, &layout).unwrap();

        assert_eq!(attrs & AP_RO_EL1, 0);
        assert_ne!(attrs & PXN, 0);
    }

    #[test]
    fn beyond_kernel_end_is_rw_noexec() {
        let layout = test_layout();
        let attrs = page_attrs(layout.kernel_end, &layout).unwrap();

        assert_eq!(attrs & AP_RO_EL1, 0, "should be writable");
        assert_ne!(attrs & PXN, 0, "should not be kernel-executable");
    }

    // -- Boundary precision --

    #[test]
    fn text_end_is_exclusive() {
        let layout = test_layout();
        let last_text = page_attrs(layout.text_end - PAGE_SIZE, &layout).unwrap();
        let first_rodata = page_attrs(layout.text_end, &layout).unwrap();

        assert_eq!(last_text & PXN, 0, "last text page is executable");
        assert_ne!(first_rodata & PXN, 0, "first rodata page is not executable");
    }

    // -- L3 builder --

    #[test]
    fn build_l3_maps_kernel_and_free_ram() {
        let mut table = [0u64; ENTRIES_PER_TABLE];
        let layout = test_layout();

        build_l3(&mut table, 0x4000_0000, &layout);

        let text_idx = (layout.text_start - 0x4000_0000) / PAGE_SIZE;

        assert_ne!(table[text_idx], 0);

        // Pages beyond kernel_end are now mapped (free RAM for allocation).
        let beyond_idx = (layout.kernel_end - 0x4000_0000) / PAGE_SIZE;

        assert_ne!(table[beyond_idx], 0);
    }

    // -- L2 builder --

    #[test]
    fn build_l2_device_region_is_mapped() {
        let mut table = [0u64; ENTRIES_PER_TABLE];

        build_l2(&mut table, 0x4100_0000, 0x4000_0000, 256 * 1024 * 1024);

        let gic_idx = l2_index(platform::GIC_DIST_BASE);

        assert_ne!(table[gic_idx], 0);
    }

    #[test]
    fn build_l2_kernel_block_is_table_descriptor() {
        let mut table = [0u64; ENTRIES_PER_TABLE];

        build_l2(&mut table, 0x4100_0000, 0x4000_0000, 256 * 1024 * 1024);

        let kernel_idx = l2_index(0x4000_0000);
        let entry = table[kernel_idx];

        assert_ne!(entry & TABLE, 0);
        assert_ne!(entry & VALID, 0);
    }

    // -- VA offset --

    #[test]
    fn phys_to_virt_roundtrip() {
        let pa = 0x4008_0000usize;
        let va = platform::phys_to_virt(pa);

        assert_eq!(platform::virt_to_phys(va), pa);
        assert_eq!(va, 0xFFFF_FFF0_4008_0000);
    }
}
