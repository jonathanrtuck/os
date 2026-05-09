//! SVPK service pack parser — reads the header and entry table from
//! a memory-mapped service pack VMO.

const MAGIC: [u8; 4] = *b"SVPK";
const VERSION: u32 = 1;

pub const HEADER_SIZE: usize = 16;
const ENTRY_SIZE: usize = 48;
const NAME_LEN: usize = 32;

pub struct PackHeader {
    pub count: u32,
    pub total_size: u32,
    magic: [u8; 4],
    version: u32,
}

impl PackHeader {
    pub fn is_valid(&self) -> bool {
        self.magic == MAGIC && self.version == VERSION
    }
}

pub struct PackEntry {
    pub offset: u32,
    pub size: u32,
    pub data_offset: u32,
    pub mem_size: u32,
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub fn read_header(data: &[u8]) -> PackHeader {
    if data.len() < HEADER_SIZE {
        return PackHeader {
            magic: [0; 4],
            version: 0,
            count: 0,
            total_size: 0,
        };
    }

    let mut magic = [0u8; 4];

    magic.copy_from_slice(&data[0..4]);

    PackHeader {
        magic,
        version: read_u32_le(data, 4),
        count: read_u32_le(data, 8),
        total_size: read_u32_le(data, 12),
    }
}

pub fn read_entry(data: &[u8], index: usize) -> PackEntry {
    let offset = HEADER_SIZE + index * ENTRY_SIZE;

    PackEntry {
        offset: read_u32_le(data, offset + 32),
        size: read_u32_le(data, offset + 36),
        data_offset: read_u32_le(data, offset + 40),
        mem_size: read_u32_le(data, offset + 44),
    }
}

pub fn read_name(data: &[u8], index: usize) -> &[u8] {
    let offset = HEADER_SIZE + index * ENTRY_SIZE;
    let name_bytes = &data[offset..offset + NAME_LEN];
    let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);

    &name_bytes[..end]
}
