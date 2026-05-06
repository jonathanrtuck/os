//! Message protocol — structured header over the 128-byte IPC buffer.
//!
//! Layout: `[method: u32][status: u16][len: u16][payload: 0..120 bytes]`

pub const MSG_SIZE: usize = 128;
pub const HEADER_SIZE: usize = 8;
pub const MAX_PAYLOAD: usize = MSG_SIZE - HEADER_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Header {
    pub method: u32,
    pub status: u16,
    pub len: u16,
}

impl Header {
    pub const fn request(method: u32, len: u16) -> Self {
        Self {
            method,
            status: 0,
            len,
        }
    }

    pub const fn ok(method: u32, len: u16) -> Self {
        Self {
            method,
            status: 0,
            len,
        }
    }

    pub const fn error(method: u32, status: u16) -> Self {
        Self {
            method,
            status,
            len: 0,
        }
    }

    pub fn write_to(&self, buf: &mut [u8; MSG_SIZE]) {
        buf[0..4].copy_from_slice(&self.method.to_le_bytes());
        buf[4..6].copy_from_slice(&self.status.to_le_bytes());
        buf[6..8].copy_from_slice(&self.len.to_le_bytes());
    }

    pub fn read_from(buf: &[u8; MSG_SIZE]) -> Self {
        Self {
            method: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            status: u16::from_le_bytes([buf[4], buf[5]]),
            len: u16::from_le_bytes([buf[6], buf[7]]),
        }
    }

    pub fn is_error(&self) -> bool {
        self.status != 0
    }
}

pub fn payload(buf: &[u8; MSG_SIZE]) -> &[u8] {
    let header = Header::read_from(buf);
    let end = HEADER_SIZE + (header.len as usize).min(MAX_PAYLOAD);

    &buf[HEADER_SIZE..end]
}

/// The full writable payload region (all MAX_PAYLOAD bytes). The caller
/// is responsible for setting `len` in the header to match what they write.
pub fn payload_region_mut(buf: &mut [u8; MSG_SIZE]) -> &mut [u8] {
    &mut buf[HEADER_SIZE..]
}

pub fn write_request(buf: &mut [u8; MSG_SIZE], method: u32, data: &[u8]) -> usize {
    let len = data.len().min(MAX_PAYLOAD);

    Header::request(method, len as u16).write_to(buf);

    buf[HEADER_SIZE..HEADER_SIZE + len].copy_from_slice(&data[..len]);

    HEADER_SIZE + len
}

pub fn write_reply(buf: &mut [u8; MSG_SIZE], method: u32, data: &[u8]) -> usize {
    let len = data.len().min(MAX_PAYLOAD);

    Header::ok(method, len as u16).write_to(buf);

    buf[HEADER_SIZE..HEADER_SIZE + len].copy_from_slice(&data[..len]);

    HEADER_SIZE + len
}

pub fn write_error(buf: &mut [u8; MSG_SIZE], method: u32, status: u16) -> usize {
    Header::error(method, status).write_to(buf);

    HEADER_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let mut buf = [0u8; MSG_SIZE];
        let header = Header::request(42, 10);

        header.write_to(&mut buf);

        let read = Header::read_from(&buf);

        assert_eq!(read, header);
    }

    #[test]
    fn error_header() {
        let h = Header::error(7, 3);

        assert!(h.is_error());
        assert_eq!(h.status, 3);
        assert_eq!(h.len, 0);
    }

    #[test]
    fn ok_header_is_not_error() {
        let h = Header::ok(1, 0);

        assert!(!h.is_error());
    }

    #[test]
    fn write_request_copies_payload() {
        let mut buf = [0u8; MSG_SIZE];
        let data = [1, 2, 3, 4, 5];
        let total = write_request(&mut buf, 99, &data);

        assert_eq!(total, HEADER_SIZE + 5);

        let header = Header::read_from(&buf);

        assert_eq!(header.method, 99);
        assert_eq!(header.len, 5);
        assert_eq!(payload(&buf), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn write_request_truncates_oversized_payload() {
        let mut buf = [0u8; MSG_SIZE];
        let data = [0xAA; MSG_SIZE]; // bigger than MAX_PAYLOAD
        let total = write_request(&mut buf, 1, &data);

        assert_eq!(total, MSG_SIZE);

        let header = Header::read_from(&buf);

        assert_eq!(header.len as usize, MAX_PAYLOAD);
    }

    #[test]
    fn write_reply_round_trip() {
        let mut buf = [0u8; MSG_SIZE];
        let data = b"hello";

        write_reply(&mut buf, 10, data);

        let header = Header::read_from(&buf);

        assert_eq!(header.method, 10);
        assert!(!header.is_error());
        assert_eq!(payload(&buf), b"hello");
    }

    #[test]
    fn write_error_no_payload() {
        let mut buf = [0u8; MSG_SIZE];
        let total = write_error(&mut buf, 5, 42);

        assert_eq!(total, HEADER_SIZE);

        let header = Header::read_from(&buf);

        assert!(header.is_error());
        assert_eq!(header.status, 42);
        assert_eq!(payload(&buf).len(), 0);
    }

    #[test]
    fn empty_payload() {
        let mut buf = [0u8; MSG_SIZE];

        write_request(&mut buf, 1, &[]);
        assert_eq!(payload(&buf).len(), 0);
    }
}
