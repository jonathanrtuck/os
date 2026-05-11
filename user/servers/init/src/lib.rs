//! Bootstrap protocol — one-shot config delivery from init to services.
//!
//! Transport: sync call/reply (init calls the service's bootstrap
//! endpoint).
//!
//! Handle slots:
//! - `[0]` = name service endpoint (all services except name service)
//! - `[1..]` = service-specific (MMIO regions for drivers, etc.)

#![no_std]

use abi::types::{Handle, Rights, SyscallError};

pub const BOOTSTRAP: u32 = 1;

pub const SERVICE_NAME: u16 = 0;
pub const SERVICE_CONSOLE: u16 = 1;
pub const SERVICE_INPUT: u16 = 2;
pub const SERVICE_BLK: u16 = 3;
pub const SERVICE_RENDER: u16 = 4;
pub const SERVICE_STORE: u16 = 5;
pub const SERVICE_DOCUMENT: u16 = 6;
pub const SERVICE_LAYOUT: u16 = 7;
pub const SERVICE_PRESENTER: u16 = 8;
pub const SERVICE_EDITOR_TEXT: u16 = 9;
pub const SERVICE_PNG_DECODER: u16 = 10;
pub const SERVICE_9P: u16 = 11;
pub const SERVICE_FS: u16 = 12;
pub const SERVICE_JPEG_DECODER: u16 = 13;
pub const SERVICE_RNG: u16 = 14;
pub const SERVICE_SND: u16 = 15;
pub const SERVICE_AUDIO: u16 = 16;

pub const SERVICE_COUNT: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapConfig {
    pub service_id: u16,
    pub flags: u16,
}

impl BootstrapConfig {
    pub const SIZE: usize = 4;

    #[must_use]
    pub fn new(service_id: u16) -> Self {
        Self {
            service_id,
            flags: 0,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.service_id.to_le_bytes());
        buf[2..4].copy_from_slice(&self.flags.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            service_id: u16::from_le_bytes(buf[0..2].try_into().unwrap()),
            flags: u16::from_le_bytes(buf[2..4].try_into().unwrap()),
        }
    }
}

// ── Device manifest ─────────────────────────────────────────
//
// Written by the kernel into init's handle 3 VMO. Init reads it to
// discover device MMIO VMOs and IRQ bindings.
//
// Layout: [DeviceManifestHeader] [DeviceEntry × count]

pub const MANIFEST_MAGIC: u32 = 0x4456_4544; // "DEVD"

pub const DEV_UART: u8 = 0;
pub const DEV_VIRTIO: u8 = 1;

#[derive(Debug, Clone, Copy)]
pub struct DeviceManifestHeader {
    pub magic: u32,
    pub count: u32,
}

impl DeviceManifestHeader {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.magic.to_le_bytes());
        buf[4..8].copy_from_slice(&self.count.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            count: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == MANIFEST_MAGIC
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceEntry {
    pub device_type: u8,
    pub _pad: [u8; 3],
    pub handle_index: u32,
    pub irq: u32,
    pub mmio_offset: u32,
}

impl DeviceEntry {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.device_type;
        buf[1..4].copy_from_slice(&self._pad);
        buf[4..8].copy_from_slice(&self.handle_index.to_le_bytes());
        buf[8..12].copy_from_slice(&self.irq.to_le_bytes());
        buf[12..16].copy_from_slice(&self.mmio_offset.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            device_type: buf[0],
            _pad: [buf[1], buf[2], buf[3]],
            handle_index: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            irq: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            mmio_offset: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        }
    }
}

// ── Font pack VMO ────────────────────────────────────────────
//
// A shared read-only VMO containing all font data. Init creates it,
// passes RO handles to presenter, layout, and render. This avoids
// embedding 4+ MB of font data in each consumer binary.
//
// Layout: [magic: u32][count: u32][entries: (offset: u32, size: u32) × count][data]

pub const FONT_PACK_MAGIC: u32 = 0x544E_4F46; // "FONT" LE
pub const FONT_PACK_COUNT: usize = 6;
pub const FONT_PACK_HEADER: usize = 8 + FONT_PACK_COUNT * 8; // 56 bytes

pub const FONT_IDX_MONO: usize = 0;
pub const FONT_IDX_MONO_ITALIC: usize = 1;
pub const FONT_IDX_SANS: usize = 2;
pub const FONT_IDX_SANS_ITALIC: usize = 3;
pub const FONT_IDX_SERIF: usize = 4;
pub const FONT_IDX_SERIF_ITALIC: usize = 5;

