//! Snapshot data types, serialization, and linked-block blob I/O.
//!
//! A snapshot captures the state of one or more files at a point in time.
//! The snapshot store is serialized to a chain of linked blocks on disk.
//!
//! Linked-block format (per block):
//! ```text
//! [next_block: u32] [chunk_len: u32] [data: chunk_len bytes]
//! ```
//!
//! Serialized snapshot store format:
//! ```text
//! [snapshot_count: u32] [next_snapshot_id: u64]
//! Per snapshot:
//!   [id: u64] [txg: u64] [file_count: u32]
//!   Per file:
//!     [file_id: u64] [size: u64] [was_inline: u8]
//!     If inline:  [data_len: u32] [data bytes]
//!     If extents: [count: u16] Per extent: [start: u32] [count: u16] [birth_txg: u48]
//! ```

use std::collections::HashMap;

use crate::alloc::Allocator;
use crate::block::BlockDevice;
use crate::inode::InodeExtent;
use crate::{FsError, BLOCK_SIZE};

const BLOB_HEADER: usize = 8; // next_block (4) + chunk_len (4)
const BLOB_DATA_CAP: usize = BLOCK_SIZE as usize - BLOB_HEADER;

/// Saved state of a single file within a snapshot.
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub was_inline: bool,
    pub inline_data: Vec<u8>,
    pub extents: Vec<InodeExtent>,
    pub size: u64,
}

/// A multi-file snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: u64,
    pub txg: u64,
    pub files: HashMap<u64, FileSnapshot>,
}

// ── Serialization ──────────────────────────────────────────────────

pub fn serialize(snapshots: &HashMap<u64, Snapshot>, next_id: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    push_u32(&mut buf, snapshots.len() as u32);
    push_u64(&mut buf, next_id);

    for snap in snapshots.values() {
        push_u64(&mut buf, snap.id);
        push_u64(&mut buf, snap.txg);
        push_u32(&mut buf, snap.files.len() as u32);

        for (&file_id, fs) in &snap.files {
            push_u64(&mut buf, file_id);
            push_u64(&mut buf, fs.size);
            buf.push(if fs.was_inline { 1 } else { 0 });

            if fs.was_inline {
                push_u32(&mut buf, fs.inline_data.len() as u32);
                buf.extend_from_slice(&fs.inline_data);
            } else {
                push_u16(&mut buf, fs.extents.len() as u16);
                for ext in &fs.extents {
                    push_u32(&mut buf, ext.start_block);
                    push_u16(&mut buf, ext.count);
                    // birth_txg as 6 bytes LE
                    let bytes = ext.birth_txg.to_le_bytes();
                    buf.extend_from_slice(&bytes[..6]);
                }
            }
        }
    }

    buf
}

pub fn deserialize(data: &[u8]) -> Result<(HashMap<u64, Snapshot>, u64), FsError> {
    let mut pos = 0;

    let count = read_u32(data, &mut pos)?;
    let next_id = read_u64(data, &mut pos)?;

    let mut snapshots = HashMap::with_capacity(count as usize);

    for _ in 0..count {
        let id = read_u64(data, &mut pos)?;
        let txg = read_u64(data, &mut pos)?;
        let file_count = read_u32(data, &mut pos)?;

        let mut files = HashMap::with_capacity(file_count as usize);

        for _ in 0..file_count {
            let file_id = read_u64(data, &mut pos)?;
            let size = read_u64(data, &mut pos)?;
            let was_inline = read_u8(data, &mut pos)? != 0;

            let fs = if was_inline {
                let data_len = read_u32(data, &mut pos)? as usize;
                if pos + data_len > data.len() {
                    return Err(FsError::Corrupt("snapshot inline data truncated".into()));
                }
                let inline_data = data[pos..pos + data_len].to_vec();
                pos += data_len;
                FileSnapshot {
                    was_inline: true,
                    inline_data,
                    extents: Vec::new(),
                    size,
                }
            } else {
                let ext_count = read_u16(data, &mut pos)? as usize;
                let mut extents = Vec::with_capacity(ext_count);
                for _ in 0..ext_count {
                    let start = read_u32(data, &mut pos)?;
                    let count = read_u16(data, &mut pos)?;
                    // 6 bytes for birth_txg
                    if pos + 6 > data.len() {
                        return Err(FsError::Corrupt("snapshot extent truncated".into()));
                    }
                    let mut bytes = [0u8; 8];
                    bytes[..6].copy_from_slice(&data[pos..pos + 6]);
                    let birth_txg = u64::from_le_bytes(bytes);
                    pos += 6;
                    extents.push(InodeExtent {
                        start_block: start,
                        count,
                        birth_txg,
                    });
                }
                FileSnapshot {
                    was_inline: false,
                    inline_data: Vec::new(),
                    extents,
                    size,
                }
            };

            files.insert(file_id, fs);
        }

        snapshots.insert(id, Snapshot { id, txg, files });
    }

    Ok((snapshots, next_id))
}

