//! Host-side tests for the device tree (FDT) parser.
//!
//! Constructs minimal FDT blobs in memory and verifies parsing.

extern crate alloc;

#[path = "../../device_tree.rs"]
mod device_tree;

// --- FDT blob construction helpers ---

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_END: u32 = 9;

/// Builder for constructing minimal FDT blobs in tests.
struct FdtBuilder {
    structs: Vec<u8>,
    strings: Vec<u8>,
}

impl FdtBuilder {
    fn new() -> Self {
        Self {
            structs: Vec::new(),
            strings: Vec::new(),
        }
    }

    fn add_string(&mut self, name: &str) -> u32 {
        let offset = self.strings.len() as u32;

        for b in name.bytes() {
            self.strings.push(b);
        }

        self.strings.push(0);

        offset
    }
    fn align4(&mut self) {
        while self.structs.len() % 4 != 0 {
            self.structs.push(0);
        }
    }
    fn begin_node(&mut self, name: &str) {
        self.push_u32(FDT_BEGIN_NODE);

        for b in name.bytes() {
            self.structs.push(b);
        }

        self.structs.push(0); // null terminator
        self.align4();
    }
    fn end_node(&mut self) {
        self.push_u32(FDT_END_NODE);
    }
    fn finish(mut self) -> Vec<u8> {
        self.push_u32(FDT_END);

        let header_size = 40usize;
        let off_dt_struct = header_size;
        let off_dt_strings = header_size + self.structs.len();
        let totalsize = off_dt_strings + self.strings.len();
        let mut blob = Vec::with_capacity(totalsize);

        // Header (10 x u32 = 40 bytes).
        blob.extend_from_slice(&FDT_MAGIC.to_be_bytes());
        blob.extend_from_slice(&(totalsize as u32).to_be_bytes());
        blob.extend_from_slice(&(off_dt_struct as u32).to_be_bytes());
        blob.extend_from_slice(&(off_dt_strings as u32).to_be_bytes());
        // mem_rsvmap_off, version, last_comp_version, boot_cpuid, strings_size, struct_size
        blob.extend_from_slice(&0u32.to_be_bytes());
        blob.extend_from_slice(&17u32.to_be_bytes()); // version
        blob.extend_from_slice(&16u32.to_be_bytes()); // last compatible version
        blob.extend_from_slice(&0u32.to_be_bytes());
        blob.extend_from_slice(&(self.strings.len() as u32).to_be_bytes());
        blob.extend_from_slice(&(self.structs.len() as u32).to_be_bytes());

        assert_eq!(blob.len(), header_size);

        blob.extend_from_slice(&self.structs);
        blob.extend_from_slice(&self.strings);

        assert_eq!(blob.len(), totalsize);

        blob
    }
    fn prop_interrupts(&mut self, name: &str, irq_type: u32, irq_num: u32, flags: u32) {
        let nameoff = self.add_string(name);
        let mut data = Vec::new();

        data.extend_from_slice(&irq_type.to_be_bytes());
        data.extend_from_slice(&irq_num.to_be_bytes());
        data.extend_from_slice(&flags.to_be_bytes());
        self.push_prop(nameoff, &data);
    }
    fn prop_reg(&mut self, name: &str, entries: &[(u64, u64)]) {
        let nameoff = self.add_string(name);
        let mut data = Vec::new();

        for &(addr, size) in entries {
            data.extend_from_slice(&addr.to_be_bytes());
            data.extend_from_slice(&size.to_be_bytes());
        }

        self.push_prop(nameoff, &data);
    }
    fn prop_str(&mut self, name: &str, value: &str) {
        let nameoff = self.add_string(name);
        let mut data = Vec::new();

        for b in value.bytes() {
            data.push(b);
        }

        data.push(0); // null terminator
        self.push_prop(nameoff, &data);
    }
    fn push_prop(&mut self, nameoff: u32, data: &[u8]) {
        self.push_u32(FDT_PROP);
        self.push_u32(data.len() as u32);
        self.push_u32(nameoff);
        self.structs.extend_from_slice(data);
        self.align4();
    }
    fn push_u32(&mut self, val: u32) {
        self.structs.extend_from_slice(&val.to_be_bytes());
    }
}

