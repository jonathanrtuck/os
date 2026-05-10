#![no_std]

//! RNG protocol — random byte delivery from the virtio-rng driver.
//!
//! Transport: sync call/reply.
//!
//! Random bytes are returned inline in the reply payload (up to 120
//! bytes per call). No shared VMO needed.

pub const FILL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillRequest {
    pub size: u32,
}

impl FillRequest {
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
    fn fill_request_round_trip() {
        let req = FillRequest { size: 64 };
        let mut buf = [0u8; FillRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = FillRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn fill_request_zero() {
        let req = FillRequest { size: 0 };
        let mut buf = [0u8; FillRequest::SIZE];

        req.write_to(&mut buf);

        assert_eq!(buf, [0; 4]);
    }

    #[test]
    fn fill_request_max() {
        let req = FillRequest { size: 120 };
        let mut buf = [0u8; FillRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = FillRequest::read_from(&buf);

        assert_eq!(decoded.size, 120);
    }

    #[test]
    fn method_id_nonzero() {
        assert_ne!(FILL, 0);
    }
}
