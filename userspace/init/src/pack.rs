//! SVPK service pack parser — reads the header and entry table from
//! a memory-mapped service pack VMO.

const MAGIC: [u8; 4] = *b"SVPK";
const VERSION: u32 = 1;

const HEADER_SIZE: usize = 16;
const ENTRY_SIZE: usize = 48;

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
}

pub fn read_header(base: *const u8) -> PackHeader {
    // SAFETY: caller guarantees base points to at least HEADER_SIZE readable bytes.
    unsafe {
        let mut magic = [0u8; 4];

        core::ptr::copy_nonoverlapping(base, magic.as_mut_ptr(), 4);

        PackHeader {
            magic,
            version: read_u32(base, 4),
            count: read_u32(base, 8),
            total_size: read_u32(base, 12),
        }
    }
}

pub fn read_entry(base: *const u8, index: usize) -> PackEntry {
    let offset = HEADER_SIZE + index * ENTRY_SIZE;

    // SAFETY: caller guarantees base + offset + ENTRY_SIZE is within the mapping.
    unsafe {
        PackEntry {
            offset: read_u32(base, offset + 32),
            size: read_u32(base, offset + 36),
        }
    }
}

unsafe fn read_u32(base: *const u8, offset: usize) -> u32 {
    let mut bytes = [0u8; 4];

    // SAFETY: caller guarantees base + offset + 4 is readable.
    unsafe {
        core::ptr::copy_nonoverlapping(base.add(offset), bytes.as_mut_ptr(), 4);
    }

    u32::from_le_bytes(bytes)
}
