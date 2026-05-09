//! PNG decoder service protocol — decodes PNG image data to BGRA pixels.
//!
//! Transport: sync call/reply with VMO handle transfer.
//!
//! ```text
//! Decode(file_size)  + handle[0] = png_data_vmo
//!   → Ok(width, height, pixel_size) + handle[0] = bgra_pixel_vmo
//!   | Error(status)
//! ```

#![no_std]

pub use ipc::MAX_PAYLOAD;

pub const DECODE: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeRequest {
    pub file_size: u32,
}

impl DecodeRequest {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.file_size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            file_size: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeReply {
    pub width: u32,
    pub height: u32,
    pub pixel_size: u32,
}

impl DecodeReply {
    pub const SIZE: usize = 12;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.width.to_le_bytes());
        buf[4..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.pixel_size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            pixel_size: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_request_round_trip() {
        let req = DecodeRequest { file_size: 65536 };
        let mut buf = [0u8; DecodeRequest::SIZE];
        req.write_to(&mut buf);
        assert_eq!(DecodeRequest::read_from(&buf), req);
    }

    #[test]
    fn decode_reply_round_trip() {
        let reply = DecodeReply {
            width: 1920,
            height: 1080,
            pixel_size: 1920 * 1080 * 4,
        };
        let mut buf = [0u8; DecodeReply::SIZE];
        reply.write_to(&mut buf);
        assert_eq!(DecodeReply::read_from(&buf), reply);
    }

    #[test]
    fn sizes_fit_payload() {
        assert!(DecodeRequest::SIZE <= MAX_PAYLOAD);
        assert!(DecodeReply::SIZE <= MAX_PAYLOAD);
    }
}
