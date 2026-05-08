//! Name service protocol and client — flat namespace for service discovery.
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

#![no_std]

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
        &self.name[..self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len())]
    }
}

// ---------------------------------------------------------------------------
// Client stubs
// ---------------------------------------------------------------------------

/// Look up a service by name and receive its endpoint handle.
pub fn lookup(
    ns_ep: abi::types::Handle,
    name: &[u8],
) -> Result<abi::types::Handle, abi::types::SyscallError> {
    let req = NameRequest::new(name);
    let mut buf = [0u8; ipc::message::MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, LOOKUP, &req.name);
    let mut recv_handles = [0u32; 4];
    let result = abi::ipc::call(ns_ep, &mut buf, total, &[], &mut recv_handles)?;

    if result.handle_count == 0 {
        return Err(abi::types::SyscallError::NotFound);
    }

    Ok(abi::types::Handle(recv_handles[0]))
}

/// Look up a service by name, retrying until it appears or the attempt
/// limit is reached. Between retries, spins briefly to avoid monopolizing
/// the name service endpoint via direct switch.
pub fn lookup_wait(
    ns_ep: abi::types::Handle,
    name: &[u8],
    max_attempts: u32,
) -> Result<abi::types::Handle, abi::types::SyscallError> {
    for _ in 0..max_attempts {
        match lookup(ns_ep, name) {
            Ok(h) => return Ok(h),
            Err(_) => {
                for _ in 0..100_000 {
                    core::hint::spin_loop();
                }
            }
        }
    }

    Err(abi::types::SyscallError::NotFound)
}

/// Register a service endpoint under the given name.
pub fn register(ns_ep: abi::types::Handle, service_name: &[u8], own_ep: abi::types::Handle) {
    let dup = match abi::handle::dup(own_ep, abi::types::Rights::ALL) {
        Ok(h) => h,
        Err(_) => return,
    };
    let req = NameRequest::new(service_name);
    let mut buf = [0u8; ipc::message::MSG_SIZE];
    let total = ipc::message::write_request(&mut buf, REGISTER, &req.name);
    let _ = abi::ipc::call(ns_ep, &mut buf, total, &[dup.0], &mut []);
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
        assert!(NameRequest::SIZE <= ipc::MAX_PAYLOAD);
    }
}
