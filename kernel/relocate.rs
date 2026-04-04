//! ELF64 relocation processing for KASLR.
//!
//! Processes `R_AARCH64_RELATIVE` entries from the `.rela.dyn` section to
//! adjust absolute addresses in the kernel binary by the KASLR slide delta.
//!
//! Each R_AARCH64_RELATIVE entry says: "at offset `r_offset` in the binary,
//! write `r_addend + delta`." This is the simplest relocation type — the
//! linker computed the absolute address at link time, and we just add the
//! slide to it.
//!
//! # Safety
//!
//! The relocation processor runs once during early boot, before any kernel
//! subsystem is initialized. It operates on raw memory at physical addresses
//! and must not use any kernel services (heap, serial, etc.).
//!
//! # Architecture boundary
//!
//! This module is generic — the ELF Rela format and relocation logic are
//! architecture-independent. Only the relocation type constant
//! (`R_AARCH64_RELATIVE = 0x403`) is ARM64-specific. A RISC-V port would
//! define `R_RISCV_RELATIVE = 3`.

// Test-only: boot.S handles relocations at runtime in asm. This Rust
// implementation is used by kernel/test/tests/kernel_kaslr.rs.
#![allow(dead_code)]

/// Size of one ELF64 Rela entry (r_offset + r_info + r_addend).
const RELA_ENTRY_SIZE: usize = 24;

/// ELF64 relocation type: R_AARCH64_ABS64.
///
/// Absolute 64-bit address. With `--emit-relocs`, the linker resolves
/// the symbol value and writes it to the target; the relocation entry
/// is preserved for tools to find the fixup sites.
pub const R_AARCH64_ABS64: u64 = 0x101;
/// ELF64 relocation type: R_AARCH64_RELATIVE.
///
/// "The location is adjusted by the difference between the address at
/// which the segment is loaded and the address at which it was linked."
pub const R_AARCH64_RELATIVE: u64 = 0x403;

/// A parsed ELF64 Rela entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelaEntry {
    /// Offset in the binary where the relocated value lives.
    pub offset: u64,
    /// Relocation type and symbol index. Low 32 bits = type.
    pub info: u64,
    /// Addend — the value to add the delta to.
    pub addend: i64,
}

impl RelaEntry {
    /// Parse a Rela entry from 24 little-endian bytes.
    pub fn from_le_bytes(bytes: &[u8]) -> Self {
        Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            info: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            addend: i64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        }
    }
    /// Check if this is an R_AARCH64_ABS64 relocation.
    pub fn is_abs64(&self) -> bool {
        (self.info & 0xFFFF_FFFF) == R_AARCH64_ABS64
    }
    /// Check if this is an R_AARCH64_RELATIVE relocation.
    pub fn is_relative(&self) -> bool {
        (self.info & 0xFFFF_FFFF) == R_AARCH64_RELATIVE
    }
}

/// Apply a single R_AARCH64_ABS64 relocation (from `--emit-relocs`) to a
/// binary region.
///
/// With `--emit-relocs`, the linker already resolved the absolute address.
/// The value at `r_offset` is `symbol + addend`. To apply KASLR, we add
/// the slide — but only if the current value is a kernel VA (>= `va_threshold`).
/// Physical addresses (e.g., `secondary_entry` for PSCI CPU_ON) must not
/// be adjusted.
///
/// The `va_threshold` parameter is `KERNEL_VA_OFFSET` — any value at or
/// above it is considered a kernel virtual address.
pub fn apply_abs64_relocation(binary: &mut [u8], entry: &RelaEntry, slide: u64, va_threshold: u64) {
    if !entry.is_abs64() {
        return;
    }

    let offset = entry.offset as usize;

    if offset + 8 > binary.len() {
        return;
    }

    let current = u64::from_le_bytes(binary[offset..offset + 8].try_into().unwrap());

    if current < va_threshold {
        return; // Physical address — leave unchanged.
    }

    let new_value = current.wrapping_add(slide);

    binary[offset..offset + 8].copy_from_slice(&new_value.to_le_bytes());
}
/// Apply all ABS64 relocations from a `.rela.dyn` section (from `--emit-relocs`).
///
/// Iterates the relocation table and applies each ABS64 entry to the binary.
/// Only values >= `va_threshold` are adjusted; physical addresses are skipped.
pub fn apply_abs64_table(binary: &mut [u8], rela_table: &[u8], slide: u64, va_threshold: u64) {
    let count = rela_entry_count(rela_table);

    for i in 0..count {
        let entry = rela_entry_at(rela_table, i);

        apply_abs64_relocation(binary, &entry, slide, va_threshold);
    }
}
/// Apply all R_AARCH64_RELATIVE relocations from a `.rela.dyn` section.
///
/// Iterates the relocation table and applies each relative entry to the
/// binary region. Non-relative entries are skipped.
pub fn apply_rela_table(binary: &mut [u8], rela_table: &[u8], slide: u64) {
    let count = rela_entry_count(rela_table);

    for i in 0..count {
        let entry = rela_entry_at(rela_table, i);

        apply_relocation(binary, &entry, slide);
    }
}
/// Apply a single R_AARCH64_RELATIVE relocation to a binary region.
///
/// For relative relocations: writes `addend + slide` to the u64 at
/// `binary[offset..offset+8]`. Non-relative entries are skipped.
pub fn apply_relocation(binary: &mut [u8], entry: &RelaEntry, slide: u64) {
    if !entry.is_relative() {
        return;
    }

    let offset = entry.offset as usize;

    if offset + 8 > binary.len() {
        return; // Out of bounds — skip silently.
    }

    let new_value = (entry.addend as u64).wrapping_add(slide);

    binary[offset..offset + 8].copy_from_slice(&new_value.to_le_bytes());
}
/// Parse a `.rela.dyn` section into a Vec of Rela entries (test helper).
#[cfg(test)]
pub fn parse_rela_entries(table: &[u8]) -> Vec<RelaEntry> {
    (0..rela_entry_count(table))
        .map(|i| rela_entry_at(table, i))
        .collect()
}
/// Parse the Nth Rela entry from a `.rela.dyn` section.
///
/// # Panics
///
/// Panics if `index * 24 + 24 > table.len()`.
pub fn rela_entry_at(table: &[u8], index: usize) -> RelaEntry {
    let start = index * RELA_ENTRY_SIZE;

    RelaEntry::from_le_bytes(&table[start..start + RELA_ENTRY_SIZE])
}
/// Count the number of complete Rela entries in a `.rela.dyn` section.
///
/// Truncated entries (< 24 bytes) at the end are ignored.
pub fn rela_entry_count(table: &[u8]) -> usize {
    table.len() / RELA_ENTRY_SIZE
}
