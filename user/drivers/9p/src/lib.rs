//! virtio-9p driver protocol — host filesystem access via 9P2000.L.
//!
//! Transport: sync call/reply (fs service → 9p driver).
//!
//! Data transfer uses a shared VMO established once via SETUP. The driver
//! reads host files via 9P2000.L over virtio and writes file data into the
//! shared VMO. Paths are null-padded ASCII within the IPC payload.

#![no_std]

pub use ipc::MAX_PAYLOAD;

pub const SETUP: u32 = 1;
pub const READ_FILE: u32 = 2;
pub const STAT: u32 = 3;

pub const MAX_PATH: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFileRequest {
    pub vmo_offset: u32,
    pub max_len: u32,
    pub path: [u8; MAX_PATH],
}

impl ReadFileRequest {
    pub const SIZE: usize = 8 + MAX_PATH;

    pub fn new(path: &[u8], vmo_offset: u32, max_len: u32) -> Self {
        let mut p = [0u8; MAX_PATH];
        let len = path.len().min(MAX_PATH);

        p[..len].copy_from_slice(&path[..len]);

        Self {
            vmo_offset,
            max_len,
            path: p,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.vmo_offset.to_le_bytes());
        buf[4..8].copy_from_slice(&self.max_len.to_le_bytes());
        buf[8..8 + MAX_PATH].copy_from_slice(&self.path);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        let mut path = [0u8; MAX_PATH];

        path.copy_from_slice(&buf[8..8 + MAX_PATH]);

        Self {
            vmo_offset: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            max_len: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            path,
        }
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
        let req = ReadFileRequest::new(b"test.txt", 0, 4096);
        let mut buf = [0u8; ReadFileRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = ReadFileRequest::read_from(&buf);

        assert_eq!(decoded, req);
        assert_eq!(decoded.path_bytes(), b"test.txt");
    }

    #[test]
    fn read_file_reply_round_trip() {
        let reply = ReadFileReply { bytes_read: 1234 };
        let mut buf = [0u8; ReadFileReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(ReadFileReply::read_from(&buf), reply);
    }

    #[test]
    fn stat_request_round_trip() {
        let req = StatRequest::new(b"image.png");
        let mut buf = [0u8; StatRequest::SIZE];

        req.write_to(&mut buf);

        let decoded = StatRequest::read_from(&buf);

        assert_eq!(decoded, req);
        assert_eq!(decoded.path_bytes(), b"image.png");
    }

    #[test]
    fn stat_reply_round_trip() {
        let reply = StatReply {
            size: 65536,
            exists: 1,
        };
        let mut buf = [0u8; StatReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(StatReply::read_from(&buf), reply);
    }

    #[test]
    fn empty_path() {
        let req = ReadFileRequest::new(b"", 0, 100);

        assert_eq!(req.path_bytes(), b"");
    }

    #[test]
    fn max_path_length() {
        let name = [b'x'; MAX_PATH];
        let req = ReadFileRequest::new(&name, 0, 100);

        assert_eq!(req.path_bytes().len(), MAX_PATH);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [SETUP, READ_FILE, STAT];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }

    #[test]
    fn sizes_fit_payload() {
        assert!(ReadFileReply::SIZE <= MAX_PAYLOAD);
        assert!(StatReply::SIZE <= MAX_PAYLOAD);
    }
}
