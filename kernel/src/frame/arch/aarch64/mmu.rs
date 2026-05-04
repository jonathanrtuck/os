//! MMU setup — identity map with W^X permissions.
//!
//! Builds a two-level page table (L2 root → L3 for kernel block) using the
//! 16 KiB granule with 36-bit virtual addresses (T0SZ = 28).
//!
//! ## Table structure
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

static L2_ROOT: PageTablePage = PageTablePage(UnsafeCell::new([0; ENTRIES_PER_TABLE]));
static L3_KERNEL: PageTablePage = PageTablePage(UnsafeCell::new([0; ENTRIES_PER_TABLE]));

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

/// Populate the L2 root table: device MMIO blocks, kernel L3 table descriptor,
/// and remaining RAM blocks.
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

/// Build identity-mapped page tables and enable the MMU.
///
/// After this returns, VA == PA for all mapped regions. The kernel's text is
/// RX, rodata is RO, and data/bss/stack is RW. Device memory is mapped as
/// Device-nGnRnE. SCTLR.WXN enforces W^X in hardware.
pub fn init() {
    // SAFETY: Single-threaded init, tables are written before MMU enable.
    let l2 = unsafe { &mut *L2_ROOT.0.get() };
    let l3 = unsafe { &mut *L3_KERNEL.0.get() };
    let layout = KernelLayout {
        text_start: linker_addr(&raw const __text_start),
        text_end: linker_addr(&raw const __text_end),
        rodata_start: linker_addr(&raw const __rodata_start),
        rodata_end: linker_addr(&raw const __rodata_end),
        data_start: linker_addr(&raw const __data_start),
        kernel_end: linker_addr(&raw const __kernel_end),
    };
    let l3_pa = L3_KERNEL.0.get() as usize;

    build_l2(l2, l3_pa, platform::ram_base(), platform::ram_size());
    build_l3(l3, platform::ram_base(), &layout);

    configure_and_enable();
}

/// Enable the MMU on a secondary core using the BSP's page tables.
///
/// The BSP must have called [`init`] first — this function does NOT build
/// page tables. It configures this core's system registers to share the
/// existing L2/L3 tables and enables the MMU.
pub fn init_secondary() {
    configure_and_enable();
}

/// Program MAIR/TCR/TTBR0, invalidate TLBs, and enable the MMU.
///
/// Shared by both BSP ([`init`]) and secondary cores ([`init_secondary`]).
/// Page tables must already exist before this is called.
fn configure_and_enable() {
    // -----------------------------------------------------------------------
    // MAIR_EL1: memory attribute definitions
    // -----------------------------------------------------------------------
    let mair = MAIR_DEVICE_NGNRNE | (MAIR_NORMAL_WB << 8);

    sysreg::set_mair_el1(mair);

    // -----------------------------------------------------------------------
    // TCR_EL1: translation control
    // -----------------------------------------------------------------------
    // Read the hardware's physical address size from ID_AA64MMFR0_EL1[3:0].
    // The PARange field encodes the supported PA width (32, 36, 40, 42, 44,
    // 48, or 52 bits). We use this directly as TCR_EL1.IPS — the encodings
    // are identical by design.
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
        // TTBR0 is free for per-process user address spaces.
        | (0b01    << 24)  // IRGN1: Inner Write-Back Write-Allocate
        | (0b01    << 26)  // ORGN1: Outer Write-Back Write-Allocate
        | (0b11    << 28)  // SH1: Inner Shareable
        | (0b01    << 30)  // TG1: 16 KiB granule
        | (pa_range << 32); // IPS: from hardware (ID_AA64MMFR0_EL1.PARange)

    sysreg::set_tcr_el1(tcr);

    // -----------------------------------------------------------------------
    // TTBR0/TTBR1: kernel runs from both halves (identity map)
    // -----------------------------------------------------------------------
    // TTBR1 points to the kernel's L2 root — the same table as TTBR0 for now.
    // After user address spaces are created, TTBR0 will be switched per-process
    // while TTBR1 stays fixed on the kernel table.
    let l2_pa = L2_ROOT.0.get() as u64;

    sysreg::set_ttbr0_el1(l2_pa);
    sysreg::set_ttbr1_el1(l2_pa);

    // -----------------------------------------------------------------------
    // Invalidate TLBs and enable MMU
    // -----------------------------------------------------------------------

    // Ensure system register writes (MAIR, TCR, TTBR) take effect.
    sysreg::isb();

    // Ensure page table stores are visible to hardware walkers before TLBI.
    // DSB ISHST (store-only) is the ARM ARM D5.10 recommended pre-TLBI barrier.
    sysreg::dsb_ishst();

    // Invalidate stale TLB entries (defensive — none should exist before
    // first enable, but firmware or EL2 may have populated speculative entries).
    // IS (inner-shareable) is deliberate: PSCI leaves TLB state IMPLEMENTATION
    // DEFINED, so we broadcast to handle hypervisors with shared TLB structures.
    sysreg::tlbi_vmalle1is();
    sysreg::dsb_ish();
    sysreg::isb();

    // Enable MMU, caches, and W^X enforcement via assembly trampoline.
    // The trampoline is the single transition point from physical to virtual
    // addressing — see mmu.S for why this must be in assembly.
    let mut sctlr = sysreg::sctlr_el1();

    sctlr |= 1 << 0; // M: MMU enable
    sctlr |= 1 << 2; // C: data cache enable
    sctlr |= 1 << 12; // I: instruction cache enable
    sctlr |= 1 << 19; // WXN: write-implies-XN (hardware W^X)

    unsafe extern "C" {
        fn __mmu_enable(sctlr: u64);
    }
    // SAFETY: Page tables are populated (by BSP's init or shared for
    // secondaries). MAIR/TCR/TTBR0 are configured above. TLBs are
    // invalidated. The identity map ensures VA == PA, so the trampoline can
    // return after enabling.
    unsafe { __mmu_enable(sctlr) };
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
}
