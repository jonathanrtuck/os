//! Host-side tests for KASLR relocation processing.
//!
//! Tests the ELF64 R_AARCH64_RELATIVE relocation processor that adjusts
//! absolute addresses in the kernel binary by the KASLR slide delta.

#[path = "../../relocate.rs"]
mod relocate;

use relocate::*;

// =========================================================================
// Rela entry parsing
// =========================================================================

#[test]
fn rela_entry_from_bytes_valid() {
    let mut buf = [0u8; 24];
    // r_offset = 0x1000
    buf[0..8].copy_from_slice(&0x1000u64.to_le_bytes());
    // r_info = R_AARCH64_RELATIVE (0x403)
    buf[8..16].copy_from_slice(&0x403u64.to_le_bytes());
    // r_addend = 0xFFFF_FFF0_4000_0000 (a kernel VA)
    buf[16..24].copy_from_slice(&0xFFFF_FFF0_4000_0000u64.to_le_bytes());

    let entry = RelaEntry::from_le_bytes(&buf);
    assert_eq!(entry.offset, 0x1000);
    assert_eq!(entry.info, 0x403);
    assert_eq!(entry.addend, 0xFFFF_FFF0_4000_0000u64 as i64);
}

#[test]
fn is_relative_checks_type() {
    let relative = RelaEntry {
        offset: 0,
        info: R_AARCH64_RELATIVE,
        addend: 0,
    };
    assert!(relative.is_relative());

    let other = RelaEntry {
        offset: 0,
        info: R_AARCH64_ABS64,
        addend: 0,
    };
    assert!(!other.is_relative());
}

// =========================================================================
// Relocation application
// =========================================================================

#[test]
fn apply_single_relocation() {
    // Simulate a binary region with a pointer at offset 0x10.
    let mut binary = vec![0u8; 0x100];
    let original_va: u64 = 0xFFFF_FFF0_4000_1000;
    binary[0x10..0x18].copy_from_slice(&original_va.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x10,
        info: R_AARCH64_RELATIVE,
        addend: original_va as i64,
    };

    let slide: u64 = 0x20_0000; // 2 MiB slide
    apply_relocation(&mut binary, &rela, slide);

    let result = u64::from_le_bytes(binary[0x10..0x18].try_into().unwrap());
    assert_eq!(result, original_va + slide);
}

#[test]
fn apply_zero_slide_is_noop() {
    let mut binary = vec![0u8; 0x100];
    let original_va: u64 = 0xFFFF_FFF0_4000_1000;
    binary[0x10..0x18].copy_from_slice(&original_va.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x10,
        info: R_AARCH64_RELATIVE,
        addend: original_va as i64,
    };

    apply_relocation(&mut binary, &rela, 0);

    let result = u64::from_le_bytes(binary[0x10..0x18].try_into().unwrap());
    assert_eq!(result, original_va, "zero slide should not change the value");
}

#[test]
fn apply_multiple_relocations() {
    let mut binary = vec![0u8; 0x100];
    let va1: u64 = 0xFFFF_FFF0_4000_1000;
    let va2: u64 = 0xFFFF_FFF0_4000_2000;
    let va3: u64 = 0xFFFF_FFF0_4000_3000;
    binary[0x00..0x08].copy_from_slice(&va1.to_le_bytes());
    binary[0x20..0x28].copy_from_slice(&va2.to_le_bytes());
    binary[0x40..0x48].copy_from_slice(&va3.to_le_bytes());

    let relas = [
        RelaEntry { offset: 0x00, info: R_AARCH64_RELATIVE, addend: va1 as i64 },
        RelaEntry { offset: 0x20, info: R_AARCH64_RELATIVE, addend: va2 as i64 },
        RelaEntry { offset: 0x40, info: R_AARCH64_RELATIVE, addend: va3 as i64 },
    ];

    let slide: u64 = 0x40_0000; // 4 MiB

    for rela in &relas {
        apply_relocation(&mut binary, rela, slide);
    }

    assert_eq!(u64::from_le_bytes(binary[0x00..0x08].try_into().unwrap()), va1 + slide);
    assert_eq!(u64::from_le_bytes(binary[0x20..0x28].try_into().unwrap()), va2 + slide);
    assert_eq!(u64::from_le_bytes(binary[0x40..0x48].try_into().unwrap()), va3 + slide);
}

