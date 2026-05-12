//! Video decoder service protocol.
//!
//! ```text
//! Open(file_size) + handle[0] = file_vmo
//!   → Ok(width, height, ns_per_frame, total_frames) + handle[0] = frame_vmo
//!   | Error(status)
//!
//! DecodeFrame(frame_index)
//!   → Ok(pixel_size)
//!   | Error(status)
//!
//! Toggle()
//!   → Ok(playing)
//!
//! Pause()
//!   → Ok()
//!
//! Close()
//!   → Ok()
//! ```

#![no_std]

pub use ipc::MAX_PAYLOAD;

pub const OPEN: u32 = 1;
pub const DECODE_FRAME: u32 = 2;
pub const CLOSE: u32 = 3;
pub const TOGGLE: u32 = 4;
pub const PAUSE: u32 = 5;

pub const GEN_HEADER_SIZE: usize = 2 * core::mem::size_of::<u64>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToggleReply {
    pub playing: u8,
}

impl ToggleReply {
    pub const SIZE: usize = 1;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.playing;
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self { playing: buf[0] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRequest {
    pub file_size: u32,
}

impl OpenRequest {
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
pub struct OpenReply {
    pub width: u32,
    pub height: u32,
    pub ns_per_frame: u64,
    pub total_frames: u32,
    pub texture_handle: u32,
}

impl OpenReply {
    pub const SIZE: usize = 24;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.width.to_le_bytes());
        buf[4..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..16].copy_from_slice(&self.ns_per_frame.to_le_bytes());
        buf[16..20].copy_from_slice(&self.total_frames.to_le_bytes());
        buf[20..24].copy_from_slice(&self.texture_handle.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            ns_per_frame: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            total_frames: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            texture_handle: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeFrameRequest {
    pub frame_index: u32,
}

impl DecodeFrameRequest {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.frame_index.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            frame_index: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeFrameReply {
    pub pixel_size: u32,
}

impl DecodeFrameReply {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.pixel_size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            pixel_size: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_request_round_trip() {
        let req = OpenRequest {
            file_size: 1_000_000,
        };
        let mut buf = [0u8; OpenRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(OpenRequest::read_from(&buf), req);
    }

    #[test]
    fn open_reply_round_trip() {
        let reply = OpenReply {
            width: 1280,
            height: 720,
            ns_per_frame: 33_333_000,
            total_frames: 900,
            texture_handle: 0x8000_0001,
        };
        let mut buf = [0u8; OpenReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(OpenReply::read_from(&buf), reply);
    }

    #[test]
    fn decode_frame_request_round_trip() {
        let req = DecodeFrameRequest { frame_index: 42 };
        let mut buf = [0u8; DecodeFrameRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(DecodeFrameRequest::read_from(&buf), req);
    }

    #[test]
    fn decode_frame_reply_round_trip() {
        let reply = DecodeFrameReply {
            pixel_size: 1280 * 720 * 4,
        };
        let mut buf = [0u8; DecodeFrameReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(DecodeFrameReply::read_from(&buf), reply);
    }

    #[test]
    fn toggle_reply_round_trip() {
        let reply = ToggleReply { playing: 1 };
        let mut buf = [0u8; ToggleReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(ToggleReply::read_from(&buf), reply);
    }

    #[test]
    fn sizes_fit_payload() {
        assert!(OpenRequest::SIZE <= MAX_PAYLOAD);
        assert!(OpenReply::SIZE <= MAX_PAYLOAD);
        assert!(DecodeFrameRequest::SIZE <= MAX_PAYLOAD);
        assert!(DecodeFrameReply::SIZE <= MAX_PAYLOAD);
        assert!(ToggleReply::SIZE <= MAX_PAYLOAD);
    }
}
