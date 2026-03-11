//! Host-side tests for the device tree (FDT) parser.
//!
//! Constructs minimal FDT blobs in memory and verifies parsing.

extern crate alloc;

#[path = "../../kernel/device_tree.rs"]
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