#[test]
fn skip_non_relative_relocations() {
    let mut binary = vec![0u8; 0x100];
    let original: u64 = 0xDEADBEEF;
    binary[0x10..0x18].copy_from_slice(&original.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x10,
        info: R_AARCH64_ABS64,
        addend: original as i64,
    };

    apply_relocation(&mut binary, &rela, 0x20_0000);

    // Non-relative entries should be left unchanged.
    let result = u64::from_le_bytes(binary[0x10..0x18].try_into().unwrap());
    assert_eq!(result, original, "non-relative relocation should be skipped");
}

#[test]
fn parse_rela_table() {
    // Build a table of 3 entries (24 bytes each = 72 bytes total).
    let mut table = Vec::new();
    for i in 0..3u64 {
        table.extend_from_slice(&(i * 0x10).to_le_bytes()); // offset
        table.extend_from_slice(&R_AARCH64_RELATIVE.to_le_bytes()); // info
        table.extend_from_slice(&(0xFFFF_0000_0000_0000u64.wrapping_add(i) as i64).to_le_bytes()); // addend
    }

    let entries = parse_rela_entries(&table);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].offset, 0x00);
    assert_eq!(entries[1].offset, 0x10);
    assert_eq!(entries[2].offset, 0x20);
}

#[test]
fn parse_rela_table_truncated_entry_ignored() {
    // 25 bytes = 1 full entry (24) + 1 byte (ignored).
    let mut table = vec![0u8; 25];
    table[8..16].copy_from_slice(&R_AARCH64_RELATIVE.to_le_bytes());

    let entries = parse_rela_entries(&table);
    assert_eq!(entries.len(), 1);
}

#[test]
fn parse_empty_rela_table() {
    let entries = parse_rela_entries(&[]);
    assert_eq!(entries.len(), 0);
}

// =========================================================================
// ABS64 relocation (--emit-relocs KASLR)
// =========================================================================

const KERNEL_VA_OFFSET: u64 = 0xFFFF_FFF0_0000_0000;

#[test]
fn is_abs64_checks_type() {
    let abs64 = RelaEntry {
        offset: 0,
        info: R_AARCH64_ABS64,
        addend: 0,
    };
    assert!(abs64.is_abs64());
    assert!(!abs64.is_relative());

    let relative = RelaEntry {
        offset: 0,
        info: R_AARCH64_RELATIVE,
        addend: 0,
    };
    assert!(!relative.is_abs64());
    assert!(relative.is_relative());
}

#[test]
fn abs64_adjusts_kernel_va() {
    let mut binary = vec![0u8; 0x100];
    let kernel_va: u64 = 0xFFFF_FFF0_4000_1000;
    binary[0x10..0x18].copy_from_slice(&kernel_va.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x10,
        info: R_AARCH64_ABS64,
        addend: 0,
    };

    let slide: u64 = 0x200_0000; // 32 MiB
    apply_abs64_relocation(&mut binary, &rela, slide, KERNEL_VA_OFFSET);

    let result = u64::from_le_bytes(binary[0x10..0x18].try_into().unwrap());
    assert_eq!(result, kernel_va + slide);
}

#[test]
fn abs64_skips_physical_address() {
    // SECONDARY_ENTRY_PA holds a physical address (e.g., 0x40080xxx).
    // It must NOT be adjusted by the KASLR slide.
    let mut binary = vec![0u8; 0x100];
    let phys_addr: u64 = 0x4008_0100;
    binary[0x20..0x28].copy_from_slice(&phys_addr.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x20,
        info: R_AARCH64_ABS64,
        addend: 0,
    };

    apply_abs64_relocation(&mut binary, &rela, 0x200_0000, KERNEL_VA_OFFSET);

    let result = u64::from_le_bytes(binary[0x20..0x28].try_into().unwrap());
    assert_eq!(result, phys_addr, "physical address should be unchanged");
}

