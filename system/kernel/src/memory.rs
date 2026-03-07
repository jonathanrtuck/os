use core::cell::SyncUnsafeCell;

const AF: u64 = 1 << 10;
const AP_RO: u64 = 1 << 7; // AP[2]=1 → read-only at EL1
const ATTRIDX0: u64 = 0 << 2; // normal memory (MAIR AttrIdx0)
const ATTRIDX1: u64 = 1 << 2; // device memory (MAIR AttrIdx1)
const DESC_BLOCK: u64 = 0 << 1;
const DESC_PAGE: u64 = 0b11; // valid page descriptor (L3)
const DESC_TABLE: u64 = 1 << 1;
const DESC_VALID: u64 = 1 << 0;
const PXN: u64 = 1 << 53;
const SH_INNER: u64 = 0b11 << 8;
const UXN: u64 = 1 << 54;
const PAGE_SIZE: u64 = 4096;
const BLOCK_2MB: u64 = 2 * 1024 * 1024;
const RAM_START: u64 = 0x4000_0000;
const RAM_SIZE: u64 = 256 * 1024 * 1024; // we force -m 256M in the run script

static TT_L0: SyncUnsafeCell<PageTable> = SyncUnsafeCell::new(PageTable::new());
static TT_L1: SyncUnsafeCell<PageTable> = SyncUnsafeCell::new(PageTable::new());
static TT_L2_0: SyncUnsafeCell<PageTable> = SyncUnsafeCell::new(PageTable::new()); // VA 0..1GB (devices)
static TT_L2_1: SyncUnsafeCell<PageTable> = SyncUnsafeCell::new(PageTable::new()); // VA 1..2GB (RAM)
static TT_L3_KERN: SyncUnsafeCell<PageTable> = SyncUnsafeCell::new(PageTable::new()); // kernel 2MB (4KB pages)

#[inline(always)]
fn align_up(x: u64, align: u64) -> u64 {
    (x + align - 1) & !(align - 1)
}

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
    static __stack_top: u8;

    fn enable_mmu(ttbr0: u64);
}

// AArch64 4k-page tables.
#[repr(align(4096))]
struct PageTable {
    entries: [u64; 512],
}

impl PageTable {
    const fn new() -> Self {
        Self { entries: [0; 512] }
    }
}

/// Map a 2MB device block (identity) into an L2 table.
fn map_device_block(table: &mut PageTable, va: u64) {
    let idx = ((va >> 21) & 0x1FF) as usize;

    table.entries[idx] =
        (va & 0xFFFF_FFFF_FFE0_0000) | DESC_VALID | DESC_BLOCK | ATTRIDX1 | AF | PXN | UXN;
}

fn build_tables() -> u64 {
    // Safety: single-core init, called once before MMU is enabled.
    unsafe {
        let l0 = &mut *TT_L0.get();
        let l1 = &mut *TT_L1.get();
        let l2_0 = &mut *TT_L2_0.get();
        let l2_1 = &mut *TT_L2_1.get();
        let l3_kern = &mut *TT_L3_KERN.get();

        l0.entries = [0; 512];
        l1.entries = [0; 512];
        l2_0.entries = [0; 512];
        l2_1.entries = [0; 512];
        l3_kern.entries = [0; 512];

        // L0[0] -> L1 (covers low VA range)
        l0.entries[0] = (TT_L1.get() as u64) | DESC_VALID | DESC_TABLE;
        // L1[0] (0..1GB) -> L2_0 (devices)
        l1.entries[0] = (TT_L2_0.get() as u64) | DESC_VALID | DESC_TABLE;
        // L1[1] (1..2GB) -> L2_1 (RAM at 0x4000_0000)
        l1.entries[1] = (TT_L2_1.get() as u64) | DESC_VALID | DESC_TABLE;

        // Device MMIO (identity-mapped 2MB blocks).
        map_device_block(l2_0, 0x0800_0000); // GIC (GICD + GICC)
        map_device_block(l2_0, 0x0900_0000); // UART

        // RAM: 2MB blocks, except the kernel's block which gets 4KB pages.
        let kernel_block = &__text_start as *const u8 as u64 & !(BLOCK_2MB - 1);
        let kernel_l2_idx = ((kernel_block >> 21) & 0x1FF) as usize;
        let blocks = RAM_SIZE / BLOCK_2MB;

        for i in 0..blocks {
            let pa = RAM_START + i * BLOCK_2MB;
            let idx = ((pa >> 21) & 0x1FF) as usize;

            if idx == kernel_l2_idx {
                // Kernel block → L3 table with per-section permissions.
                l2_1.entries[idx] = (TT_L3_KERN.get() as u64) | DESC_VALID | DESC_TABLE;

                continue;
            }

            l2_1.entries[idx] = (pa & 0xFFFF_FFFF_FFE0_0000)
                | DESC_VALID
                | DESC_BLOCK
                | ATTRIDX0
                | AF
                | SH_INNER
                | PXN
                | UXN;
        }

        // L3: 4KB pages for the kernel's 2MB block.
        // Section boundaries from the linker script (4KB-aligned).
        let text_start = &__text_start as *const u8 as u64;
        let text_end = align_up(&__text_end as *const u8 as u64, PAGE_SIZE);
        let rodata_start = &__rodata_start as *const u8 as u64;
        let rodata_end = align_up(&__rodata_end as *const u8 as u64, PAGE_SIZE);
        let data_start = &__data_start as *const u8 as u64;
        let kernel_end = align_up(&__kernel_end as *const u8 as u64, PAGE_SIZE);
        let normal = ATTRIDX0 | AF | SH_INNER;

        for i in 0..512u64 {
            let pa = kernel_block + i * PAGE_SIZE;

            let attrs = if pa >= text_start && pa < text_end {
                normal | AP_RO | UXN // .text: RX
            } else if pa >= rodata_start && pa < rodata_end {
                normal | AP_RO | PXN | UXN // .rodata: RO, no execute
            } else if pa >= data_start && pa < kernel_end {
                normal | PXN | UXN // .data/.bss/stack: RW, no execute
            } else if pa >= kernel_end {
                normal | PXN | UXN // post-kernel: RW (heap)
            } else {
                continue; // unmapped (before kernel)
            };

            l3_kern.entries[i as usize] = (pa & 0x0000_FFFF_FFFF_F000) | DESC_PAGE | attrs;
        }
    }

    TT_L0.get() as u64
}

pub fn init() {
    let ttbr0 = build_tables();

    unsafe { enable_mmu(ttbr0) };
}
