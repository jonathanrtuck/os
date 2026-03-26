//! Binary catalog serialization.
//!
//! Format:
//! ```text
//! [magic: u32] [entry_count: u32]
//! per entry:
//!   [file_id: u64] [media_type_len: u16] [media_type bytes]
//!   [attr_count: u16] per attr: [key_len: u16] [key bytes] [val_len: u16] [val bytes]
//! ```

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

use crate::{CatalogEntry, StoreError};

/// Encode the catalog to binary.
pub fn encode_catalog(magic: u32, catalog: &BTreeMap<u64, CatalogEntry>) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&magic.to_le_bytes());
    buf.extend_from_slice(&(catalog.len() as u32).to_le_bytes());

    for (&file_id, entry) in catalog {
        buf.extend_from_slice(&file_id.to_le_bytes());

        let mt = entry.media_type.as_bytes();
        buf.extend_from_slice(&(mt.len() as u16).to_le_bytes());
        buf.extend_from_slice(mt);

        buf.extend_from_slice(&(entry.attributes.len() as u16).to_le_bytes());
        for (key, val) in &entry.attributes {
            let kb = key.as_bytes();
            let vb = val.as_bytes();
            buf.extend_from_slice(&(kb.len() as u16).to_le_bytes());
            buf.extend_from_slice(kb);
            buf.extend_from_slice(&(vb.len() as u16).to_le_bytes());
            buf.extend_from_slice(vb);
        }
    }

    buf
}

/// Decode binary catalog data.
pub fn decode_catalog(magic: u32, data: &[u8]) -> Result<BTreeMap<u64, CatalogEntry>, StoreError> {
    let mut pos = 0;

    let read_u16 = |pos: &mut usize, data: &[u8]| -> Result<u16, StoreError> {
        if *pos + 2 > data.len() {
            return Err(StoreError::Corrupt("truncated u16".into()));
        }
        let val = u16::from_le_bytes([data[*pos], data[*pos + 1]]);
        *pos += 2;
        Ok(val)
    };

    let read_u32 = |pos: &mut usize, data: &[u8]| -> Result<u32, StoreError> {
        if *pos + 4 > data.len() {
            return Err(StoreError::Corrupt("truncated u32".into()));
        }
        let val = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
        *pos += 4;
        Ok(val)
    };

    let read_u64 = |pos: &mut usize, data: &[u8]| -> Result<u64, StoreError> {
        if *pos + 8 > data.len() {
            return Err(StoreError::Corrupt("truncated u64".into()));
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[*pos..*pos + 8]);
        let val = u64::from_le_bytes(bytes);
        *pos += 8;
        Ok(val)
    };

    let read_string = |pos: &mut usize, data: &[u8]| -> Result<String, StoreError> {
        let len = read_u16(pos, data)? as usize;
        if *pos + len > data.len() {
            return Err(StoreError::Corrupt("truncated string".into()));
        }
        let s = core::str::from_utf8(&data[*pos..*pos + len])
            .map_err(|_| StoreError::Corrupt("invalid UTF-8".into()))?;
        *pos += len;
        Ok(s.to_string())
    };

    let stored_magic = read_u32(&mut pos, data)?;
    if stored_magic != magic {
        return Err(StoreError::Corrupt("bad catalog magic".into()));
    }

    let entry_count = read_u32(&mut pos, data)? as usize;
    let mut catalog = BTreeMap::new();

    for _ in 0..entry_count {
        let file_id = read_u64(&mut pos, data)?;
        let media_type = read_string(&mut pos, data)?;
        let attr_count = read_u16(&mut pos, data)? as usize;
        let mut attributes = BTreeMap::new();
        for _ in 0..attr_count {
            let key = read_string(&mut pos, data)?;
            let val = read_string(&mut pos, data)?;
            attributes.insert(key, val);
        }
        catalog.insert(
            file_id,
            CatalogEntry {
                media_type,
                attributes,
            },
        );
    }

    Ok(catalog)
}