#[test]
fn find_all_returns_multiple_matches() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");

    for i in 0..3u64 {
        let name = format!("virtio_mmio@{:x}", 0x0A00_0000 + i * 0x200);

        builder.begin_node(&name);
        builder.prop_str("compatible", "virtio,mmio");
        builder.prop_reg("reg", &[(0x0A00_0000 + i * 0x200, 0x200)]);
        builder.end_node();
    }

    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");
    let virtio_devices: Vec<_> = dt.find_all("virtio,mmio").collect();

    assert_eq!(virtio_devices.len(), 3);
}
#[test]
fn find_first_nonexistent_returns_none() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert!(dt.find_first("nonexistent").is_none());
}
#[test]
fn node_without_compatible_is_skipped() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("memory@40000000");
    // Has reg but no compatible string.
    builder.prop_reg("reg", &[(0x4000_0000, 0x1000_0000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert_eq!(dt.device_count(), 0);
}
#[test]
fn node_without_reg_is_skipped() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("chosen");
    builder.prop_str("compatible", "chosen");
    // No reg property — should not appear as a device.
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert_eq!(dt.device_count(), 0);
}
#[test]
fn parse_bad_magic_returns_none() {
    let mut blob = vec![0u8; 40];

    // Write wrong magic.
    blob[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());

    assert!(device_tree::parse(&blob).is_none());
}
#[test]
fn parse_device_with_interrupt() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    // SPI interrupt type=0, number=1 → hardware IRQ = 1 + 32 = 33.
    builder.prop_interrupts("interrupts", 0, 1, 4);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");
    let dev = dt.find_first("arm,pl011").unwrap();

    assert_eq!(dev.irq, Some(33));
}
#[test]
fn parse_empty_blob_returns_none() {
    assert!(device_tree::parse(&[]).is_none());
}
#[test]
fn parse_empty_tree() {
    let mut builder = FdtBuilder::new();

    builder.begin_node(""); // root node
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse empty tree");

    assert_eq!(dt.device_count(), 0);
}
#[test]
fn parse_multiple_devices() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    builder.end_node();
    builder.begin_node("virtio_mmio@a000000");
    builder.prop_str("compatible", "virtio,mmio");
    builder.prop_reg("reg", &[(0x0A00_0000, 0x200)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert_eq!(dt.device_count(), 2);
    assert!(dt.find_first("arm,pl011").is_some());
    assert!(dt.find_first("virtio,mmio").is_some());
}
#[test]
fn parse_multiple_reg_entries() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("intc@8000000");
    builder.prop_str("compatible", "arm,cortex-a15-gic");
    // GIC has two regions: distributor and CPU interface.
    builder.prop_reg("reg", &[(0x0800_0000, 0x10000), (0x0801_0000, 0x10000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");
    let dev = dt.find_first("arm,cortex-a15-gic").unwrap();

    assert_eq!(dev.regs.len(), 2);
    assert_eq!(dev.regs[0], (0x0800_0000, 0x10000));
    assert_eq!(dev.regs[1], (0x0801_0000, 0x10000));
}
#[test]
fn parse_nested_node_preserves_parent() {
    // GIC node with a v2m child — mirrors real QEMU virt DTB structure.
    // The parent must be emitted even though a child node intervenes.
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("intc@8000000");
    builder.prop_str("compatible", "arm,cortex-a15-gic");
    builder.prop_reg("reg", &[(0x0800_0000, 0x10000), (0x0801_0000, 0x10000)]);
    // Child node.
    builder.begin_node("v2m@8020000");
    builder.prop_str("compatible", "arm,gic-v2m-frame");
    builder.prop_reg("reg", &[(0x0802_0000, 0x1000)]);
    builder.end_node(); // v2m
    builder.end_node(); // intc
    builder.end_node(); // root

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    // Both parent and child should be discovered.
    assert_eq!(dt.device_count(), 2);

    let gic = dt
        .find_first("arm,cortex-a15-gic")
        .expect("should find GIC");

    assert_eq!(gic.regs.len(), 2);
    assert_eq!(gic.regs[0], (0x0800_0000, 0x10000));
    assert_eq!(gic.regs[1], (0x0801_0000, 0x10000));

    let v2m = dt.find_first("arm,gic-v2m-frame").expect("should find v2m");

    assert_eq!(v2m.regs.len(), 1);
    assert_eq!(v2m.regs[0], (0x0802_0000, 0x1000));
}
#[test]
fn parse_ppi_interrupt() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("timer");
    builder.prop_str("compatible", "arm,armv8-timer");
    builder.prop_reg("reg", &[(0x1000, 0x100)]);
    // PPI interrupt type=1, number=13 → hardware IRQ = 13 + 16 = 29.
    builder.prop_interrupts("interrupts", 1, 13, 4);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");
    let dev = dt.find_first("arm,armv8-timer").unwrap();

    assert_eq!(dev.irq, Some(29));
}
#[test]
fn parse_single_device() {
    let mut builder = FdtBuilder::new();

    builder.begin_node(""); // root
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert_eq!(dt.device_count(), 1);

    let dev = dt.find_first("arm,pl011").expect("should find uart");

    assert_eq!(dev.compatible, "arm,pl011");
    assert_eq!(dev.base_address(), 0x0900_0000);
    assert_eq!(dev.size(), 0x1000);
    assert!(dev.irq.is_none());
}
#[test]
fn parse_too_small_blob_returns_none() {
    assert!(device_tree::parse(&[0u8; 10]).is_none());
}
#[test]
fn truncated_blob_returns_none() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    // Claim totalsize is larger than actual blob.
    let mut bad_blob = blob.clone();
    let big_size = (blob.len() as u32 + 1000).to_be_bytes();

    bad_blob[4..8].copy_from_slice(&big_size);

    assert!(device_tree::parse(&bad_blob).is_none());
}

// --- Audit tests: edge cases for malformed / adversarial FDT blobs ---

/// A property with length close to usize::MAX could cause `offset + len`
/// to overflow, bypassing the bounds check and leading to a panic or
/// out-of-bounds read. The parser must reject this gracefully.
#[test]
fn parse_prop_length_overflow_returns_none() {
    // Build a minimal FDT with a single property whose `len` field is
    // 0xFFFF_FF00 — large enough that `offset + len` wraps on 64-bit.
    let header_size = 40usize;
    let mut structs = Vec::new();

    // FDT_BEGIN_NODE with empty name.
    structs.extend_from_slice(&1u32.to_be_bytes()); // FDT_BEGIN_NODE
    structs.push(0); // empty name null terminator
                     // Pad to 4-byte alignment.
    while structs.len() % 4 != 0 {
        structs.push(0);
    }

    // FDT_PROP with an enormous length.
    structs.extend_from_slice(&3u32.to_be_bytes()); // FDT_PROP
    structs.extend_from_slice(&0xFFFF_FF00u32.to_be_bytes()); // len (nearly u32::MAX)
    structs.extend_from_slice(&0u32.to_be_bytes()); // nameoff = 0

    // FDT_END (won't be reached if parser handles overflow).
    structs.extend_from_slice(&9u32.to_be_bytes()); // FDT_END

    let strings = b"x\0";
    let off_dt_struct = header_size;
    let off_dt_strings = header_size + structs.len();
    let totalsize = off_dt_strings + strings.len();
    let mut blob = Vec::with_capacity(totalsize);

    blob.extend_from_slice(&0xD00D_FEEDu32.to_be_bytes()); // magic
    blob.extend_from_slice(&(totalsize as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_struct as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_strings as u32).to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes()); // mem_rsvmap_off
    blob.extend_from_slice(&17u32.to_be_bytes()); // version
    blob.extend_from_slice(&16u32.to_be_bytes()); // last_comp_version
    blob.extend_from_slice(&0u32.to_be_bytes()); // boot_cpuid
    blob.extend_from_slice(&(strings.len() as u32).to_be_bytes());
    blob.extend_from_slice(&(structs.len() as u32).to_be_bytes());
    blob.extend_from_slice(&structs);
    blob.extend_from_slice(strings);

    // Must return None, not panic.
    assert!(device_tree::parse(&blob).is_none());
}

/// `align4` on a value close to usize::MAX wraps around. A crafted
/// property whose offset + len lands near usize::MAX - 2 triggers
/// this. The parser must not enter an infinite loop or panic.
#[test]
fn parse_prop_offset_plus_len_near_max_returns_none() {
    // Similar to above but with a length that, when added to a small
    // offset, produces a value in the [usize::MAX-2, usize::MAX] range
    // where align4 would wrap.
    let header_size = 40usize;
    let mut structs = Vec::new();

    // FDT_BEGIN_NODE with empty name.
    structs.extend_from_slice(&1u32.to_be_bytes());
    structs.push(0);
    while structs.len() % 4 != 0 {
        structs.push(0);
    }

    // FDT_PROP with length that's large but won't overflow with offset.
    // Use a length that's bigger than the structs slice but within u32.
    structs.extend_from_slice(&3u32.to_be_bytes()); // FDT_PROP
    structs.extend_from_slice(&0x7FFF_FFFFu32.to_be_bytes()); // len (~2GB)
    structs.extend_from_slice(&0u32.to_be_bytes()); // nameoff

    structs.extend_from_slice(&9u32.to_be_bytes()); // FDT_END

    let strings = b"x\0";
    let off_dt_struct = header_size;
    let off_dt_strings = header_size + structs.len();
    let totalsize = off_dt_strings + strings.len();
    let mut blob = Vec::with_capacity(totalsize);

    blob.extend_from_slice(&0xD00D_FEEDu32.to_be_bytes());
    blob.extend_from_slice(&(totalsize as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_struct as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_strings as u32).to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&17u32.to_be_bytes());
    blob.extend_from_slice(&16u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&(strings.len() as u32).to_be_bytes());
    blob.extend_from_slice(&(structs.len() as u32).to_be_bytes());
    blob.extend_from_slice(&structs);
    blob.extend_from_slice(strings);

    // Must reject the blob, not crash.
    assert!(device_tree::parse(&blob).is_none());
}

/// A property with zero-length data should be handled gracefully
/// (the reg/compatible/interrupts handlers just see an empty slice).
#[test]
fn parse_zero_length_property_is_harmless() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("test");
    // Manually push a zero-length "compatible" property.
    let nameoff = builder.add_string("compatible");
    builder.push_prop(nameoff, &[]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("zero-length prop should parse");

    // Node has compatible="" but no reg, so it shouldn't appear as a device.
    assert_eq!(dt.device_count(), 0);
}

/// Interrupts property with fewer than 12 bytes should be ignored
/// (parser requires >= 12 for the 3-cell GIC format).
#[test]
fn parse_short_interrupt_property_ignored() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("dev@1000");
    builder.prop_str("compatible", "test,device");
    builder.prop_reg("reg", &[(0x1000, 0x100)]);
    // Push an 8-byte interrupts property (too short for 3-cell format).
    let nameoff = builder.add_string("interrupts");
    let mut data = Vec::new();
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(&42u32.to_be_bytes());
    builder.push_prop(nameoff, &data);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("short interrupt should parse");
    let dev = dt.find_first("test,device").expect("device found");

    assert!(
        dev.irq.is_none(),
        "short interrupt data should not yield an IRQ"
    );
}

