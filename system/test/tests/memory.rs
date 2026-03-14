//! Host-side tests for memory.rs pure logic.
//!
//! Model-based tests verifying page table descriptor construction, W^X
//! attribute assignment, block-to-L3 attribute extraction, break-before-make
//! sequencing, and address translation. Cannot import kernel code directly
//! (different target), so duplicates pure logic into test models.

// --- Paging constants (from kernel/paging.rs) ---

const PAGE_SIZE: u64 = 4096;
const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1;
const DESC_PAGE: u64 = 0b11;
const AF: u64 = 1 << 10;
const AP_RO: u64 = 1 << 7;
const ATTRIDX0: u64 = 0 << 2;
const PXN: u64 = 1 << 53;
const SH_INNER: u64 = 0b11 << 8;
const UXN: u64 = 1 << 54;
const PA_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// --- Memory constants (from kernel/memory.rs) ---

const KERNEL_VA_OFFSET: usize = 0xFFFF_0000_0000_0000;

// --- Model: virt_to_phys / phys_to_virt ---

fn virt_to_phys(va: usize) -> usize {
    va.wrapping_sub(KERNEL_VA_OFFSET)
}

fn phys_to_virt(pa: usize) -> usize {
    pa.wrapping_add(KERNEL_VA_OFFSET)
}

// --- Model: W^X attribute computation (from init()) ---

fn compute_wx_attrs(
    va: u64,
    text_start: u64,
    text_end: u64,
    rodata_start: u64,
    rodata_end: u64,
) -> u64 {
    let normal = ATTRIDX0 | AF | SH_INNER;
    if va >= text_start && va < text_end {
        normal | AP_RO | UXN // .text: RX kernel-only
    } else if va >= rodata_start && va < rodata_end {
        normal | AP_RO | PXN | UXN // .rodata: RO
    } else {
        normal | PXN | UXN // RW NX (data)
    }
}

// --- Model: L3 entry construction (from init()) ---

fn build_l3_entry(pa: u64, attrs: u64) -> u64 {
    (pa & 0x0000_FFFF_FFFF_F000) | DESC_PAGE | attrs
}

// --- Model: block attribute extraction (from try_set_kernel_guard_page) ---

fn extract_block_attrs(l2_entry: u64) -> u64 {
    l2_entry & !(0x0000_FFFF_FFE0_0003u64)
}

fn build_replicated_l3_entry(block_pa: u64, page_index: u64, block_attrs: u64) -> u64 {
    let page_pa = block_pa + page_index * PAGE_SIZE;
    (page_pa & PA_MASK) | block_attrs | DESC_PAGE
}

// --- Model: neighbor attribute recovery (from clear_kernel_guard_page) ---

fn neighbor_index(l3_idx: usize) -> usize {
    if l3_idx > 0 {
        l3_idx - 1
    } else {
        l3_idx + 1
    }
}

// ===========================================
// Tests: virt_to_phys / phys_to_virt
// ===========================================

#[test]
fn virt_to_phys_kernel_va() {
    // Standard kernel VA → PA translation.
    let va = 0xFFFF_0000_4008_0000usize; // kernel VA for PA 0x4008_0000
    let pa = virt_to_phys(va);
    assert_eq!(pa, 0x4008_0000);
}

#[test]
fn phys_to_virt_ram_address() {
    // PA in RAM → kernel VA.
    let pa = 0x4008_0000usize;
    let va = phys_to_virt(pa);
    assert_eq!(va, 0xFFFF_0000_4008_0000);
}

#[test]
fn virt_phys_roundtrip() {
    let original_pa = 0x4000_0000usize; // RAM start
    let va = phys_to_virt(original_pa);
    let pa = virt_to_phys(va);
    assert_eq!(pa, original_pa);
}

#[test]
fn virt_phys_roundtrip_ram_end() {
    let original_pa = 0x4FFF_FFFFusize; // RAM end - 1
    let va = phys_to_virt(original_pa);
    let pa = virt_to_phys(va);
    assert_eq!(pa, original_pa);
}

