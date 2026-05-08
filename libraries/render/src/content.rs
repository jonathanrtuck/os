//! Content Region shared memory layout (read-only types for the renderer).

pub const MAX_CONTENT_ENTRIES: usize = 64;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ContentEntry {
    pub content_id: u32,
    pub offset: u32,
    pub length: u32,
    pub class: u8,
    pub _pad: [u8; 3],
    pub width: u16,
    pub height: u16,
    pub generation: u32,
}

const _: () = assert!(core::mem::size_of::<ContentEntry>() == 24);

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ContentRegionHeader {
    pub magic: u32,
    pub version: u32,
    pub entry_count: u32,
    pub max_entries: u32,
    pub data_offset: u32,
    pub next_alloc: u32,
    pub _reserved: [u32; 2],
    pub entries: [ContentEntry; MAX_CONTENT_ENTRIES],
}

pub fn find_entry(header: &ContentRegionHeader, content_id: u32) -> Option<&ContentEntry> {
    let count = header.entry_count as usize;
    if count > MAX_CONTENT_ENTRIES {
        return None;
    }
    for i in 0..count {
        if header.entries[i].content_id == content_id {
            return Some(&header.entries[i]);
        }
    }
    None
}
