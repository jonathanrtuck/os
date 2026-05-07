//! Bootstrap protocol — one-shot config delivery from init to services.
//!
//! Transport: sync call/reply (init calls the service's bootstrap
//! endpoint).
//!
//! Handle slots:
//! - `[0]` = name service endpoint (all services except name service)
//! - `[1..]` = service-specific (MMIO regions for drivers, etc.)

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

pub const SERVICE_COUNT: usize = 10;

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
        assert!(BootstrapConfig::SIZE <= crate::MAX_PAYLOAD);
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
