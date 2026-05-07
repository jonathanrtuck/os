//! Name service protocol — flat namespace for service discovery.
//!
//! Transport: sync call/reply.
//!
//! All three operations carry a 32-byte null-padded ASCII name as
//! payload. Handle transfer uses IPC handle slots (out-of-band).
//!
//! ```text
//! Register(name)   + handle[0] = endpoint  → Ok | AlreadyExists
//! Lookup(name)                             → Ok + handle[0] = endpoint | NotFound
//! Unregister(name)                         → Ok | NotFound
//! ```

pub const REGISTER: u32 = 1;
pub const LOOKUP: u32 = 2;
pub const UNREGISTER: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRequest {
    pub name: [u8; 32],
}

impl NameRequest {
    pub const SIZE: usize = 32;

    #[must_use]
    pub fn new(name: &[u8]) -> Self {
        let mut buf = [0u8; 32];
        let len = name.len().min(32);

        buf[..len].copy_from_slice(&name[..len]);

        Self { name: buf }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[..32].copy_from_slice(&self.name);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        let mut name = [0u8; 32];

        name.copy_from_slice(&buf[..32]);

        Self { name }
    }

    #[must_use]
    pub fn as_str(&self) -> &[u8] {
        &self.name[..crate::null_terminated_len(&self.name)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let req = NameRequest::new(b"document");
        let mut buf = [0u8; 32];

        req.write_to(&mut buf);

        let decoded = NameRequest::read_from(&buf);

        assert_eq!(req, decoded);
    }

    #[test]
    fn name_null_padded() {
        let req = NameRequest::new(b"blk");

        assert_eq!(req.as_str(), b"blk");
        assert_eq!(req.name[3], 0);
        assert_eq!(req.name[31], 0);
    }

    #[test]
    fn max_length_name() {
        let name = [b'x'; 32];
        let req = NameRequest::new(&name);

        assert_eq!(req.as_str(), &name[..]);
    }

    #[test]
    fn empty_name() {
        let req = NameRequest::new(b"");

        assert_eq!(req.as_str(), b"");
        assert_eq!(req.name, [0u8; 32]);
    }

    #[test]
    fn truncates_oversized_name() {
        let name = [b'A'; 64];
        let req = NameRequest::new(&name);

        assert_eq!(req.as_str().len(), 32);
    }

    #[test]
    fn size_fits_payload() {
        assert!(NameRequest::SIZE <= crate::MAX_PAYLOAD);
    }
}
