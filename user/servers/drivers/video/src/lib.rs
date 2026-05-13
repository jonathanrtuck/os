#![no_std]

//! Codec-decode service protocol — video decode via virtio-video.
//!
//! Transport: sync call/reply (client -> codec-decode driver).
//!
//! Data transfer for compressed frames uses a shared VMO established via
//! SETUP. The client writes compressed data into the shared VMO, then
//! calls DECODE_FRAME with offset/size. The driver submits to the virtio
//! decode queue and returns decoded frame status.

pub const PIXEL_OFFSET: usize = 16;

// Codec constants
pub const CODEC_MJPEG: u8 = 0;
pub const CODEC_H264: u8 = 1;
pub const CODEC_HEVC: u8 = 2;
pub const CODEC_VP9: u8 = 3;
pub const CODEC_AV1: u8 = 4;

// Audio codec constants
pub const AUDIO_CODEC_AAC: u8 = 0;

// IPC methods
pub const SETUP: u32 = 1;
pub const GET_INFO: u32 = 2;
pub const CREATE_SESSION: u32 = 3;
pub const DECODE_FRAME: u32 = 4;
pub const DESTROY_SESSION: u32 = 5;
pub const FLUSH_SESSION: u32 = 6;
pub const DECODE_AUDIO: u32 = 7;
pub const STOP_AUDIO: u32 = 8;

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
    pub codec_data_offset: u32,
    pub codec_data_size: u32,
}

impl CreateSessionRequest {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&(self.codec as u32).to_le_bytes());
        buf[4..8].copy_from_slice(&self.width.to_le_bytes());
        buf[8..12].copy_from_slice(&self.height.to_le_bytes());
        buf[12..16].copy_from_slice(&self.codec_data_offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.codec_data_size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            codec: u32::from_le_bytes(buf[0..4].try_into().unwrap()) as u8,
            width: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            height: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            codec_data_offset: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            codec_data_size: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
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
    pub output_pixel_offset: u32,
}

impl DecodeFrameRequest {
    pub const SIZE: usize = 24;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.session_id.to_le_bytes());
        buf[4..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..12].copy_from_slice(&self.size.to_le_bytes());
        buf[12..20].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[20..24].copy_from_slice(&self.output_pixel_offset.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            session_id: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            offset: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            size: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            timestamp_ns: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
            output_pixel_offset: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
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

/// Batch audio decode request.
///
/// The shared VMO layout (set up via SETUP) is:
///   [config_bytes (config_size)] [frame_sizes (4 * num_frames)] [compressed_data (data_size)]
///
/// The caller also passes an output VMO handle for PCM output (F32 stereo interleaved).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeAudioRequest {
    pub codec: u8,
    pub channels: u8,
    pub sample_rate: u32,
    pub config_size: u32,
    pub num_frames: u32,
    pub data_size: u32,
}

impl DecodeAudioRequest {
    pub const SIZE: usize = 20;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.codec;
        buf[1] = self.channels;
        buf[2..4].copy_from_slice(&[0; 2]);
        buf[4..8].copy_from_slice(&self.sample_rate.to_le_bytes());
        buf[8..12].copy_from_slice(&self.config_size.to_le_bytes());
        buf[12..16].copy_from_slice(&self.num_frames.to_le_bytes());
        buf[16..20].copy_from_slice(&self.data_size.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            codec: buf[0],
            channels: buf[1],
            sample_rate: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            config_size: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            num_frames: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            data_size: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeAudioReply {
    pub status: u32,
    pub pcm_bytes: u32,
}

impl DecodeAudioReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.status.to_le_bytes());
        buf[4..8].copy_from_slice(&self.pcm_bytes.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            status: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            pcm_bytes: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
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
            codec_data_offset: 0,
            codec_data_size: 64,
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
    fn decode_audio_request_round_trip() {
        let req = DecodeAudioRequest {
            codec: AUDIO_CODEC_AAC,
            channels: 2,
            sample_rate: 44100,
            config_size: 2,
            num_frames: 384,
            data_size: 98304,
        };
        let mut buf = [0u8; DecodeAudioRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = DecodeAudioRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn decode_audio_reply_round_trip() {
        let reply = DecodeAudioReply {
            status: 0,
            pcm_bytes: 1_411_200,
        };
        let mut buf = [0u8; DecodeAudioReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = DecodeAudioReply::read_from(&buf);

        assert_eq!(reply, decoded);
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
            DECODE_AUDIO,
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
        assert!(DecodeAudioRequest::SIZE <= ipc::MAX_PAYLOAD);
        assert!(DecodeAudioReply::SIZE <= ipc::MAX_PAYLOAD);
    }
}