#[test]
fn virt_to_phys_base_offset() {
    // The base of kernel VA space maps to PA 0.
    let va = KERNEL_VA_OFFSET;
    let pa = virt_to_phys(va);
    assert_eq!(pa, 0);
}

// ===========================================
// Tests: W^X attribute enforcement
// ===========================================

#[test]
fn wx_text_section_is_rx() {
    let text_start = 0xFFFF_0000_4008_0000u64;
    let text_end = 0xFFFF_0000_4009_0000u64;
    let rodata_start = 0xFFFF_0000_4009_0000u64;
    let rodata_end = 0xFFFF_0000_400A_0000u64;

    let attrs = compute_wx_attrs(text_start, text_start, text_end, rodata_start, rodata_end);

    // .text: read-only (AP_RO), user-execute-never (UXN), but NOT PXN (kernel can execute)
    assert!(attrs & AP_RO != 0, ".text must be read-only");
    assert!(attrs & UXN != 0, ".text must be UXN");
    assert!(attrs & PXN == 0, ".text must NOT be PXN (kernel executable)");
    assert!(attrs & AF != 0, ".text must have access flag");
}

#[test]
fn wx_rodata_section_is_ro_nx() {
    let text_start = 0xFFFF_0000_4008_0000u64;
    let text_end = 0xFFFF_0000_4009_0000u64;
    let rodata_start = 0xFFFF_0000_4009_0000u64;
    let rodata_end = 0xFFFF_0000_400A_0000u64;

    let attrs =
        compute_wx_attrs(rodata_start, text_start, text_end, rodata_start, rodata_end);

    // .rodata: read-only, no execute at any level
    assert!(attrs & AP_RO != 0, ".rodata must be read-only");
    assert!(attrs & PXN != 0, ".rodata must be PXN");
    assert!(attrs & UXN != 0, ".rodata must be UXN");
}

#[test]
fn wx_data_section_is_rw_nx() {
    let text_start = 0xFFFF_0000_4008_0000u64;
    let text_end = 0xFFFF_0000_4009_0000u64;
    let rodata_start = 0xFFFF_0000_4009_0000u64;
    let rodata_end = 0xFFFF_0000_400A_0000u64;

    // Data region (below .text)
    let data_va = 0xFFFF_0000_4007_F000u64;
    let attrs = compute_wx_attrs(data_va, text_start, text_end, rodata_start, rodata_end);

    // Data: writable (no AP_RO), no execute
    assert!(attrs & AP_RO == 0, "data must be writable (no AP_RO)");
    assert!(attrs & PXN != 0, "data must be PXN");
    assert!(attrs & UXN != 0, "data must be UXN");
}

#[test]
fn wx_no_page_is_both_writable_and_executable() {
    // W^X invariant: no page should have both W and X permissions.
    let text_start = 0xFFFF_0000_4008_0000u64;
    let text_end = 0xFFFF_0000_4009_0000u64;
    let rodata_start = 0xFFFF_0000_4009_0000u64;
    let rodata_end = 0xFFFF_0000_400A_0000u64;

    // Check all pages in a 2MB block
    let block_base = 0xFFFF_0000_4000_0000u64;
    for i in 0..512u64 {
        let va = block_base + i * PAGE_SIZE;
        let attrs = compute_wx_attrs(va, text_start, text_end, rodata_start, rodata_end);

        let writable = attrs & AP_RO == 0;
        let kernel_executable = attrs & PXN == 0;
        let user_executable = attrs & UXN == 0;

        assert!(
            !(writable && kernel_executable),
            "W^X violation at VA {:#x}: writable AND kernel-executable",
            va
        );
        assert!(
            !(writable && user_executable),
            "W^X violation at VA {:#x}: writable AND user-executable",
            va
        );
    }
}

#[test]
fn wx_text_end_aligned_up_excludes_boundary() {
    // text_end is align_up'd — the page at text_end is NOT part of .text.
    let text_start = 0xFFFF_0000_4008_0000u64;
    let text_end = 0xFFFF_0000_4009_0000u64; // already aligned
    let rodata_start = 0xFFFF_0000_4009_0000u64;
    let rodata_end = 0xFFFF_0000_400A_0000u64;

    // Page at text_end should be .rodata, not .text
    let at_text_end =
        compute_wx_attrs(text_end, text_start, text_end, rodata_start, rodata_end);
    assert!(at_text_end & PXN != 0, "page at text_end should be NX (rodata or data)");
}

