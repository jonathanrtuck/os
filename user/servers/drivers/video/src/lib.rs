#![no_std]

//! Codec-decode service protocol — video decode via virtio-video.
//!
//! Transport: sync call/reply (client -> codec-decode driver).
//!
//! Data transfer for compressed frames uses a shared VMO established via
//! SETUP. The client writes compressed data into the shared VMO, then
//! calls DECODE_FRAME with offset/size. The driver submits to the virtio
//! decode queue and returns decoded frame status.

pub const PIXEL_OFFSET: usize = 8;

// Codec constants
pub const CODEC_MJPEG: u8 = 0;
pub const CODEC_H264: u8 = 1;
pub const CODEC_HEVC: u8 = 2;
pub const CODEC_VP9: u8 = 3;
pub const CODEC_AV1: u8 = 4;

// IPC methods
pub const SETUP: u32 = 1;
pub const GET_INFO: u32 = 2;
pub const CREATE_SESSION: u32 = 3;
pub const DECODE_FRAME: u32 = 4;
pub const DESTROY_SESSION: u32 = 5;
pub const FLUSH_SESSION: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub supported_codecs: u32,
    pub max_width: u32,
    pub max_height: u32,
}

impl InfoReply {
    pub const SIZE: usize = 12;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.supported_codecs.to_le_bytes());
        buf[4..8].copy_from_slice(&self.max_width.to_le_bytes());
        buf[8..12].copy_from_slice(&self.max_height.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            supported_codecs: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            max_width: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            max_height: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateSessionRequest {
    pub codec: u8,
    pub width: u32,
    pub height: u32,
}

impl CreateSessionRequest {
    pub const SIZE: usize = 12;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&(self.codec as u32).to_le_bytes());
        buf[4..8].copy_from_slice(&self.width.to_le_bytes());
        buf[8..12].copy_from_slice(&self.height.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            codec: u32::from_le_bytes(buf[0..4].try_into().unwrap()) as u8,
            width: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            height: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateSessionReply {
    pub session_id: u32,
    pub texture_handle: u32,
}

impl CreateSessionReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.session_id.to_le_bytes());
        buf[4..8].copy_from_slice(&self.texture_handle.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            session_id: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            texture_handle: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeFrameRequest {
    pub session_id: u32,
    pub offset: u32,
    pub size: u32,
    pub timestamp_ns: u64,
}

impl DecodeFrameRequest {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.session_id.to_le_bytes());
        buf[4..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..12].copy_from_slice(&self.size.to_le_bytes());
        buf[12..20].copy_from_slice(&self.timestamp_ns.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            session_id: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            offset: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            size: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            timestamp_ns: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeFrameReply {
    pub status: u32,
    pub bytes_written: u32,
    pub timestamp_ns: u64,
    pub duration_ns: u64,
}

impl DecodeFrameReply {
    pub const SIZE: usize = 24;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.status.to_le_bytes());
        buf[4..8].copy_from_slice(&self.bytes_written.to_le_bytes());
        buf[8..16].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[16..24].copy_from_slice(&self.duration_ns.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            status: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            bytes_written: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            timestamp_ns: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            duration_ns: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRequest {
    pub session_id: u32,
}

impl SessionRequest {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.session_id.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            session_id: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            supported_codecs: 0x1F,
            max_width: 3840,
            max_height: 2160,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = InfoReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn create_session_request_round_trip() {
        let req = CreateSessionRequest {
            codec: CODEC_H264,
            width: 1920,
            height: 1080,
        };
        let mut buf = [0u8; CreateSessionRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = CreateSessionRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn create_session_reply_round_trip() {
        let reply = CreateSessionReply {
            session_id: 42,
            texture_handle: 7,
        };
        let mut buf = [0u8; CreateSessionReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = CreateSessionReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn decode_frame_request_round_trip() {
        let req = DecodeFrameRequest {
            session_id: 1,
            offset: 0,
            size: 65536,
            timestamp_ns: 16_666_667,
        };
        let mut buf = [0u8; DecodeFrameRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = DecodeFrameRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn decode_frame_reply_round_trip() {
        let reply = DecodeFrameReply {
            status: 0,
            bytes_written: 8294400,
            timestamp_ns: 16_666_667,
            duration_ns: 33_333_333,
        };
        let mut buf = [0u8; DecodeFrameReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = DecodeFrameReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn session_request_round_trip() {
        let req = SessionRequest { session_id: 99 };
        let mut buf = [0u8; SessionRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = SessionRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [
            SETUP,
            GET_INFO,
            CREATE_SESSION,
            DECODE_FRAME,
            DESTROY_SESSION,
            FLUSH_SESSION,
        ];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn codec_constants_distinct() {
        let codecs = [CODEC_MJPEG, CODEC_H264, CODEC_HEVC, CODEC_VP9, CODEC_AV1];

        for i in 0..codecs.len() {
            for j in (i + 1)..codecs.len() {
                assert_ne!(codecs[i], codecs[j]);
            }
        }
    }

    #[test]
    fn sizes_fit_payload() {
        assert!(InfoReply::SIZE <= ipc::MAX_PAYLOAD);
        assert!(CreateSessionRequest::SIZE <= ipc::MAX_PAYLOAD);
        assert!(CreateSessionReply::SIZE <= ipc::MAX_PAYLOAD);
        assert!(DecodeFrameRequest::SIZE <= ipc::MAX_PAYLOAD);
        assert!(DecodeFrameReply::SIZE <= ipc::MAX_PAYLOAD);
        assert!(SessionRequest::SIZE <= ipc::MAX_PAYLOAD);
    }
}
