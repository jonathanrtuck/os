#![no_std]

//! Block device protocol — sector-level I/O for the virtio-blk driver.
//!
//! Transport: sync call/reply (store → blk driver).
//!
//! Data transfer uses a shared VMO established once via SETUP. Read and
//! write requests reference offsets within that VMO. The block driver
//! copies between the shared VMO and its DMA buffer.
//!
//! Block size is 16 KiB (matching the kernel page size and filesystem
//! block size). Capacity is reported in blocks.

pub const SETUP: u32 = 1;
pub const READ_BLOCK: u32 = 2;
pub const WRITE_BLOCK: u32 = 3;
pub const FLUSH: u32 = 4;
pub const GET_INFO: u32 = 5;

pub const SECTOR_SIZE: u32 = 512;
pub const BLOCK_SIZE: u32 = 16384;
pub const SECTORS_PER_BLOCK: u32 = BLOCK_SIZE / SECTOR_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRequest {
    pub block_index: u32,
    pub vmo_offset: u32,
}

impl BlockRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.block_index.to_le_bytes());
        buf[4..8].copy_from_slice(&self.vmo_offset.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            block_index: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            vmo_offset: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub capacity_blocks: u32,
    pub has_flush: u8,
}

impl InfoReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.capacity_blocks.to_le_bytes());
        buf[4] = self.has_flush;
        buf[5..8].copy_from_slice(&[0; 3]);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            capacity_blocks: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            has_flush: buf[4],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_request_round_trip() {
        let req = BlockRequest {
            block_index: 42,
            vmo_offset: 0x4000,
        };
        let mut buf = [0u8; BlockRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = BlockRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            capacity_blocks: 1024,
            has_flush: 1,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = InfoReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn block_constants() {
        assert_eq!(SECTORS_PER_BLOCK, 32);
        assert_eq!(BLOCK_SIZE, 16384);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [SETUP, READ_BLOCK, WRITE_BLOCK, FLUSH, GET_INFO];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn sizes_fit_payload() {
        assert!(BlockRequest::SIZE <= 120);
        assert!(InfoReply::SIZE <= 120);
    }

    #[test]
    fn block_request_zero() {
        let req = BlockRequest {
            block_index: 0,
            vmo_offset: 0,
        };
        let mut buf = [0u8; BlockRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(buf, [0; 8]);
    }

    #[test]
    fn info_reply_no_flush() {
        let reply = InfoReply {
            capacity_blocks: 500,
            has_flush: 0,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = InfoReply::read_from(&buf);

        assert_eq!(decoded.has_flush, 0);
    }
}
