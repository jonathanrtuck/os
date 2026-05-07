//! Minimal Flattened Device Tree (FDT) scanner for early boot.
//!
//! Extracts only what the kernel needs to initialize: RAM region and core
//! count. No allocator required — all state is on the stack.
//!
//! Assumes `#address-cells = 2, #size-cells = 2` at the root level
//! (QEMU virt / Apple Hypervisor standard), meaning each `reg` entry in
//! top-level nodes is 16 bytes (base: u64, size: u64).

// FDT structure tokens.
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_END: u32 = 9;
const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_NOP: u32 = 4;
const FDT_PROP: u32 = 3;
const HEADER_SIZE: usize = 40;

// FDT node and property names used during scanning.
const NODE_CPUS: &[u8] = b"cpus";
const NODE_CPU_PREFIX: &[u8] = b"cpu@";
const NODE_HVF_TIMING_PREFIX: &[u8] = b"hvf-timing@";
const PROP_DEVICE_TYPE: &str = "device_type";
const PROP_COMPATIBLE: &str = "compatible";
const PROP_REG: &str = "reg";
const DEVICE_TYPE_MEMORY: &str = "memory";
const HVF_TIMING_COMPATIBLE_V1: &str = "arts,hvf-timing-v1";

/// Hardware information discovered from the device tree.
pub struct BootInfo {
    /// Physical RAM base address.
    pub ram_base: usize,
    /// Physical RAM size in bytes.
    pub ram_size: usize,
    /// Number of CPU cores.
    pub core_count: usize,
    /// HVF timing counter page (paravirtual, optional). Present when running
    /// under our macOS hypervisor; absent otherwise. The reader trusts the
    /// magic stamped at the page so a stale or incorrect address is harmless.
    pub hvf_timing_pa: usize,
    /// Size of the HVF timing region in bytes. 0 when no node is present.
    pub hvf_timing_size: usize,
}

/// Scan the FDT blob at `dtb_ptr` and extract boot-critical hardware info.
///
/// Returns `None` if the pointer is null or the DTB is invalid.
/// Only available on bare metal — the raw pointer comes from firmware.
#[cfg(target_os = "none")]
pub fn scan(dtb_ptr: usize) -> Option<BootInfo> {
    if dtb_ptr == 0 {
        return None;
    }

    // SAFETY: The DTB is placed at a known address by the hypervisor/firmware.
    // We're in single-threaded physical-mode boot. Read just the header first
    // to discover totalsize, then create the full slice.
    let header = unsafe { core::slice::from_raw_parts(dtb_ptr as *const u8, HEADER_SIZE) };
    let magic = read_be_u32(header, 0);

    if magic != FDT_MAGIC {
        return None;
    }

    let totalsize = read_be_u32(header, 4) as usize;

    if totalsize < HEADER_SIZE {
        return None;
    }

    // SAFETY: totalsize comes from the validated FDT header. The entire blob
    // is within the RAM region placed by the hypervisor.
    let blob = unsafe { core::slice::from_raw_parts(dtb_ptr as *const u8, totalsize) };

    scan_blob(blob)
}

