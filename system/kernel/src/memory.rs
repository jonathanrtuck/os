const AF: u64 = 1 << 10;
const ATTRIDX0: u64 = 0 << 2; // normal memory
const ATTRIDX1: u64 = 1 << 2; // device memory
const DESC_BLOCK: u64 = 0 << 1;
const DESC_TABLE: u64 = 1 << 1;
const DESC_VALID: u64 = 1 << 0;
const PXN: u64 = 1 << 53;
const SH_INNER: u64 = 0b11 << 8;
const UXN: u64 = 1 << 54;
const PAGE_SIZE: u64 = 4096;
const RAM_START: u64 = 0x4000_0000;
const RAM_SIZE: u64 = 256 * 1024 * 1024; // we force -m 256M in the run script
const RAM_END: u64 = RAM_START + RAM_SIZE;

static mut TT_L0: PageTable = PageTable::new();
static mut TT_L1: PageTable = PageTable::new();
static mut TT_L2_0: PageTable = PageTable::new(); // VA 0..1GB (devices)
static mut TT_L2_1: PageTable = PageTable::new(); // VA 1..2GB (RAM)

#[inline(always)]
fn align_up(x: u64, align: u64) -> u64 {
    (x + align - 1) & !(align - 1)
}

extern "C" {
    static __stack_top: u8;

    fn enable_mmu(ttbr0: u64);
}

struct FrameAlloc {
    next: u64,
    end: u64,
}

// AArch64 4k-page tables.
#[repr(align(4096))]
struct PageTable {
    entries: [u64; 512],
}

impl FrameAlloc {
    fn new(start: u64, end: u64) -> Self {
        Self { next: start, end }
    }

    fn alloc(&mut self) -> Option<u64> {
        let p = self.next;
        if p + PAGE_SIZE > self.end {
            return None;
        }
        self.next += PAGE_SIZE;
        Some(p)
    }
}

impl PageTable {
    const fn new() -> Self {
        Self { entries: [0; 512] }
    }
}

/// Map a 2MB device block (identity) into an L2 table.
///
/// # Safety
/// `table` must point to a valid, writable PageTable.
unsafe fn map_device_block(table: *mut PageTable, va: u64) {
    let idx = ((va >> 21) & 0x1FF) as usize;

    (*table).entries[idx] =
        (va & 0xFFFF_FFFF_FFE0_0000) | DESC_VALID | DESC_BLOCK | ATTRIDX1 | AF | PXN | UXN;
}

fn build_tables() -> u64 {
    unsafe {
        TT_L0.entries = [0; 512];
        TT_L1.entries = [0; 512];
        TT_L2_0.entries = [0; 512];
        TT_L2_1.entries = [0; 512];

        // L0[0] -> L1 (covers low VA range)
        TT_L0.entries[0] = (&raw const TT_L1 as *const _ as u64) | DESC_VALID | DESC_TABLE;
        // L1[0] (0..1GB) -> L2_0 (devices)
        TT_L1.entries[0] = (&raw const TT_L2_0 as *const _ as u64) | DESC_VALID | DESC_TABLE;
        // L1[1] (1..2GB) -> L2_1 (RAM at 0x4000_0000)
        TT_L1.entries[1] = (&raw const TT_L2_1 as *const _ as u64) | DESC_VALID | DESC_TABLE;

        // Device MMIO (identity-mapped 2MB blocks).
        map_device_block(&raw mut TT_L2_0, 0x0800_0000); // GIC (GICD + GICC)
        map_device_block(&raw mut TT_L2_0, 0x0900_0000); // UART

        // RAM 0x4000_0000..0x5000_0000 as 2MB blocks (identity), normal memory.
        let blocks = RAM_SIZE / (2 * 1024 * 1024);

        for i in 0..blocks {
            let pa = RAM_START + i * 2 * 1024 * 1024;
            let idx = ((pa >> 21) & 0x1FF) as usize;

            TT_L2_1.entries[idx] =
                (pa & 0xFFFF_FFFF_FFE0_0000) | DESC_VALID | DESC_BLOCK | ATTRIDX0 | AF | SH_INNER;
        }
    }

    &raw const TT_L0 as *const _ as u64
}

pub fn init() {
    let ttbr0 = build_tables();

    unsafe { enable_mmu(ttbr0) };
}