/// Read font data slice from a mapped font pack VMO.
///
/// # Safety
/// `va` must point to a valid, mapped font pack VMO of sufficient size.
pub unsafe fn font_data(va: usize, index: usize) -> &'static [u8] {
    let entry_off = 8 + index * 8;
    // SAFETY: caller guarantees va points to a valid font pack VMO.
    let offset = unsafe { core::ptr::read_unaligned((va + entry_off) as *const u32) } as usize;
    let size = unsafe { core::ptr::read_unaligned((va + entry_off + 4) as *const u32) } as usize;

    unsafe { core::slice::from_raw_parts((va + offset) as *const u8, size) }
}

// ── DMA allocation protocol (init serves this) ────────────

pub const DMA_ALLOC: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaAllocRequest {
    pub size: u32,
}

impl DmaAllocRequest {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            size: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

// ── DMA buffer + client stub ──────────────────────────────

pub struct DmaBuf {
    pub va: usize,
    pub pa: u64,
}

pub fn request_dma(init_ep: Handle, size: usize) -> Result<DmaBuf, SyscallError> {
    let mut msg = [0u8; 128];
    let method = DMA_ALLOC;

    msg[0..4].copy_from_slice(&method.to_le_bytes());

    let req = DmaAllocRequest { size: size as u32 };

    req.write_to(&mut msg[4..8]);

    let mut recv_handles = [0u32; 4];
    let result = abi::ipc::call(init_ep, &mut msg, 8, &[], &mut recv_handles)?;

    if result.handle_count == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let vmo = Handle(recv_handles[0]);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let va = abi::vmo::map(vmo, 0, rw)?;

    Ok(DmaBuf { va, pa: va as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let config = BootstrapConfig {
            service_id: SERVICE_PRESENTER,
            flags: 0x00FF,
        };
        let mut buf = [0u8; 4];

        config.write_to(&mut buf);

        let decoded = BootstrapConfig::read_from(&buf);

        assert_eq!(config, decoded);
    }

    #[test]
    fn new_sets_zero_flags() {
        let config = BootstrapConfig::new(SERVICE_CONSOLE);

        assert_eq!(config.service_id, SERVICE_CONSOLE);
        assert_eq!(config.flags, 0);
    }

    #[test]
    fn all_service_ids_distinct() {
        let ids = [
            SERVICE_NAME,
            SERVICE_CONSOLE,
            SERVICE_INPUT,
            SERVICE_BLK,
            SERVICE_RENDER,
            SERVICE_STORE,
            SERVICE_DOCUMENT,
            SERVICE_LAYOUT,
            SERVICE_PRESENTER,
            SERVICE_EDITOR_TEXT,
        ];

        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j]);
            }
        }
    }

    #[test]
    fn little_endian_encoding() {
        let config = BootstrapConfig {
            service_id: 0x0102,
            flags: 0x0304,
        };
        let mut buf = [0u8; 4];

        config.write_to(&mut buf);

        assert_eq!(buf, [0x02, 0x01, 0x04, 0x03]);
    }

    #[test]
    fn size_fits_payload() {
        assert!(BootstrapConfig::SIZE <= 120);
    }

    #[test]
    fn manifest_header_round_trip() {
        let header = DeviceManifestHeader {
            magic: MANIFEST_MAGIC,
            count: 5,
        };
        let mut buf = [0u8; DeviceManifestHeader::SIZE];

        header.write_to(&mut buf);

        let decoded = DeviceManifestHeader::read_from(&buf);

        assert!(decoded.is_valid());
        assert_eq!(decoded.count, 5);
    }

    #[test]
    fn device_entry_round_trip() {
        let entry = DeviceEntry {
            device_type: DEV_VIRTIO,
            _pad: [0; 3],
            handle_index: 5,
            irq: 49,
            mmio_offset: 0x200,
        };
        let mut buf = [0u8; DeviceEntry::SIZE];

        entry.write_to(&mut buf);

        let decoded = DeviceEntry::read_from(&buf);

        assert_eq!(decoded.device_type, DEV_VIRTIO);
        assert_eq!(decoded.handle_index, 5);
        assert_eq!(decoded.irq, 49);
        assert_eq!(decoded.mmio_offset, 0x200);
    }

    #[test]
    fn dma_alloc_round_trip() {
        let req = DmaAllocRequest { size: 0x4000 };
        let mut buf = [0u8; DmaAllocRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = DmaAllocRequest::read_from(&buf);

        assert_eq!(decoded, req);
    }

    #[test]
    fn manifest_invalid_magic() {
        let header = DeviceManifestHeader {
            magic: 0xDEAD,
            count: 0,
        };

        assert!(!header.is_valid());
    }
}
