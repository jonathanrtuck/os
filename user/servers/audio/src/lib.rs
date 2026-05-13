#![no_std]

//! Audio service protocol — one-shot PCM playback.
//!
//! Transport: sync call/reply.
//!
//! Clients send PLAY with a VMO containing F32 stereo 48 kHz PCM data.
//! The audio service decodes (if WAV) or passes through (if raw F32),
//! and forwards to the snd driver for playback.

pub const PLAY: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayRequest {
    pub format: u32,
    pub data_len: u32,
    pub data_offset: u32,
}

impl PlayRequest {
    pub const SIZE: usize = 12;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.format.to_le_bytes());
        buf[4..8].copy_from_slice(&self.data_len.to_le_bytes());
        buf[8..12].copy_from_slice(&self.data_offset.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            format: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            data_len: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            data_offset: if buf.len() >= 12 {
                u32::from_le_bytes(buf[8..12].try_into().unwrap())
            } else {
                0
            },
        }
    }
}

pub const FORMAT_F32_STEREO_48K: u32 = 0;
pub const FORMAT_WAV: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_request_round_trip() {
        let req = PlayRequest {
            format: FORMAT_WAV,
            data_len: 19200,
            data_offset: 0,
        };
        let mut buf = [0u8; PlayRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = PlayRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn format_constants_distinct() {
        assert_ne!(FORMAT_F32_STEREO_48K, FORMAT_WAV);
    }
}
