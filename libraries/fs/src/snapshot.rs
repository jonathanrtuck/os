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

use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};

use crate::{alloc_mod::Allocator, block::BlockDevice, inode::InodeExtent, FsError, BLOCK_SIZE};

pub(crate) const BLOB_HEADER: usize = 8; // next_block (4) + chunk_len (4)
pub(crate) const BLOB_DATA_CAP: usize = BLOCK_SIZE as usize - BLOB_HEADER;

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
    pub files: BTreeMap<u64, FileSnapshot>,
}

// ── Serialization ──────────────────────────────────────────────────

pub fn serialize(snapshots: &BTreeMap<u64, Snapshot>, next_id: u64) -> Vec<u8> {
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

pub fn deserialize(data: &[u8]) -> Result<(BTreeMap<u64, Snapshot>, u64), FsError> {
    let mut pos = 0;
    let count = read_u32(data, &mut pos)?;
    let next_id = read_u64(data, &mut pos)?;
    let mut snapshots = BTreeMap::new();

    for _ in 0..count {
        let id = read_u64(data, &mut pos)?;
        let txg = read_u64(data, &mut pos)?;
        let file_count = read_u32(data, &mut pos)?;
        let mut files = BTreeMap::new();

        for _ in 0..file_count {
            let file_id = read_u64(data, &mut pos)?;
            let size = read_u64(data, &mut pos)?;
            let was_inline = read_u8(data, &mut pos)? != 0;
            let fs = if was_inline {
                let data_len = read_u32(data, &mut pos)? as usize;

                if pos + data_len > data.len() {
                    return Err(FsError::Corrupt(String::from(
                        "snapshot inline data truncated",
                    )));
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
                        return Err(FsError::Corrupt(String::from("snapshot extent truncated")));
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
    let max_blocks = device.block_count();

    loop {
        if block_list.len() as u32 >= max_blocks {
            return Err(FsError::Corrupt(String::from(
                "blob chain exceeds device block count (cycle?)",
            )));
        }

        device.read_block(current, &mut buf)?;
        block_list.push(current);

        let next = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let chunk_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;

        if chunk_len > BLOB_DATA_CAP {
            return Err(FsError::Corrupt(String::from(
                "blob chunk_len exceeds block capacity",
            )));
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
        return Err(FsError::Corrupt(String::from(
            "unexpected end of snapshot data",
        )));
    }

    let v = data[*pos];

    *pos += 1;

    Ok(v)
}

fn read_u16(data: &[u8], pos: &mut usize) -> Result<u16, FsError> {
    if *pos + 2 > data.len() {
        return Err(FsError::Corrupt(String::from(
            "unexpected end of snapshot data",
        )));
    }

    let v = u16::from_le_bytes(data[*pos..*pos + 2].try_into().unwrap());

    *pos += 2;

    Ok(v)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, FsError> {
    if *pos + 4 > data.len() {
        return Err(FsError::Corrupt(String::from(
            "unexpected end of snapshot data",
        )));
    }

    let v = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());

    *pos += 4;

    Ok(v)
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, FsError> {
    if *pos + 8 > data.len() {
        return Err(FsError::Corrupt(String::from(
            "unexpected end of snapshot data",
        )));
    }

    let v = u64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());

    *pos += 8;

    Ok(v)
}