/// Parse an FDT blob from a byte slice.
///
/// This is the pure-computation core, separated from the raw pointer
/// access in [`scan`] so it can be tested on the host.
pub fn scan_blob(blob: &[u8]) -> Option<BootInfo> {
    if blob.len() < HEADER_SIZE {
        return None;
    }

    let magic = read_be_u32(blob, 0);

    if magic != FDT_MAGIC {
        return None;
    }

    let totalsize = read_be_u32(blob, 4) as usize;

    if totalsize < HEADER_SIZE || totalsize > blob.len() {
        return None;
    }

    let off_struct = read_be_u32(blob, 8) as usize;
    let off_strings = read_be_u32(blob, 12) as usize;
    let size_struct = read_be_u32(blob, 36) as usize; // v17+ header field

    if off_struct >= totalsize || off_strings >= totalsize {
        return None;
    }

    let struct_end = off_struct.checked_add(size_struct)?;

    if struct_end > totalsize {
        return None;
    }

    let structs = blob.get(off_struct..struct_end)?;
    let strings = blob.get(off_strings..totalsize)?;
    let mut info = BootInfo {
        ram_base: 0,
        ram_size: 0,
        core_count: 0,
        hvf_timing_pa: 0,
        hvf_timing_size: 0,
    };
    // Per-node state (reset at each depth-2 BEGIN_NODE, committed at END_NODE).
    let mut is_memory = false;
    let mut is_hvf_timing = false;
    let mut hvf_timing_compatible_ok = false;
    let mut reg_base: u64 = 0;
    let mut reg_size: u64 = 0;
    let mut has_reg = false;
    // /cpus tracking.
    let mut in_cpus = false;
    let mut cpus_depth: usize = 0;
    let mut depth: usize = 0;
    let mut offset: usize = 0;
    let mut seen_end = false;

    loop {
        if offset + 4 > structs.len() {
            break;
        }

        let token = read_be_u32(structs, offset);

        offset += 4;

        match token {
            FDT_BEGIN_NODE => {
                // Read the node name (null-terminated, padded to 4 bytes).
                let name_start = offset;

                while offset < structs.len() && structs[offset] != 0 {
                    offset += 1;
                }

                let name = &structs[name_start..offset];

                if offset < structs.len() {
                    offset += 1;
                }

                offset = align4(offset);
                depth += 1;

                if depth == 2 {
                    // Top-level node — reset accumulator.
                    is_memory = false;
                    is_hvf_timing = starts_with(name, NODE_HVF_TIMING_PREFIX);
                    hvf_timing_compatible_ok = false;
                    has_reg = false;
                    in_cpus = name == NODE_CPUS;

                    if in_cpus {
                        cpus_depth = depth;
                    }
                } else if in_cpus && depth == cpus_depth + 1 && starts_with(name, NODE_CPU_PREFIX) {
                    info.core_count += 1;
                }
            }
            FDT_END_NODE => {
                if depth == 2 {
                    if is_memory && has_reg {
                        info.ram_base = reg_base as usize;
                        info.ram_size = reg_size as usize;
                    }

                    if is_hvf_timing && hvf_timing_compatible_ok && has_reg {
                        info.hvf_timing_pa = reg_base as usize;
                        info.hvf_timing_size = reg_size as usize;
                    }

                    if depth == cpus_depth {
                        in_cpus = false;
                    }
                }
                depth = depth.saturating_sub(1);
            }
            FDT_PROP => {
                if offset + 8 > structs.len() {
                    return None;
                }

                let len = read_be_u32(structs, offset) as usize;

                offset += 4;

                let nameoff = read_be_u32(structs, offset) as usize;

                offset += 4;

                if offset + len > structs.len() {
                    return None;
                }

                // Only interpret properties of top-level (depth-2) nodes.
                if depth == 2 {
                    let data = &structs[offset..offset + len];
                    let name = read_cstr(strings, nameoff);

                    if name == PROP_DEVICE_TYPE && data_eq_str(data, DEVICE_TYPE_MEMORY) {
                        is_memory = true;
                    } else if name == PROP_COMPATIBLE
                        && is_hvf_timing
                        && data_eq_str(data, HVF_TIMING_COMPATIBLE_V1)
                    {
                        hvf_timing_compatible_ok = true;
                    } else if name == PROP_REG && data.len() >= 16 {
                        // #address-cells=2, #size-cells=2: first entry is 16 bytes.
                        reg_base = read_be_u64(data, 0);
                        reg_size = read_be_u64(data, 8);
                        has_reg = true;
                    }
                }

                offset = align4(offset + len);
            }
            FDT_NOP => {}
            FDT_END => {
                seen_end = true;

                break;
            }
            _ => return None, // Unknown token — corrupted struct block.
        }
    }

    // Reject if the struct block was truncated (no FDT_END seen) or if
    // no memory node was found.
    if !seen_end || info.ram_size == 0 {
        return None;
    }

    Some(info)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn align4(v: usize) -> usize {
    (v + 3) & !3
}

fn read_be_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_be_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Read a null-terminated string from the strings block.
fn read_cstr(data: &[u8], offset: usize) -> &str {
    if offset >= data.len() {
        return "";
    }

    let mut end = offset;

    while end < data.len() && data[end] != 0 {
        end += 1;
    }

    core::str::from_utf8(&data[offset..end]).unwrap_or("")
}

/// Check if a property value is a null-terminated string equal to `expected`.
///
/// FDT string properties are stored as the string bytes followed by a null
/// terminator. This checks for an exact match — not a prefix match.
fn data_eq_str(data: &[u8], expected: &str) -> bool {
    let bytes = expected.as_bytes();

    // Property data must contain the string + null terminator.
    data.len() > bytes.len() && &data[..bytes.len()] == bytes && data[bytes.len()] == 0
}

fn starts_with(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && &haystack[..prefix.len()] == prefix
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate alloc;
    extern crate std;

    use alloc::{format, vec, vec::Vec};
    use std::collections::HashMap;

    use super::*;

    // -----------------------------------------------------------------------
    // FDT builder (test helper)
    // -----------------------------------------------------------------------

    struct FdtBuilder {
        structs: Vec<u8>,
        strings: Vec<u8>,
        string_offsets: HashMap<&'static str, u32>,
    }

    impl FdtBuilder {
        fn new() -> Self {
            Self {
                structs: Vec::new(),
                strings: Vec::new(),
                string_offsets: HashMap::new(),
            }
        }

        fn begin_node(&mut self, name: &str) {
            self.push_token(FDT_BEGIN_NODE);
            self.structs.extend_from_slice(name.as_bytes());
            self.structs.push(0);
            self.pad4();
        }

        fn end_node(&mut self) {
            self.push_token(FDT_END_NODE);
        }

        fn nop(&mut self) {
            self.push_token(FDT_NOP);
        }

        fn prop_u32(&mut self, name: &'static str, val: u32) {
            let nameoff = self.intern(name);

            self.push_token(FDT_PROP);
            self.push_be_u32(4);
            self.push_be_u32(nameoff);
            self.push_be_u32(val);
        }

        fn prop_string(&mut self, name: &'static str, val: &str) {
            let nameoff = self.intern(name);
            let bytes: Vec<u8> = val.bytes().chain(core::iter::once(0)).collect();

            self.push_token(FDT_PROP);
            self.push_be_u32(bytes.len() as u32);
            self.push_be_u32(nameoff);
            self.structs.extend_from_slice(&bytes);
            self.pad4();
        }

        fn prop_reg(&mut self, addr: u64, size: u64) {
            let nameoff = self.intern("reg");

            self.push_token(FDT_PROP);
            self.push_be_u32(16);
            self.push_be_u32(nameoff);
            self.push_be_u64(addr);
            self.push_be_u64(size);
        }

        fn finish(mut self) -> Vec<u8> {
            self.push_token(FDT_END);

            let header_size: u32 = 40;
            let rsvmap_size: u32 = 16;
            let struct_off = header_size + rsvmap_size;
            let strings_off = struct_off + self.structs.len() as u32;
            let totalsize = strings_off + self.strings.len() as u32;
            let mut blob = Vec::new();

            be_u32(&mut blob, FDT_MAGIC);
            be_u32(&mut blob, totalsize);
            be_u32(&mut blob, struct_off);
            be_u32(&mut blob, strings_off);
            be_u32(&mut blob, header_size);
            be_u32(&mut blob, 17); // version
            be_u32(&mut blob, 16); // last_comp_version
            be_u32(&mut blob, 0); // boot_cpuid_phys
            be_u32(&mut blob, self.strings.len() as u32);
            be_u32(&mut blob, self.structs.len() as u32);

            blob.extend_from_slice(&[0u8; 16]); // empty rsvmap
            blob.extend_from_slice(&self.structs);
            blob.extend_from_slice(&self.strings);

            blob
        }

        fn push_token(&mut self, val: u32) {
            self.structs.extend_from_slice(&val.to_be_bytes());
        }

        fn push_be_u32(&mut self, val: u32) {
            self.structs.extend_from_slice(&val.to_be_bytes());
        }

        fn push_be_u64(&mut self, val: u64) {
            self.structs.extend_from_slice(&val.to_be_bytes());
        }

        fn pad4(&mut self) {
            while self.structs.len() % 4 != 0 {
                self.structs.push(0);
            }
        }

        fn intern(&mut self, name: &'static str) -> u32 {
            if let Some(&off) = self.string_offsets.get(name) {
                return off;
            }

            let off = self.strings.len() as u32;

            self.string_offsets.insert(name, off);
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);

            off
        }
    }

    fn be_u32(v: &mut Vec<u8>, val: u32) {
        v.extend_from_slice(&val.to_be_bytes());
    }

    fn build_test_dtb(ram_base: u64, ram_size: u64, cpu_count: usize) -> Vec<u8> {
        let mut b = FdtBuilder::new();

        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node(&format!("memory@{ram_base:x}"));
        b.prop_string("device_type", "memory");
        b.prop_reg(ram_base, ram_size);
        b.end_node();
        b.begin_node("cpus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);

        for i in 0..cpu_count {
            b.begin_node(&format!("cpu@{i}"));
            b.prop_string("device_type", "cpu");
            b.prop_u32("reg", i as u32);
            b.end_node();
        }

        b.end_node();

        b.end_node();

        b.finish()
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_qemu_virt_layout() {
        let blob = build_test_dtb(0x4000_0000, 256 * 1024 * 1024, 4);
        let info = scan_blob(&blob).expect("scan should succeed");

        assert_eq!(info.ram_base, 0x4000_0000);
        assert_eq!(info.ram_size, 256 * 1024 * 1024);
        assert_eq!(info.core_count, 4);
    }

    #[test]
    fn parse_different_ram_size() {
        let blob = build_test_dtb(0x4000_0000, 512 * 1024 * 1024, 8);
        let info = scan_blob(&blob).expect("scan should succeed");

        assert_eq!(info.ram_size, 512 * 1024 * 1024);
        assert_eq!(info.core_count, 8);
    }

    #[test]
    fn parse_single_core() {
        let blob = build_test_dtb(0x4000_0000, 128 * 1024 * 1024, 1);
        let info = scan_blob(&blob).expect("scan should succeed");

        assert_eq!(info.core_count, 1);
    }

    #[test]
    fn reject_bad_magic() {
        let mut blob = build_test_dtb(0x4000_0000, 256 * 1024 * 1024, 4);

        blob[0] = 0;

        assert!(scan_blob(&blob).is_none());
    }

    #[test]
    fn reject_truncated_header() {
        let blob = vec![0xD0, 0x0D, 0xFE, 0xED];

        assert!(scan_blob(&blob).is_none());
    }

    #[test]
    fn reject_empty_blob() {
        assert!(scan_blob(&[]).is_none());
    }

    #[test]
    fn reject_no_memory_node() {
        let mut b = FdtBuilder::new();

        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("cpus");
        b.begin_node("cpu@0");
        b.end_node();
        b.end_node();
        b.end_node();

        let blob = b.finish();

        assert!(scan_blob(&blob).is_none());
    }

    #[test]
    fn nop_tokens_are_skipped() {
        let mut b = FdtBuilder::new();

        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.nop();
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory@40000000");
        b.prop_string("device_type", "memory");
        b.prop_reg(0x4000_0000, 0x1000_0000);
        b.end_node();
        b.end_node();

        let blob = b.finish();
        let info = scan_blob(&blob).expect("scan should succeed despite NOPs");

        assert_eq!(info.ram_base, 0x4000_0000);
    }

    #[test]
    fn memory_controller_not_classified_as_ram() {
        let mut b = FdtBuilder::new();

        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory-controller@50000000");
        b.prop_string("device_type", "memory-controller");
        b.prop_reg(0x5000_0000, 0x1000);
        b.end_node();
        b.end_node();

        let blob = b.finish();

        assert!(
            scan_blob(&blob).is_none(),
            "memory-controller must not be classified as RAM"
        );
    }

    #[test]
    fn real_memory_with_memory_controller_present() {
        let mut b = FdtBuilder::new();

        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory-controller@50000000");
        b.prop_string("device_type", "memory-controller");
        b.prop_reg(0x5000_0000, 0x1000);
        b.end_node();
        b.begin_node("memory@40000000");
        b.prop_string("device_type", "memory");
        b.prop_reg(0x4000_0000, 0x1000_0000);
        b.end_node();
        b.end_node();

        let blob = b.finish();
        let info = scan_blob(&blob).expect("should find real memory node");

        assert_eq!(info.ram_base, 0x4000_0000);
        assert_eq!(info.ram_size, 0x1000_0000);
    }

    #[test]
    fn reject_truncated_struct_block() {
        let mut blob = build_test_dtb(0x4000_0000, 256 * 1024 * 1024, 4);
        // Shrink size_dt_struct (header offset 36) so FDT_END falls outside
        // the declared struct block. The parser must reject this even though
        // the memory node was parsed successfully before truncation.
        let current_size = u32::from_be_bytes([blob[36], blob[37], blob[38], blob[39]]);
        let truncated = (current_size / 2).to_be_bytes();

        blob[36..40].copy_from_slice(&truncated);

        assert!(
            scan_blob(&blob).is_none(),
            "truncated struct block must be rejected"
        );
    }

    #[test]
    fn reject_unknown_token() {
        let mut blob = build_test_dtb(0x4000_0000, 256 * 1024 * 1024, 4);
        // Overwrite the first struct token (FDT_BEGIN_NODE for root) with an
        // invalid token value. Header offset 16 = off_mem_rsvmap, the rsvmap
        // is 16 bytes, so the struct block starts at header + 16 + 16 = 56 + rsvmap.
        // Actually, use the header's off_dt_struct to find it precisely.
        let off_struct = u32::from_be_bytes([blob[8], blob[9], blob[10], blob[11]]) as usize;

        // Write an invalid token (0xFF) at the start of the struct block.
        blob[off_struct..off_struct + 4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());

        assert!(scan_blob(&blob).is_none(), "unknown token must be rejected");
    }

    #[test]
    fn different_ram_base_is_reported_correctly() {
        let blob = build_test_dtb(0x8000_0000, 1024 * 1024 * 1024, 2);
        let info = scan_blob(&blob).expect("scan should succeed");

        assert_eq!(info.ram_base, 0x8000_0000);
        assert_eq!(info.ram_size, 1024 * 1024 * 1024);
    }
}
