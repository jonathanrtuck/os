//! Pure functional ELF64 parser for loading userspace binaries.
//!
//! Parses the minimum needed to load statically-linked executables: the ELF
//! header (magic, class, machine, entry point, program header table location)
//! and PT_LOAD segments (vaddr, file data, mem size, permissions). Ignores
//! section headers, dynamic linking, and relocations.
//!
//! All functions take `&[u8]` and return `Result`. No allocation, no mutation.
//! Trusts that the ELF comes from the build system (no adversarial input).
//! A production loader would need overflow-checked arithmetic in segment_data.

use super::addr_space::PageAttrs;

#[derive(Debug)]
pub enum Error {
    TooSmall,
    BadMagic,
    NotElf64,
    NotLittleEndian,
    NotExecutable,
    NotAarch64,
    BadPhEntSize,
    SegmentOutOfBounds,
}

pub struct Header {
    pub entry: u64,
    pub ph_offset: u64,
    pub ph_count: u16,
    pub ph_ent_size: u16,
}
pub struct LoadSegment {
    pub vaddr: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub flags: u32,
}

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_AARCH64: u16 = 183;
const ET_EXEC: u16 = 2;
const PF_W: u32 = 2;
const PF_X: u32 = 1;
const PT_LOAD: u32 = 1;

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
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

/// Returns `Some(LoadSegment)` for PT_LOAD, `None` for other types.
pub fn load_segment(
    data: &[u8],
    header: &Header,
    index: u16,
) -> Result<Option<LoadSegment>, Error> {
    let offset = header.ph_offset as usize + (index as usize) * (header.ph_ent_size as usize);
    let end = offset + header.ph_ent_size as usize;

    if end > data.len() {
        return Err(Error::SegmentOutOfBounds);
    }

    let p_type = read_u32_le(data, offset);

    if p_type != PT_LOAD {
        return Ok(None);
    }

    Ok(Some(LoadSegment {
        flags: read_u32_le(data, offset + 4),
        file_offset: read_u64_le(data, offset + 8),
        vaddr: read_u64_le(data, offset + 16),
        file_size: read_u64_le(data, offset + 32),
        mem_size: read_u64_le(data, offset + 40),
    }))
}
pub fn parse_header(data: &[u8]) -> Result<Header, Error> {
    if data.len() < 64 {
        return Err(Error::TooSmall);
    }
    if data[0..4] != ELF_MAGIC {
        return Err(Error::BadMagic);
    }
    if data[4] != ELFCLASS64 {
        return Err(Error::NotElf64);
    }
    if data[5] != ELFDATA2LSB {
        return Err(Error::NotLittleEndian);
    }
    if read_u16_le(data, 16) != ET_EXEC {
        return Err(Error::NotExecutable);
    }
    if read_u16_le(data, 18) != EM_AARCH64 {
        return Err(Error::NotAarch64);
    }

    let ph_ent_size = read_u16_le(data, 54);

    if ph_ent_size < 56 {
        return Err(Error::BadPhEntSize);
    }

    Ok(Header {
        entry: read_u64_le(data, 24),
        ph_offset: read_u64_le(data, 32),
        ph_count: read_u16_le(data, 56),
        ph_ent_size,
    })
}
/// Map ELF segment flags to page table attributes.
///
/// Priority: X > W > RO. A segment with both W and X gets RX (W^X enforcement).
pub fn segment_attrs(flags: u32) -> PageAttrs {
    if flags & PF_X != 0 {
        PageAttrs::user_rx()
    } else if flags & PF_W != 0 {
        PageAttrs::user_rw()
    } else {
        PageAttrs::user_ro()
    }
}
pub fn segment_data<'a>(data: &'a [u8], seg: &LoadSegment) -> Result<&'a [u8], Error> {
    let start = seg.file_offset as usize;
    let end = start + seg.file_size as usize;

    if end > data.len() {
        return Err(Error::SegmentOutOfBounds);
    }

    Ok(&data[start..end])
}