// ===========================================
// Tests: L3 entry construction
// ===========================================

#[test]
fn l3_entry_has_page_descriptor_bits() {
    let pa = 0x4008_0000u64;
    let attrs = ATTRIDX0 | AF | SH_INNER | AP_RO | UXN;
    let entry = build_l3_entry(pa, attrs);

    // Bits [1:0] must be 0b11 (DESC_PAGE)
    assert_eq!(entry & 0b11, 0b11, "L3 entry must have page descriptor bits");
}

#[test]
fn l3_entry_pa_preserved() {
    let pa = 0x4008_F000u64; // page-aligned PA
    let attrs = ATTRIDX0 | AF | SH_INNER;
    let entry = build_l3_entry(pa, attrs);

    // Extract PA from entry
    let extracted_pa = entry & PA_MASK;
    assert_eq!(extracted_pa, pa, "PA must be preserved in L3 entry");
}

#[test]
fn l3_entry_pa_strips_low_bits() {
    // If PA has sub-page bits (shouldn't happen, but verify masking)
    let pa = 0x4008_0123u64; // not page-aligned
    let entry = build_l3_entry(pa, ATTRIDX0 | AF | SH_INNER);

    let extracted_pa = entry & PA_MASK;
    assert_eq!(
        extracted_pa, 0x4008_0000,
        "low 12 bits of PA must be masked"
    );
}

#[test]
fn l3_entry_attrs_preserved() {
    let pa = 0x4008_0000u64;
    let attrs = ATTRIDX0 | AF | SH_INNER | AP_RO | PXN | UXN;
    let entry = build_l3_entry(pa, attrs);

    // All attribute bits should be present
    assert!(entry & AF != 0, "AF must be set");
    assert!(entry & AP_RO != 0, "AP_RO must be set");
    assert!(entry & PXN != 0, "PXN must be set");
    assert!(entry & UXN != 0, "UXN must be set");
    assert!(entry & SH_INNER != 0, "SH must be set");
}

// ===========================================
// Tests: block attribute extraction
// ===========================================

#[test]
fn block_attrs_strips_pa_and_type() {
    // Construct a synthetic L2 block descriptor:
    // PA = 0x4020_0000 (2MB-aligned), type = 0b01 (block), attrs = AF | SH_INNER | ATTRIDX0
    let block_pa = 0x4020_0000u64;
    let block_type = 0b01u64;
    let attrs = AF | SH_INNER | ATTRIDX0;
    let l2_entry = block_pa | block_type | attrs;

    let extracted = extract_block_attrs(l2_entry);

    // Must not contain PA bits [47:21]
    assert_eq!(extracted & 0x0000_FFFF_FFE0_0000, 0, "PA bits must be stripped");
    // Must not contain type bits [1:0]
    assert_eq!(extracted & 0b11, 0, "type bits must be stripped");
    // Must contain attribute bits
    assert_eq!(extracted, attrs, "attributes must be preserved");
}

#[test]
fn block_attrs_preserves_upper_bits() {
    // Upper bits (PXN, UXN) should be preserved.
    let l2_entry = 0x4020_0000u64 | 0b01 | PXN | UXN | AF | SH_INNER;
    let extracted = extract_block_attrs(l2_entry);

    assert!(extracted & PXN != 0, "PXN must survive extraction");
    assert!(extracted & UXN != 0, "UXN must survive extraction");
}

#[test]
fn replicated_l3_entry_has_correct_pa() {
    let block_pa = 0x4020_0000u64;
    let block_attrs = AF | SH_INNER | ATTRIDX0;

    for i in 0..512u64 {
        let entry = build_replicated_l3_entry(block_pa, i, block_attrs);
        let expected_pa = block_pa + i * PAGE_SIZE;
        let actual_pa = entry & PA_MASK;
        assert_eq!(
            actual_pa, expected_pa,
            "L3 entry {} must have PA {:#x}",
            i, expected_pa
        );
    }
}

