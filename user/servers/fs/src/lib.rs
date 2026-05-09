//! Filesystem service protocol — unified file access over store and 9p.
//!
//! Transport: sync call/reply with VMO handle transfer.
//!
//! ```text
//! ReadFile(path)  → Ok(bytes_read) + handle[0] = data_vmo
//!                 | Error(status)
//!
//! Stat(path)      → Ok(size, exists)
//!                 | Error(status)
//! ```

#![no_std]

pub use ipc::MAX_PAYLOAD;

pub const READ_FILE: u32 = 1;
pub const STAT: u32 = 2;

pub const MAX_PATH: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFileRequest {
    pub path: [u8; MAX_PATH],
}

impl ReadFileRequest {
    pub const SIZE: usize = MAX_PATH;

    pub fn new(path: &[u8]) -> Self {
        let mut p = [0u8; MAX_PATH];
        let len = path.len().min(MAX_PATH);

        p[..len].copy_from_slice(&path[..len]);

        Self { path: p }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[..MAX_PATH].copy_from_slice(&self.path);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        let mut path = [0u8; MAX_PATH];

        path.copy_from_slice(&buf[..MAX_PATH]);

        Self { path }
    }

    pub fn path_bytes(&self) -> &[u8] {
        &self.path[..self.path.iter().position(|&b| b == 0).unwrap_or(MAX_PATH)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFileReply {
    pub bytes_read: u32,
}

impl ReadFileReply {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.bytes_read.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            bytes_read: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatRequest {
    pub path: [u8; MAX_PATH],
}

impl StatRequest {
    pub const SIZE: usize = MAX_PATH;

    pub fn new(path: &[u8]) -> Self {
        let mut p = [0u8; MAX_PATH];
        let len = path.len().min(MAX_PATH);

        p[..len].copy_from_slice(&path[..len]);

        Self { path: p }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[..MAX_PATH].copy_from_slice(&self.path);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        let mut path = [0u8; MAX_PATH];

        path.copy_from_slice(&buf[..MAX_PATH]);

        Self { path }
    }

    pub fn path_bytes(&self) -> &[u8] {
        &self.path[..self.path.iter().position(|&b| b == 0).unwrap_or(MAX_PATH)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatReply {
    pub size: u64,
    pub exists: u8,
}

impl StatReply {
    pub const SIZE: usize = 16;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.size.to_le_bytes());
        buf[8] = self.exists;
        buf[9..16].copy_from_slice(&[0; 7]);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            size: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            exists: buf[8],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_request_round_trip() {
        let req = ReadFileRequest::new(b"hello.txt");
        let mut buf = [0u8; ReadFileRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = ReadFileRequest::read_from(&buf);

        assert_eq!(decoded, req);
        assert_eq!(decoded.path_bytes(), b"hello.txt");
    }

    #[test]
    fn read_file_reply_round_trip() {
        let reply = ReadFileReply { bytes_read: 42 };
        let mut buf = [0u8; ReadFileReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(ReadFileReply::read_from(&buf), reply);
    }

    #[test]
    fn stat_request_round_trip() {
        let req = StatRequest::new(b"doc.txt");
        let mut buf = [0u8; StatRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = StatRequest::read_from(&buf);

        assert_eq!(decoded, req);
        assert_eq!(decoded.path_bytes(), b"doc.txt");
    }

    #[test]
    fn stat_reply_round_trip() {
        let reply = StatReply {
            size: 1024,
            exists: 1,
        };
        let mut buf = [0u8; StatReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(StatReply::read_from(&buf), reply);
    }

    #[test]
    fn empty_path() {
        let req = ReadFileRequest::new(b"");

        assert_eq!(req.path_bytes(), b"");
    }

    #[test]
    fn method_ids_distinct() {
        assert_ne!(READ_FILE, STAT);
    }

    #[test]
    fn sizes_fit_payload() {
        assert!(ReadFileRequest::SIZE <= MAX_PAYLOAD);
        assert!(ReadFileReply::SIZE <= MAX_PAYLOAD);
        assert!(StatRequest::SIZE <= MAX_PAYLOAD);
        assert!(StatReply::SIZE <= MAX_PAYLOAD);
    }
}
