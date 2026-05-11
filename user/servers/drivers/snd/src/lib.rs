#![no_std]

//! Sound driver protocol — PCM audio playback via virtio-snd.
//!
//! Transport: sync call/reply (audio mixer → snd driver).
//!
//! Data transfer uses a shared VMO established via SETUP. The mixer
//! writes F32 stereo 48 kHz PCM into the shared VMO, then calls WRITE
//! with the byte count. The driver converts to S16LE and submits to
//! the virtio TX queue.

pub const SETUP: u32 = 1;
pub const WRITE: u32 = 2;
pub const GET_INFO: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRequest {
    pub offset: u32,
    pub len: u32,
}

impl WriteRequest {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.offset.to_le_bytes());
        buf[4..8].copy_from_slice(&self.len.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            offset: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            len: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoReply {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

impl InfoReply {
    pub const SIZE: usize = 8;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.sample_rate.to_le_bytes());
        buf[4..6].copy_from_slice(&self.channels.to_le_bytes());
        buf[6..8].copy_from_slice(&self.bits_per_sample.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            sample_rate: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            channels: u16::from_le_bytes(buf[4..6].try_into().unwrap()),
            bits_per_sample: u16::from_le_bytes(buf[6..8].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_request_round_trip() {
        let req = WriteRequest {
            offset: 0,
            len: 4096,
        };
        let mut buf = [0u8; WriteRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = WriteRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn info_reply_round_trip() {
        let reply = InfoReply {
            sample_rate: 48000,
            channels: 2,
            bits_per_sample: 16,
        };
        let mut buf = [0u8; InfoReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = InfoReply::read_from(&buf);

        assert_eq!(reply, decoded);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [SETUP, WRITE, GET_INFO];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }
}
