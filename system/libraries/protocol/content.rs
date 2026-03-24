//! Content Region shared memory layout.
//!
//! The Content Region is a persistent shared memory region containing decoded
//! rendering data (font TTF bytes, decoded image pixels). Allocated by init,
//! managed by core (sole writer), read-only for render services.
//!
//! Layout: `[ContentRegionHeader | entries[MAX] | padding to CONTENT_HEADER_SIZE | data area]`

/// Magic number for Content Region validation ("CONT" in ASCII).
pub const CONTENT_REGION_MAGIC: u32 = 0x434F_4E54;
/// Current Content Region format version.
pub const CONTENT_REGION_VERSION: u32 = 1;
/// Maximum number of content entries in the registry.
pub const MAX_CONTENT_ENTRIES: usize = 64;
/// Total header size in bytes (header struct + padding for alignment).
/// Data area starts at this offset from the Content Region base.
pub const CONTENT_HEADER_SIZE: usize = 2048;

// ── Well-known content IDs ──────────────────────────────────────────

/// Unused/invalid content ID.
pub const CONTENT_ID_NONE: u32 = 0;
/// Monospace font (JetBrains Mono) — rendering data for glyph rasterization.
pub const CONTENT_ID_FONT_MONO: u32 = 1;
/// Sans-serif font (Inter) — rendering data for chrome text.
pub const CONTENT_ID_FONT_SANS: u32 = 2;
/// Serif font (Source Serif 4) — rendering data for body text.
pub const CONTENT_ID_FONT_SERIF: u32 = 3;
/// First dynamically assigned content ID (for decoded images, etc.).
pub const CONTENT_ID_DYNAMIC_START: u32 = 16;

// ── Content class ───────────────────────────────────────────────────

/// Classification of content stored in a Content Region entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ContentClass {
    /// Font rendering data (TTF bytes). Read by render services for
    /// glyph rasterization.
    Font = 0,
    /// Decoded pixel data (BGRA8888). Referenced by Content::Image
    /// nodes in the scene graph via content_id.
    Pixels = 1,
}

// ── Content entry ───────────────────────────────────────────────────

/// A single entry in the Content Region registry.
///
/// Each entry describes one block of data in the Content Region's data area.
/// Entries are immutable once written (write-once semantics for lock-free
/// concurrent reads by the compositor).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ContentEntry {
    /// Unique content ID. 0 = unused slot. Well-known IDs (1-15) for
    /// fonts; dynamic IDs (≥16) for decoded images.
    pub content_id: u32,
    /// Byte offset from the Content Region base address.
    pub offset: u32,
    /// Byte length of the content data.
    pub length: u32,
    /// Content class (Font, Pixels). Stored as u8 for repr(C) stability.
    pub class: u8,
    pub _pad: [u8; 3],
    /// For Pixels: source image width in pixels. 0 for Font.
    pub width: u16,
    /// For Pixels: source image height in pixels. 0 for Font.
    pub height: u16,
    /// Scene graph generation when this entry was created (for future
    /// generation-based GC). 0 for entries created at boot.
    pub generation: u32,
}

const _: () = assert!(core::mem::size_of::<ContentEntry>() == 24);

impl ContentEntry {
    /// An empty/unused entry.
    pub const EMPTY: Self = Self {
        content_id: CONTENT_ID_NONE,
        offset: 0,
        length: 0,
        class: 0,
        _pad: [0; 3],
        width: 0,
        height: 0,
        generation: 0,
    };
}

// ── Content Region header ───────────────────────────────────────────

/// Header at the start of the Content Region shared memory.
///
/// Written by init (font entries) and core (decoded image entries).
/// Read by render services to locate font data and image pixels.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ContentRegionHeader {
    /// Magic number for validation (CONTENT_REGION_MAGIC).
    pub magic: u32,
    /// Format version (CONTENT_REGION_VERSION).
    pub version: u32,
    /// Number of active entries in the registry.
    pub entry_count: u32,
    /// Maximum entries (MAX_CONTENT_ENTRIES).
    pub max_entries: u32,
    /// Byte offset where the data area starts (CONTENT_HEADER_SIZE).
    pub data_offset: u32,
    /// Bump allocator: next free byte offset in the data area
    /// (relative to Content Region base, not data area start).
    pub next_alloc: u32,
    /// Reserved for future use.
    pub _reserved: [u32; 2],
    /// Registry entries.
    pub entries: [ContentEntry; MAX_CONTENT_ENTRIES],
}

// Header struct must fit within CONTENT_HEADER_SIZE.
const _: () = assert!(core::mem::size_of::<ContentRegionHeader>() <= CONTENT_HEADER_SIZE);

// ── Lookup ──────────────────────────────────────────────────────────

/// Find a content entry by ID. Returns the first entry with a matching
/// `content_id`, or `None` if not found. Linear scan — fine for ≤64 entries.
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