// ── Linked-block blob I/O ──────────────────────────────────────────

/// Write `data` to a chain of linked blocks. Returns (first_block, all_block_nums).
pub fn write_blob<D: BlockDevice>(
    device: &mut D,
    allocator: &mut Allocator,
    data: &[u8],
) -> Result<(u32, Vec<u32>), FsError> {
    let block_count = if data.is_empty() {
        1
    } else {
        (data.len() + BLOB_DATA_CAP - 1) / BLOB_DATA_CAP
    };

    let mut blocks = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        blocks.push(allocator.alloc(1).ok_or(FsError::NoSpace)?);
    }

    let mut data_off = 0;
    let mut buf = vec![0u8; BLOCK_SIZE as usize];

    for (i, &block) in blocks.iter().enumerate() {
        buf.fill(0);
        let next = if i + 1 < blocks.len() {
            blocks[i + 1]
        } else {
            0
        };
        let chunk_len = (data.len() - data_off).min(BLOB_DATA_CAP);

        buf[0..4].copy_from_slice(&next.to_le_bytes());
        buf[4..8].copy_from_slice(&(chunk_len as u32).to_le_bytes());
        if chunk_len > 0 {
            buf[8..8 + chunk_len].copy_from_slice(&data[data_off..data_off + chunk_len]);
        }

        device.write_block(block, &buf)?;
        data_off += chunk_len;
    }

    Ok((blocks[0], blocks))
}

/// Read a linked-block chain. Returns (data, all_block_nums).
pub fn read_blob<D: BlockDevice>(
    device: &D,
    first_block: u32,
) -> Result<(Vec<u8>, Vec<u32>), FsError> {
    let mut data = Vec::new();
    let mut block_list = Vec::new();
    let mut current = first_block;
    let mut buf = vec![0u8; BLOCK_SIZE as usize];

    loop {
        device.read_block(current, &mut buf)?;
        block_list.push(current);

        let next = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let chunk_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;

        if chunk_len > BLOB_DATA_CAP {
            return Err(FsError::Corrupt("blob chunk_len exceeds block capacity".into()));
        }
        data.extend_from_slice(&buf[8..8 + chunk_len]);

        if next == 0 {
            break;
        }
        current = next;
    }

    Ok((data, block_list))
}

// ── Encoding helpers ───────────────────────────────────────────────

fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, FsError> {
    if *pos >= data.len() {
        return Err(FsError::Corrupt("unexpected end of snapshot data".into()));
    }
    let v = data[*pos];
    *pos += 1;
    Ok(v)
}