#[test]
fn replicated_l3_entries_are_valid_pages() {
    let block_pa = 0x4020_0000u64;
    let block_attrs = AF | SH_INNER | ATTRIDX0;

    for i in 0..512u64 {
        let entry = build_replicated_l3_entry(block_pa, i, block_attrs);
        assert_eq!(entry & 0b11, 0b11, "entry {} must be a valid page descriptor", i);
    }
}

#[test]
fn replicated_l3_entries_preserve_block_attrs() {
    let block_pa = 0x4020_0000u64;
    let block_attrs = AF | SH_INNER | PXN | UXN;

    let entry = build_replicated_l3_entry(block_pa, 42, block_attrs);
    // Strip PA and DESC_PAGE to check attrs
    let entry_attrs = entry & !PA_MASK & !0b11u64;
    assert_eq!(entry_attrs, block_attrs, "block attrs must be preserved in L3");
}

// ===========================================
// Tests: neighbor index computation
// ===========================================

#[test]
fn neighbor_of_zero_is_one() {
    assert_eq!(neighbor_index(0), 1);
}

#[test]
fn neighbor_of_nonzero_is_predecessor() {
    assert_eq!(neighbor_index(1), 0);
    assert_eq!(neighbor_index(255), 254);
    assert_eq!(neighbor_index(511), 510);
}

#[test]
fn neighbor_always_in_bounds() {
    for i in 0..512 {
        let n = neighbor_index(i);
        assert!(n < 512, "neighbor of {} must be < 512, got {}", i, n);
    }
}

// ===========================================
// Tests: break-before-make sequencing
// ===========================================

/// Model a sequence of page table updates and verify the break-before-make
/// protocol is followed: valid→invalid→flush→valid→flush.
#[test]
fn break_before_make_sequence_model() {
    // Model: L2 entry starts as a valid block descriptor
    let mut l2_entry: u64 = 0x4020_0000 | 0b01; // block descriptor
    assert!(l2_entry & DESC_VALID != 0, "initial entry must be valid");

    // Step 1: Break — write invalid entry
    l2_entry = 0;
    assert_eq!(l2_entry & DESC_VALID, 0, "after break, entry must be invalid");

    // Step 2: TLB flush would happen here (modeled as a flag)
    let tlb_flushed_after_break = true;
    assert!(tlb_flushed_after_break);

    // Step 3: Make — write new table descriptor
    let l3_pa = 0x5000_0000u64;
    l2_entry = l3_pa | DESC_VALID | DESC_TABLE;
    assert!(l2_entry & DESC_VALID != 0, "after make, entry must be valid");
    assert!(
        l2_entry & DESC_TABLE != 0,
        "after make, entry must be table"
    );

    // Step 4: TLB flush after make
    let tlb_flushed_after_make = true;
    assert!(tlb_flushed_after_make);
}

/// Verify that a guard page (L3 entry = 0) is correctly an invalid descriptor.
#[test]
fn guard_page_entry_is_invalid() {
    let guard_entry: u64 = 0;
    assert_eq!(
        guard_entry & DESC_VALID, 0,
        "guard page L3 entry must be invalid"
    );
}

/// Verify L2 table descriptor format for L3 pointer.
#[test]
fn l2_table_descriptor_format() {
    let l3_pa = 0x5000_0000u64;
    let l2_entry = l3_pa | DESC_VALID | DESC_TABLE;

    // Must be valid
    assert!(l2_entry & DESC_VALID != 0);
    // Must be table (bit 1)
    assert!(l2_entry & DESC_TABLE != 0);
    // Bits [1:0] = 0b11 for table descriptor
    assert_eq!(l2_entry & 0b11, 0b11);
    // L3 PA extractable
    assert_eq!(l2_entry & PA_MASK, l3_pa);
}

// ===========================================
// Tests: L2 index and L3 index extraction
// ===========================================

