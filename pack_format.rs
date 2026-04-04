// Service pack format — single source of truth.
//
// This file is `include!`'d by build.rs, protocol, and tools/mkservices.
// Defines the binary format for the flat service archive that replaces
// the include_bytes! embedding chain.
//
// RULES (same as system_config.rs):
// - All integer types are explicit-width (u32/u64).
// - Only format constants and types belong here.
// - Changing a value here requires rebuilding everything. That is the point.

// --- Pack header ---

/// Magic number: "SVCP" (Service Pack) in little-endian.
pub const PACK_MAGIC: u32 = 0x5043_5653;

/// Format version. Increment if the header or entry layout changes.
pub const PACK_VERSION: u32 = 1;

/// Size of the pack header in bytes (4 x u32).
pub const PACK_HEADER_SIZE: u32 = 16;

/// Size of each entry descriptor in bytes (4 x u32).
pub const PACK_ENTRY_SIZE: u32 = 16;

// --- Service role IDs ---
//
// Stable identifiers shared between build tools (mkservices) and userspace
// (init). The pack uses these instead of virtio device IDs because not all
// services are device drivers.

pub const ROLE_STORE: u32 = 1;
pub const ROLE_DOCUMENT: u32 = 2;
pub const ROLE_LAYOUT: u32 = 3;
pub const ROLE_VIRTIO_BLK: u32 = 4;
pub const ROLE_VIRTIO_CONSOLE: u32 = 5;
pub const ROLE_VIRTIO_INPUT: u32 = 6;
pub const ROLE_VIRTIO_9P: u32 = 7;
pub const ROLE_PRESENTER: u32 = 8;
pub const ROLE_METAL_RENDER: u32 = 9;
pub const ROLE_PNG_DECODE: u32 = 10;
pub const ROLE_TEXT_EDITOR: u32 = 11;
pub const ROLE_RICH_EDITOR: u32 = 12;
pub const ROLE_STRESS: u32 = 13;
pub const ROLE_FUZZ: u32 = 14;