#[test]
fn abs64_skips_non_abs64_type() {
    let mut binary = vec![0u8; 0x100];
    let kernel_va: u64 = 0xFFFF_FFF0_4000_5000;
    binary[0x30..0x38].copy_from_slice(&kernel_va.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x30,
        info: R_AARCH64_RELATIVE, // wrong type for abs64 processor
        addend: 0,
    };

    apply_abs64_relocation(&mut binary, &rela, 0x200_0000, KERNEL_VA_OFFSET);

    let result = u64::from_le_bytes(binary[0x30..0x38].try_into().unwrap());
    assert_eq!(result, kernel_va, "non-ABS64 entry should be skipped");
}

#[test]
fn abs64_zero_slide_is_noop() {
    let mut binary = vec![0u8; 0x100];
    let kernel_va: u64 = 0xFFFF_FFF0_4000_1000;
    binary[0x10..0x18].copy_from_slice(&kernel_va.to_le_bytes());

    let rela = RelaEntry {
        offset: 0x10,
        info: R_AARCH64_ABS64,
        addend: 0,
    };

    apply_abs64_relocation(&mut binary, &rela, 0, KERNEL_VA_OFFSET);

    let result = u64::from_le_bytes(binary[0x10..0x18].try_into().unwrap());
    assert_eq!(result, kernel_va, "zero slide should not change anything");
}

#[test]
fn abs64_table_mixed_entries() {
    // Simulate a .rela.dyn with 3 entries: 2 ABS64 + 1 RELATIVE.
    // One ABS64 targets a kernel VA, the other targets a physical address.
    let mut binary = vec![0u8; 0x100];
    let va1: u64 = 0xFFFF_FFF0_4000_1000; // kernel VA → adjust
    let pa1: u64 = 0x4008_0100;           // physical → skip
    let va2: u64 = 0xFFFF_FFF0_4000_2000; // kernel VA (RELATIVE, skip by abs64)
    binary[0x00..0x08].copy_from_slice(&va1.to_le_bytes());
    binary[0x20..0x28].copy_from_slice(&pa1.to_le_bytes());
    binary[0x40..0x48].copy_from_slice(&va2.to_le_bytes());

    let mut table = Vec::new();
    // Entry 0: ABS64 at offset 0x00, kernel VA target
    table.extend_from_slice(&0x00u64.to_le_bytes());
    table.extend_from_slice(&R_AARCH64_ABS64.to_le_bytes());
    table.extend_from_slice(&0i64.to_le_bytes());
    // Entry 1: ABS64 at offset 0x20, physical target
    table.extend_from_slice(&0x20u64.to_le_bytes());
    table.extend_from_slice(&R_AARCH64_ABS64.to_le_bytes());
    table.extend_from_slice(&0i64.to_le_bytes());
    // Entry 2: RELATIVE at offset 0x40 (should be ignored by abs64 processor)
    table.extend_from_slice(&0x40u64.to_le_bytes());
    table.extend_from_slice(&R_AARCH64_RELATIVE.to_le_bytes());
    table.extend_from_slice(&(va2 as i64).to_le_bytes());

    let slide: u64 = 0x200_0000;
    apply_abs64_table(&mut binary, &table, slide, KERNEL_VA_OFFSET);

    assert_eq!(
        u64::from_le_bytes(binary[0x00..0x08].try_into().unwrap()),
        va1 + slide,
        "kernel VA should be adjusted"
    );
    assert_eq!(
        u64::from_le_bytes(binary[0x20..0x28].try_into().unwrap()),
        pa1,
        "physical address should be unchanged"
    );
    assert_eq!(
        u64::from_le_bytes(binary[0x40..0x48].try_into().unwrap()),
        va2,
        "RELATIVE entry should be ignored by abs64 processor"
    );
}

#[test]
fn abs64_out_of_bounds_skipped() {
    let mut binary = vec![0u8; 0x10];
    let rela = RelaEntry {
        offset: 0x10, // exactly at end — r_offset + 8 > len
        info: R_AARCH64_ABS64,
        addend: 0,
    };

    // Should not panic.
    apply_abs64_relocation(&mut binary, &rela, 0x200_0000, KERNEL_VA_OFFSET);
}