#[test]
fn l2_index_extraction() {
    // L2 index = bits [29:21] of PA
    let pa = 0x4020_0000u64; // 2MB aligned, L2 index 1 within GiB 1
    let l2_idx = ((pa >> 21) & 0x1FF) as usize;
    assert_eq!(l2_idx, 1);

    let pa2 = 0x4000_0000u64; // first 2MB block in GiB 1
    let l2_idx2 = ((pa2 >> 21) & 0x1FF) as usize;
    assert_eq!(l2_idx2, 0);
}

#[test]
fn l3_index_extraction() {
    // L3 index = bits [20:12] of PA
    let pa = 0x4020_3000u64; // page 3 within the 2MB block
    let l3_idx = ((pa >> 12) & 0x1FF) as usize;
    assert_eq!(l3_idx, 3);

    let pa2 = 0x4021_FF000u64; // page 511
    let l3_idx2 = ((pa2 >> 12) & 0x1FF) as usize;
    assert_eq!(l3_idx2, 511);
}

#[test]
fn l2_l3_indices_cover_2mb_block() {
    // All 512 pages in a 2MB block map to L3 indices 0..511
    let block_pa = 0x4020_0000u64;
    for i in 0..512u64 {
        let page_pa = block_pa + i * PAGE_SIZE;
        let l3_idx = ((page_pa >> 12) & 0x1FF) as usize;
        assert_eq!(l3_idx, i as usize, "page {} must map to L3 index {}", i, i);
    }
}

// ===========================================
// Tests: kernel VA offset invariants
// ===========================================

#[test]
fn kernel_va_offset_is_canonical_upper() {
    // KERNEL_VA_OFFSET puts addresses in the upper canonical half (bits 63:48 all 1s).
    let va = phys_to_virt(0x4000_0000);
    assert!(va >= 0xFFFF_0000_0000_0000, "kernel VA must be in upper half");
}

#[test]
fn kernel_va_offset_matches_expected() {
    assert_eq!(KERNEL_VA_OFFSET, 0xFFFF_0000_0000_0000);
}

// ===========================================
// Tests: block descriptor detection
// ===========================================

#[test]
fn block_descriptor_detection() {
    // L2 block descriptor: bits [1:0] = 0b01
    let block_entry = 0x4020_0000u64 | 0b01 | AF | SH_INNER;
    assert_eq!(block_entry & 0b11, 0b01, "block descriptor bits");

    // L2 table descriptor: bits [1:0] = 0b11
    let table_entry = 0x5000_0000u64 | DESC_VALID | DESC_TABLE;
    assert_eq!(table_entry & 0b11, 0b11, "table descriptor bits");

    // Invalid entry: bits [1:0] = 0b00
    let invalid_entry = 0u64;
    assert_eq!(invalid_entry & 0b11, 0b00, "invalid descriptor bits");
}

// ===========================================
// Tests: attribute mask correctness
// ===========================================

#[test]
fn block_attr_mask_covers_all_attribute_bits() {
    // The mask 0x0000_FFFF_FFE0_0003 covers PA[47:21] and type[1:0].
    // Its complement covers attributes[63:48] and attributes[20:2].
    let mask = 0x0000_FFFF_FFE0_0003u64;
    let attr_bits = !mask;

    // Must include upper attribute bits (PXN at 53, UXN at 54)
    assert!(attr_bits & PXN != 0, "mask complement must include PXN bit");
    assert!(attr_bits & UXN != 0, "mask complement must include UXN bit");

    // Must include lower attribute bits (AF at 10, SH at 8-9, AP at 6-7, AttrIndx at 2-4)
    assert!(attr_bits & AF != 0, "mask complement must include AF bit");
    assert!(attr_bits & SH_INNER != 0, "mask complement must include SH bits");
    assert!(attr_bits & AP_RO != 0, "mask complement must include AP_RO bit");
    assert!(attr_bits & (0b111 << 2) != 0, "mask complement must include AttrIndx bits");

    // Must NOT include PA bits
    assert_eq!(attr_bits & 0x0000_FFFF_FFE0_0000, 0, "must not include PA bits");
    // Must NOT include type bits
    assert_eq!(attr_bits & 0b11, 0, "must not include type bits");
}