fn read_u16(data: &[u8], pos: &mut usize) -> Result<u16, FsError> {
    if *pos + 2 > data.len() {
        return Err(FsError::Corrupt("unexpected end of snapshot data".into()));
    }
    let v = u16::from_le_bytes(data[*pos..*pos + 2].try_into().unwrap());
    *pos += 2;
    Ok(v)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, FsError> {
    if *pos + 4 > data.len() {
        return Err(FsError::Corrupt("unexpected end of snapshot data".into()));
    }
    let v = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    Ok(v)
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, FsError> {
    if *pos + 8 > data.len() {
        return Err(FsError::Corrupt("unexpected end of snapshot data".into()));
    }
    let v = u64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    #[test]
    fn serialize_deserialize_empty() {
        let snaps = HashMap::new();
        let data = serialize(&snaps, 1);
        let (loaded, next_id) = deserialize(&data).unwrap();
        assert!(loaded.is_empty());
        assert_eq!(next_id, 1);
    }

    #[test]
    fn serialize_deserialize_inline_snapshot() {
        let mut files = HashMap::new();
        files.insert(
            42,
            FileSnapshot {
                was_inline: true,
                inline_data: b"hello snapshot".to_vec(),
                extents: Vec::new(),
                size: 14,
            },
        );
        let mut snaps = HashMap::new();
        snaps.insert(1, Snapshot { id: 1, txg: 5, files });

        let data = serialize(&snaps, 2);
        let (loaded, next_id) = deserialize(&data).unwrap();
        assert_eq!(next_id, 2);
        assert_eq!(loaded.len(), 1);
        let snap = &loaded[&1];
        assert_eq!(snap.txg, 5);
        let fs = &snap.files[&42];
        assert!(fs.was_inline);
        assert_eq!(fs.inline_data, b"hello snapshot");
        assert_eq!(fs.size, 14);
    }

    #[test]
    fn serialize_deserialize_extent_snapshot() {
        let mut files = HashMap::new();
        files.insert(
            7,
            FileSnapshot {
                was_inline: false,
                inline_data: Vec::new(),
                extents: vec![
                    InodeExtent {
                        start_block: 100,
                        count: 5,
                        birth_txg: 0x0000_FFFF_FFFF_FFFF,
                    },
                    InodeExtent {
                        start_block: 200,
                        count: 3,
                        birth_txg: 42,
                    },
                ],
                size: 131072,
            },
        );
        let mut snaps = HashMap::new();
        snaps.insert(1, Snapshot { id: 1, txg: 10, files });

        let data = serialize(&snaps, 2);
        let (loaded, _) = deserialize(&data).unwrap();
        let fs = &loaded[&1].files[&7];
        assert!(!fs.was_inline);
        assert_eq!(fs.extents.len(), 2);
        assert_eq!(fs.extents[0].birth_txg, 0x0000_FFFF_FFFF_FFFF);
        assert_eq!(fs.extents[1].birth_txg, 42);
        assert_eq!(fs.size, 131072);
    }

    #[test]
    fn blob_write_read_roundtrip_small() {
        let mut dev = MemoryBlockDevice::new(64);
        let mut alloc = Allocator::new(64);
        let data = b"small blob data";

        let (first, blocks) = write_blob(&mut dev, &mut alloc, data).unwrap();
        assert_eq!(blocks.len(), 1);

        let (loaded, loaded_blocks) = read_blob(&dev, first).unwrap();
        assert_eq!(loaded, data);
        assert_eq!(loaded_blocks, blocks);
    }

    #[test]
    fn blob_write_read_roundtrip_large() {
        let mut dev = MemoryBlockDevice::new(256);
        let mut alloc = Allocator::new(256);
        // Data spanning 3 blocks.
        let data = vec![0xAB; BLOB_DATA_CAP * 2 + 100];

        let (first, blocks) = write_blob(&mut dev, &mut alloc, &data).unwrap();
        assert_eq!(blocks.len(), 3);

        let (loaded, loaded_blocks) = read_blob(&dev, first).unwrap();
        assert_eq!(loaded, data);
        assert_eq!(loaded_blocks, blocks);
    }

    #[test]
    fn blob_write_read_empty() {
        let mut dev = MemoryBlockDevice::new(64);
        let mut alloc = Allocator::new(64);

        let (first, blocks) = write_blob(&mut dev, &mut alloc, &[]).unwrap();
        assert_eq!(blocks.len(), 1);

        let (loaded, _) = read_blob(&dev, first).unwrap();
        assert!(loaded.is_empty());
    }
}
