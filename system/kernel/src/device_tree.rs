//! Minimal Flattened Device Tree (FDT/DTB) parser.
//!
//! Parses the device tree blob passed by firmware at boot to discover
//! devices. Replaces hardcoded MMIO addresses, making the kernel
//! portable across hardware configurations.
//!
//! # FDT Format
//!
//! The blob has three sections: a header (40 bytes), a structure block
//! (tree of nodes with properties encoded as tokens), and a strings
//! block (property name strings). All multi-byte values are big-endian.
//!
//! # Assumptions
//!
//! - `#address-cells = 2`, `#size-cells = 2` (QEMU virt standard).
//!   This means each reg entry is 16 bytes (two 64-bit values).
//! - GIC interrupt encoding: 3 cells per interrupt (type, number, flags).
//!   SPI (type=0): hardware IRQ = number + 32. PPI (type=1): IRQ = number + 16.

use alloc::string::String;
use alloc::vec::Vec;

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const HEADER_SIZE: usize = 40;

/// A device discovered in the device tree.
#[derive(Clone, Debug)]
pub struct Device {
    /// Compatible string (first value if multiple).
    pub compatible: String,
    /// Address regions from the `reg` property (base, size pairs).
    pub regs: Vec<(u64, u64)>,
    /// Interrupt number (GIC SPI/PPI adjusted), if present.
    pub irq: Option<u32>,
}
/// Parsed device table — flat list of discovered devices.
pub struct DeviceTable {
    devices: Vec<Device>,
}

impl Device {
    /// Base address of the first region (convenience).
    pub fn base_address(&self) -> u64 {
        self.regs.first().map_or(0, |&(addr, _)| addr)
    }
    /// Size of the first region (convenience).
    pub fn size(&self) -> u64 {
        self.regs.first().map_or(0, |&(_, size)| size)
    }
}
impl DeviceTable {
    /// Total number of discovered devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }
    /// Find all devices matching a compatible string.
    pub fn find_all<'a>(&'a self, compatible: &'a str) -> impl Iterator<Item = &'a Device> + 'a {
        self.devices
            .iter()
            .filter(move |d| d.compatible == compatible)
    }
    /// Find the first device matching a compatible string.
    pub fn find_first(&self, compatible: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.compatible == compatible)
    }
}

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
/// Read a null-terminated string from a byte slice at the given offset.
fn read_cstr<'a>(data: &'a [u8], offset: usize) -> &'a str {
    if offset >= data.len() {
        return "";
    }

    let start = offset;
    let mut end = start;

    while end < data.len() && data[end] != 0 {
        end += 1;
    }

    core::str::from_utf8(&data[start..end]).unwrap_or("")
}
/// Read a null-terminated string into an owned String.
fn read_cstr_owned(data: &[u8], offset: usize) -> String {
    String::from(read_cstr(data, offset))
}

/// Parse a Flattened Device Tree blob.
///
/// The blob must start with the FDT magic number (0xD00DFEED).
/// Returns None if the blob is invalid or too small.
pub fn parse(blob: &[u8]) -> Option<DeviceTable> {
    if blob.len() < HEADER_SIZE {
        return None;
    }

    let magic = read_be_u32(blob, 0);

    if magic != FDT_MAGIC {
        return None;
    }

    let totalsize = read_be_u32(blob, 4) as usize;

    if totalsize > blob.len() {
        return None;
    }

    let off_dt_struct = read_be_u32(blob, 8) as usize;
    let off_dt_strings = read_be_u32(blob, 12) as usize;

    if off_dt_struct >= totalsize || off_dt_strings > totalsize {
        return None;
    }

    let structs = blob.get(off_dt_struct..totalsize)?;
    let strings = blob.get(off_dt_strings..totalsize)?;
    let mut devices = Vec::new();
    let mut offset = 0usize;
    // Per-node property accumulator.
    let mut node_compatible: Option<String> = None;
    let mut node_regs: Option<Vec<(u64, u64)>> = None;
    let mut node_irq: Option<u32> = None;

    loop {
        if offset + 4 > structs.len() {
            break;
        }

        let token = read_be_u32(structs, offset);

        offset += 4;

        match token {
            FDT_BEGIN_NODE => {
                // Skip node name (null-terminated, padded to 4 bytes).
                while offset < structs.len() && structs[offset] != 0 {
                    offset += 1;
                }

                if offset < structs.len() {
                    offset += 1; // skip null terminator
                }

                offset = align4(offset);
                // Reset accumulator for new node.
                node_compatible = None;
                node_regs = None;
                node_irq = None;
            }
            FDT_END_NODE => {
                // Emit device if this node had a compatible string and reg.
                if let (Some(compat), Some(regs)) = (node_compatible.take(), node_regs.take()) {
                    if !regs.is_empty() {
                        devices.push(Device {
                            compatible: compat,
                            regs,
                            irq: node_irq.take(),
                        });
                    }
                }

                node_irq = None;
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

                let prop_data = &structs[offset..offset + len];
                let prop_name = read_cstr(strings, nameoff);

                match prop_name {
                    "compatible" => {
                        // Take first null-terminated string from the value.
                        node_compatible = Some(read_cstr_owned(prop_data, 0));
                    }
                    "reg" => {
                        // #address-cells=2, #size-cells=2 → 16 bytes per entry.
                        let mut regs = Vec::new();
                        let mut pos = 0;

                        while pos + 16 <= prop_data.len() {
                            let addr = read_be_u64(prop_data, pos);
                            let size = read_be_u64(prop_data, pos + 8);

                            regs.push((addr, size));

                            pos += 16;
                        }

                        node_regs = Some(regs);
                    }
                    "interrupts" => {
                        // GIC: 3 cells per interrupt (type, number, flags).
                        // SPI (type=0): hardware IRQ = number + 32.
                        // PPI (type=1): hardware IRQ = number + 16.
                        if prop_data.len() >= 12 {
                            let irq_type = read_be_u32(prop_data, 0);
                            let irq_num = read_be_u32(prop_data, 4);

                            node_irq = Some(if irq_type == 0 {
                                irq_num + 32
                            } else {
                                irq_num + 16
                            });
                        }
                    }
                    _ => {} // Ignore other properties.
                }

                offset = align4(offset + len);
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return None, // Invalid token.
        }
    }

    Some(DeviceTable { devices })
}