/// Reg property with partial entry (< 16 bytes) should yield no
/// entries — the while loop condition `pos + 16 <= prop_data.len()`
/// skips partial data.
#[test]
fn parse_partial_reg_entry_yields_no_device() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("mem@1000");
    builder.prop_str("compatible", "test,mem");
    // Push a reg property with only 8 bytes (half an entry).
    let nameoff = builder.add_string("reg");
    let mut data = Vec::new();
    data.extend_from_slice(&0x1000u64.to_be_bytes());
    builder.push_prop(nameoff, &data);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("partial reg should parse");

    // Node has compatible + reg, but reg is empty (no complete entries),
    // so it should be skipped (regs.is_empty() check).
    assert_eq!(dt.device_count(), 0);
}

/// Deeply nested nodes should not corrupt parent state.
#[test]
fn parse_deeply_nested_nodes() {
    let mut builder = FdtBuilder::new();

    builder.begin_node(""); // root
    builder.begin_node("level1");
    builder.prop_str("compatible", "test,l1");
    builder.prop_reg("reg", &[(0x1000, 0x100)]);

    builder.begin_node("level2");
    builder.prop_str("compatible", "test,l2");
    builder.prop_reg("reg", &[(0x2000, 0x200)]);

    builder.begin_node("level3");
    builder.prop_str("compatible", "test,l3");
    builder.prop_reg("reg", &[(0x3000, 0x300)]);
    builder.end_node(); // level3

    builder.end_node(); // level2
    builder.end_node(); // level1
    builder.end_node(); // root

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("deep nesting should parse");

    assert_eq!(dt.device_count(), 3);

    let l1 = dt.find_first("test,l1").unwrap();
    assert_eq!(l1.base_address(), 0x1000);

    let l2 = dt.find_first("test,l2").unwrap();
    assert_eq!(l2.base_address(), 0x2000);

    let l3 = dt.find_first("test,l3").unwrap();
    assert_eq!(l3.base_address(), 0x3000);
}

