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
}
