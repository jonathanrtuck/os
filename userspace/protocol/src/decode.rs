//! Decode protocol — content decoding (e.g., PNG → pixel buffer).
//!
//! Transport: sync call/reply (document → decoder).
//!
//! The request includes the MIME type and source data length. The
//! source VMO handle is transferred in IPC handle slot 0. The reply
//! includes decoded dimensions and format; the result VMO handle is
//! transferred back in handle slot 0.

pub const DECODE: u32 = 1;

pub const FORMAT_RGBA8: u32 = 0;
pub const FORMAT_BGRA8: u32 = 1;
pub const FORMAT_RGB8: u32 = 2;

/// Decode request — handle slot 0 carries the source VMO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeRequest {
    pub content_type: [u8; 32],
    pub source_len: u64,
}

impl DecodeRequest {
    pub const SIZE: usize = 40;

    #[must_use]
    pub fn new(mime: &[u8], source_len: u64) -> Self {
        let mut content_type = [0u8; 32];
        let len = mime.len().min(32);

        content_type[..len].copy_from_slice(&mime[..len]);

        Self {
            content_type,
            source_len,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..32].copy_from_slice(&self.content_type);
        buf[32..40].copy_from_slice(&self.source_len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        let mut content_type = [0u8; 32];

        content_type.copy_from_slice(&buf[0..32]);

        Self {
            content_type,
            source_len: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
        }
    }

    #[must_use]
    pub fn content_type_str(&self) -> &[u8] {
        &self.content_type[..crate::null_terminated_len(&self.content_type)]
    }
}

/// Decode reply — handle slot 0 carries the decoded pixel VMO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeReply {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

impl DecodeReply {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.width.to_le_bytes());
        buf[4..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.stride.to_le_bytes());
        buf[12..16].copy_from_slice(&self.format.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            stride: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            format: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        }
    }

    #[must_use]
    pub fn pixel_data_size(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_request_round_trip() {
        let req = DecodeRequest::new(b"image/png", 65536);
        let mut buf = [0u8; DecodeRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = DecodeRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn decode_request_content_type() {
        let req = DecodeRequest::new(b"image/png", 0);

        assert_eq!(req.content_type_str(), b"image/png");
    }

    #[test]
    fn decode_reply_round_trip() {
        let reply = DecodeReply {
            width: 800,
            height: 600,
            stride: 3200,
            format: FORMAT_RGBA8,
        };
        let mut buf = [0u8; DecodeReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = DecodeReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn pixel_data_size() {
        let reply = DecodeReply {
            width: 100,
            height: 100,
            stride: 400,
            format: FORMAT_RGBA8,
        };

        assert_eq!(reply.pixel_data_size(), 40_000);
    }

    #[test]
    fn pixel_data_size_zero() {
        let reply = DecodeReply {
            width: 0,
            height: 0,
            stride: 0,
            format: FORMAT_RGBA8,
        };

        assert_eq!(reply.pixel_data_size(), 0);
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(DecodeRequest::SIZE <= crate::MAX_PAYLOAD);
        assert!(DecodeReply::SIZE <= crate::MAX_PAYLOAD);
    }

    #[test]
    fn format_constants_distinct() {
        assert_ne!(FORMAT_RGBA8, FORMAT_BGRA8);
        assert_ne!(FORMAT_RGBA8, FORMAT_RGB8);
        assert_ne!(FORMAT_BGRA8, FORMAT_RGB8);
    }
}
