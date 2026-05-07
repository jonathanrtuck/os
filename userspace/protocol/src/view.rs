//! View protocol — notifications from document service to presenter.
//!
//! Transport: sync call/reply (document → presenter) for `DocLoaded`
//! and `ImageDecoded`. `DocChanged` uses a pure event signal with no
//! payload — "re-read the shared buffer."

pub const DOC_CHANGED: u32 = 1;
pub const DOC_LOADED: u32 = 2;
pub const IMAGE_DECODED: u32 = 3;

/// Sent when a document is first loaded and ready for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocLoaded {
    pub doc_id: u64,
    pub content_len: u64,
    pub content_type: [u8; 32],
}

impl DocLoaded {
    pub const SIZE: usize = 48;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.doc_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.content_len.to_le_bytes());
        buf[16..48].copy_from_slice(&self.content_type);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        let mut content_type = [0u8; 32];

        content_type.copy_from_slice(&buf[16..48]);

        Self {
            doc_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            content_len: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            content_type,
        }
    }

    #[must_use]
    pub fn content_type_str(&self) -> &[u8] {
        &self.content_type[..crate::null_terminated_len(&self.content_type)]
    }
}

/// Sent after an image has been decoded and the pixel buffer is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDecoded {
    pub request_id: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

impl ImageDecoded {
    pub const SIZE: usize = 24;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.request_id.to_le_bytes());
        buf[8..12].copy_from_slice(&self.width.to_le_bytes());
        buf[12..16].copy_from_slice(&self.height.to_le_bytes());
        buf[16..20].copy_from_slice(&self.stride.to_le_bytes());
        buf[20..24].copy_from_slice(&self.format.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            request_id: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            width: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            height: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            stride: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            format: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_loaded_round_trip() {
        let mut ct = [0u8; 32];

        ct[..10].copy_from_slice(b"text/plain");

        let msg = DocLoaded {
            doc_id: 1,
            content_len: 4096,
            content_type: ct,
        };
        let mut buf = [0u8; DocLoaded::SIZE];

        msg.write_to(&mut buf);

        let decoded = DocLoaded::read_from(&buf);

        assert_eq!(msg, decoded);
    }

    #[test]
    fn doc_loaded_content_type_str() {
        let mut ct = [0u8; 32];

        ct[..10].copy_from_slice(b"text/plain");

        let msg = DocLoaded {
            doc_id: 0,
            content_len: 0,
            content_type: ct,
        };

        assert_eq!(msg.content_type_str(), b"text/plain");
    }

    #[test]
    fn doc_loaded_full_content_type() {
        let ct = [b'x'; 32];
        let msg = DocLoaded {
            doc_id: 0,
            content_len: 0,
            content_type: ct,
        };

        assert_eq!(msg.content_type_str().len(), 32);
    }

    #[test]
    fn image_decoded_round_trip() {
        let msg = ImageDecoded {
            request_id: 42,
            width: 1920,
            height: 1080,
            stride: 7680,
            format: 0,
        };
        let mut buf = [0u8; ImageDecoded::SIZE];

        msg.write_to(&mut buf);

        let decoded = ImageDecoded::read_from(&buf);

        assert_eq!(msg, decoded);
    }

    #[test]
    fn image_decoded_zero_dimensions() {
        let msg = ImageDecoded {
            request_id: 0,
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
        };
        let mut buf = [0u8; ImageDecoded::SIZE];

        msg.write_to(&mut buf);

        assert_eq!(buf, [0u8; ImageDecoded::SIZE]);
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(DocLoaded::SIZE <= crate::MAX_PAYLOAD);
        assert!(ImageDecoded::SIZE <= crate::MAX_PAYLOAD);
    }

    #[test]
    fn method_ids_distinct() {
        let methods = [DOC_CHANGED, DOC_LOADED, IMAGE_DECODED];

        for i in 0..methods.len() {
            for j in (i + 1)..methods.len() {
                assert_ne!(methods[i], methods[j]);
            }
        }
    }
}