/// Struct block truncated mid-property should return None, not panic.
#[test]
fn parse_struct_truncated_mid_property_returns_none() {
    let header_size = 40usize;
    let mut structs = Vec::new();

    // FDT_BEGIN_NODE.
    structs.extend_from_slice(&1u32.to_be_bytes());
    structs.push(0); // empty name
    while structs.len() % 4 != 0 {
        structs.push(0);
    }

    // FDT_PROP with len=100 but only 4 bytes of data follow.
    structs.extend_from_slice(&3u32.to_be_bytes()); // FDT_PROP
    structs.extend_from_slice(&100u32.to_be_bytes()); // len
    structs.extend_from_slice(&0u32.to_be_bytes()); // nameoff
                                                    // Only 4 bytes of prop data (need 100).
    structs.extend_from_slice(&0u32.to_be_bytes());

    let strings = b"x\0";
    let off_dt_struct = header_size;
    let off_dt_strings = header_size + structs.len();
    let totalsize = off_dt_strings + strings.len();
    let mut blob = Vec::with_capacity(totalsize);

    blob.extend_from_slice(&0xD00D_FEEDu32.to_be_bytes());
    blob.extend_from_slice(&(totalsize as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_struct as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_strings as u32).to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&17u32.to_be_bytes());
    blob.extend_from_slice(&16u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&(strings.len() as u32).to_be_bytes());
    blob.extend_from_slice(&(structs.len() as u32).to_be_bytes());
    blob.extend_from_slice(&structs);
    blob.extend_from_slice(strings);

    // Parser should detect that offset + len > structs.len() and return None.
    assert!(device_tree::parse(&blob).is_none());
}

/// The struct offset pointing past the struct data should parse
/// as an empty tree (loop immediately exits).
#[test]
fn parse_struct_offset_at_end_returns_empty() {
    let header_size = 40usize;
    // Struct block is just FDT_END.
    let structs = 9u32.to_be_bytes();
    let strings = b"\0";
    let off_dt_struct = header_size;
    let off_dt_strings = header_size + structs.len();
    let totalsize = off_dt_strings + strings.len();
    let mut blob = Vec::with_capacity(totalsize);

    blob.extend_from_slice(&0xD00D_FEEDu32.to_be_bytes());
    blob.extend_from_slice(&(totalsize as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_struct as u32).to_be_bytes());
    blob.extend_from_slice(&(off_dt_strings as u32).to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&17u32.to_be_bytes());
    blob.extend_from_slice(&16u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&(strings.len() as u32).to_be_bytes());
    blob.extend_from_slice(&(structs.len() as u32).to_be_bytes());
    blob.extend_from_slice(&structs);
    blob.extend_from_slice(strings);

    let dt = device_tree::parse(&blob).expect("FDT_END only should parse");
    assert_eq!(dt.device_count(), 0);
}

/// Device with no base_address convenience defaults to 0.
#[test]
fn device_with_empty_regs_not_emitted() {
    // A device with compatible + reg but all partial entries (< 16 bytes)
    // yields an empty regs vec → node is skipped due to `!regs.is_empty()`.
    let mut builder = FdtBuilder::new();
    builder.begin_node("");
    builder.begin_node("ghost");
    builder.prop_str("compatible", "test,ghost");
    // reg with 0 bytes.
    let nameoff = builder.add_string("reg");
    builder.push_prop(nameoff, &[]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("empty reg should parse");
    assert_eq!(dt.device_count(), 0);
}

// --- Tests for memory_region() (DTB /memory node parsing) ---

/// A memory node with device_type = "memory" and reg should be captured.
#[test]
fn memory_region_from_device_type() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("memory@40000000");
    builder.prop_str("device_type", "memory");
    builder.prop_reg("reg", &[(0x4000_0000, 0x1000_0000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    // Memory node has no `compatible`, so device_count is 0.
    assert_eq!(dt.device_count(), 0);

    let (base, size) = dt.memory_region().expect("should find memory region");
    assert_eq!(base, 0x4000_0000);
    assert_eq!(size, 0x1000_0000);
}

/// memory_region returns None when no memory node is present.
#[test]
fn memory_region_none_without_memory_node() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert!(dt.memory_region().is_none());
}

/// A node with device_type != "memory" should not be captured as memory.
#[test]
fn memory_region_ignores_non_memory_device_type() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("cpu@0");
    builder.prop_str("device_type", "cpu");
    builder.prop_reg("reg", &[(0, 0)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert!(dt.memory_region().is_none());
}

/// A memory node without reg should not produce a memory region.
#[test]
fn memory_region_none_without_reg() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("memory@40000000");
    builder.prop_str("device_type", "memory");
    // No reg property.
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert!(dt.memory_region().is_none());
}

/// Memory node alongside normal devices: both should be captured.
#[test]
fn memory_region_coexists_with_devices() {
    let mut builder = FdtBuilder::new();

    builder.begin_node("");
    builder.begin_node("memory@40000000");
    builder.prop_str("device_type", "memory");
    builder.prop_reg("reg", &[(0x4000_0000, 0x2000_0000)]); // 512 MiB
    builder.end_node();
    builder.begin_node("uart@9000000");
    builder.prop_str("compatible", "arm,pl011");
    builder.prop_reg("reg", &[(0x0900_0000, 0x1000)]);
    builder.end_node();
    builder.end_node();

    let blob = builder.finish();
    let dt = device_tree::parse(&blob).expect("should parse");

    assert_eq!(dt.device_count(), 1); // Only the UART is a "device"
    assert!(dt.find_first("arm,pl011").is_some());

    let (base, size) = dt.memory_region().expect("should find memory");
    assert_eq!(base, 0x4000_0000);
    assert_eq!(size, 0x2000_0000);
}
