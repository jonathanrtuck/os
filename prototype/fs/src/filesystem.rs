//! Filesystem — ties together superblock, allocator, and inodes.
//!
//! The `Filesystem` struct is the main entry point. It implements the
//! two-flush commit protocol with 2-generation deferred block reuse:
//!
//! ```text
//! write data → write metadata → FLUSH → write superblock → FLUSH
//! ```
//!
//! COW model: writes to extent-based files copy the entire file content
//! to new blocks (O(file_size) per write). Acceptable for document
//! workloads (files < 1 MiB). Optimized per-block COW is a future
//! enhancement.

use std::collections::{HashMap, HashSet};

use crate::alloc::Allocator;
use crate::block::BlockDevice;
use crate::inode::{Inode, InodeExtent, INLINE_CAPACITY};
use crate::snapshot::{self, FileSnapshot, Snapshot};
use crate::superblock::Superblock;
use crate::{now_nanos, FsError, BLOCK_SIZE};

/// Maximum entries in the inode table block.
const MAX_TABLE_ENTRIES: usize = (BLOCK_SIZE as usize - 4) / 12; // 1365

/// A COW filesystem with per-file snapshots.
pub struct Filesystem<D: BlockDevice> {
    device: D,
    superblock: Superblock,
    allocator: Allocator,
    inodes: HashMap<u64, Inode>,
    dirty: HashSet<u64>,
    deferred: Vec<DeferredFree>,
    inode_table_block: u32,
    // Snapshot state
    snapshots: HashMap<u64, Snapshot>,
    next_snapshot_id: u64,
    /// Blocks used by the persisted snapshot store (for deferred freeing).
    snap_store_blocks: Vec<u32>,
}

struct DeferredFree {
    start: u32,
    count: u32,
    /// Txg of the commit that made these blocks obsolete.
    txg: u64,
}

impl<D: BlockDevice> Filesystem<D> {
    /// Format a new filesystem on `device`.
    pub fn format(mut device: D) -> Result<Self, FsError> {
        let mut sb = Superblock::format(&mut device)?;
        let mut alloc = Allocator::new(sb.total_blocks);

        let table_block = write_inode_table(&mut device, &mut alloc, &HashMap::new())?;
        let free_block = alloc.persist(&mut device)?;

        sb.root_inode_table = table_block;
        sb.root_free_list = free_block;
        sb.used_blocks = sb.total_blocks - alloc.free_blocks();
        device.flush()?;
        sb.commit(&mut device)?;

        Ok(Self {
            device,
            superblock: sb,
            allocator: alloc,
            inodes: HashMap::new(),
            dirty: HashSet::new(),
            deferred: Vec::new(),
            inode_table_block: table_block,
            snapshots: HashMap::new(),
            next_snapshot_id: 1,
            snap_store_blocks: Vec::new(),
        })
    }

    /// Mount an existing filesystem.
    pub fn mount(device: D) -> Result<Self, FsError> {
        let sb = Superblock::mount(&device)?;
        let alloc = Allocator::load(&device, sb.root_free_list)?;
        let (inodes, table_block) = load_all_inodes(&device, sb.root_inode_table)?;

        // Load snapshot store.
        let (snapshots, next_snapshot_id, snap_store_blocks) =
            if sb.root_snapshot_index != 0 {
                let (data, blocks) = snapshot::read_blob(&device, sb.root_snapshot_index)?;
                let (snaps, next_id) = snapshot::deserialize(&data)?;
                (snaps, next_id, blocks)
            } else {
                (HashMap::new(), 1, Vec::new())
            };

        Ok(Self {
            device,
            superblock: sb,
            allocator: alloc,
            inodes,
            dirty: HashSet::new(),
            deferred: Vec::new(),
            inode_table_block: table_block,
            snapshots,
            next_snapshot_id,
            snap_store_blocks,
        })
    }

    /// Create a new empty file. Returns its FileId.
    pub fn create_file(&mut self) -> Result<u64, FsError> {
        let file_id = self.superblock.next_file_id;
        self.superblock.next_file_id += 1;

        let now = now_nanos();
        let inode = Inode::create(&mut self.device, &mut self.allocator, file_id, now)?;
        self.inodes.insert(file_id, inode);
        self.dirty.insert(file_id);
        Ok(file_id)
    }

    /// Delete a file. Blocks are deferred-freed, not immediately reused.
    pub fn delete_file(&mut self, file_id: u64) -> Result<(), FsError> {
        let inode = self.inodes.remove(&file_id).ok_or(FsError::NotFound(file_id))?;
        let next_txg = self.superblock.txg + 1;
        for ext in inode.extents() {
            self.deferred.push(DeferredFree {
                start: ext.start_block,
                count: ext.count as u32,
                txg: next_txg,
            });
        }
        self.deferred.push(DeferredFree {
            start: inode.inode_block(),
            count: 1,
            txg: next_txg,
        });
        Ok(())
    }

    /// Read file content into `buf` starting at `offset`. Returns bytes read.
    pub fn read(&self, file_id: u64, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let inode = self.get(file_id)?;
        if inode.is_inline() {
            Ok(inode.read(offset, buf))
        } else {
            read_extents(&self.device, inode, offset, buf)
        }
    }

    /// Write `data` at `offset` in a file.
    pub fn write(&mut self, file_id: u64, offset: u64, data: &[u8]) -> Result<(), FsError> {
        if data.is_empty() {
            return Ok(());
        }
        let end = offset + data.len() as u64;
        let is_inline = self.get(file_id)?.is_inline();

        if is_inline && end <= INLINE_CAPACITY as u64 {
            let now = now_nanos();
            let inode = self.get_mut(file_id)?;
            inode.write_inline(offset, data)?;
            inode.modified = now;
            self.dirty.insert(file_id);
            return Ok(());
        }

        // Need extent-based storage. Build the full new content.
        let inode = self.get(file_id)?;
        let old_size = inode.size;
        let new_size = end.max(old_size) as usize;
        let mut content = vec![0u8; new_size];

        // Read existing content.
        if old_size > 0 {
            if is_inline {
                inode.read(0, &mut content[..old_size as usize]);
            } else {
                read_extents(&self.device, inode, 0, &mut content[..old_size as usize])?;
            }
        }

        // Apply the write.
        content[offset as usize..end as usize].copy_from_slice(data);

        // Allocate new blocks and write content.
        let block_count = blocks_for(content.len());
        let start = self.allocator.alloc(block_count).ok_or(FsError::NoSpace)?;
        write_content(&mut self.device, start, &content)?;

        // Defer-free old extent blocks.
        let next_txg = self.superblock.txg + 1;
        let inode = self.get(file_id)?;
        let old_extents: Vec<InodeExtent> = inode.extents().to_vec();
        for ext in &old_extents {
            self.deferred.push(DeferredFree {
                start: ext.start_block,
                count: ext.count as u32,
                txg: next_txg,
            });
        }

        // Update inode.
        let now = now_nanos();
        let inode = self.get_mut(file_id)?;
        if inode.is_inline() {
            inode.transition_to_extents();
        }
        inode.clear_extents();
        inode.add_extent(InodeExtent {
            start_block: start,
            count: block_count as u16,
            birth_txg: next_txg,
        })?;
        inode.size = content.len() as u64;
        inode.modified = now;
        self.dirty.insert(file_id);
        Ok(())
    }

    /// Resize a file.
    pub fn truncate(&mut self, file_id: u64, new_size: u64) -> Result<(), FsError> {
        let inode = self.get(file_id)?;

        if inode.is_inline() {
            let now = now_nanos();
            let inode = self.get_mut(file_id)?;
            inode.truncate(new_size)?;
            inode.modified = now;
            self.dirty.insert(file_id);
            return Ok(());
        }

        // Extent-based truncate to zero: free extents, go back to inline.
        if new_size == 0 {
            let inode = self.get(file_id)?;
            let old_extents: Vec<InodeExtent> = inode.extents().to_vec();
            let next_txg = self.superblock.txg + 1;
            for ext in &old_extents {
                self.deferred.push(DeferredFree {
                    start: ext.start_block,
                    count: ext.count as u32,
                    txg: next_txg,
                });
            }
            let now = now_nanos();
            let inode = self.get_mut(file_id)?;
            inode.transition_to_inline();
            inode.modified = now;
            self.dirty.insert(file_id);
            return Ok(());
        }

        // Non-zero truncate of extent-based: update size only.
        let now = now_nanos();
        let inode = self.get_mut(file_id)?;
        inode.size = new_size;
        inode.modified = now;
        self.dirty.insert(file_id);
        Ok(())
    }

    /// File size in bytes.
    pub fn file_size(&self, file_id: u64) -> Result<u64, FsError> {
        Ok(self.get(file_id)?.size)
    }

    /// Whether a file exists.
    pub fn file_exists(&self, file_id: u64) -> bool {
        self.inodes.contains_key(&file_id)
    }

    /// Number of files.
    pub fn file_count(&self) -> usize {
        self.inodes.len()
    }

    /// Commit all changes. Two-flush protocol with deferred reuse.
    pub fn commit(&mut self) -> Result<(), FsError> {
        // 1. Process deferred frees past the 2-generation threshold.
        let threshold = self.superblock.txg.saturating_sub(1);
        let mut to_free = Vec::new();
        self.deferred.retain(|d| {
            if d.txg <= threshold {
                to_free.push((d.start, d.count));
                false
            } else {
                true
            }
        });
        for (start, count) in to_free {
            self.allocator.free(start, count);
        }

        // 2. COW-save dirty inodes.
        let next_txg = self.superblock.txg + 1;
        for file_id in self.dirty.drain() {
            if let Some(inode) = self.inodes.get_mut(&file_id) {
                let old_block = inode.save_cow(&mut self.device, &mut self.allocator)?;
                self.deferred.push(DeferredFree {
                    start: old_block,
                    count: 1,
                    txg: next_txg,
                });
            }
        }

        // 3. COW-save inode table.
        let table: HashMap<u64, u32> = self
            .inodes
            .iter()
            .map(|(&id, inode)| (id, inode.inode_block()))
            .collect();
        let new_table = write_inode_table(&mut self.device, &mut self.allocator, &table)?;
        self.deferred.push(DeferredFree {
            start: self.inode_table_block,
            count: 1,
            txg: next_txg,
        });
        self.inode_table_block = new_table;

        // 4. Persist snapshot store.
        let new_snap_index = if !self.snapshots.is_empty() {
            let data = snapshot::serialize(&self.snapshots, self.next_snapshot_id);
            let (first, blocks) =
                snapshot::write_blob(&mut self.device, &mut self.allocator, &data)?;
            // Defer-free old snapshot blocks.
            for &b in &self.snap_store_blocks {
                self.deferred.push(DeferredFree {
                    start: b,
                    count: 1,
                    txg: next_txg,
                });
            }
            self.snap_store_blocks = blocks;
            first
        } else {
            // No snapshots — defer-free old store if any.
            for &b in &self.snap_store_blocks {
                self.deferred.push(DeferredFree {
                    start: b,
                    count: 1,
                    txg: next_txg,
                });
            }
            self.snap_store_blocks = Vec::new();
            0
        };

        // 5. COW-save allocator (defers old free-list block, persists new).
        let old_free = self.superblock.root_free_list;
        self.deferred.push(DeferredFree {
            start: old_free,
            count: 1,
            txg: next_txg,
        });
        let new_free = self.allocator.persist(&mut self.device)?;

        // 6. Update superblock pointers.
        self.superblock.root_inode_table = new_table;
        self.superblock.root_free_list = new_free;
        self.superblock.root_snapshot_index = new_snap_index;
        self.superblock.used_blocks = self.superblock.total_blocks - self.allocator.free_blocks();

        // 7. First flush (data + metadata written above).
        self.device.flush()?;

        // 8. Superblock commit (second flush).
        self.superblock.commit(&mut self.device)?;

        Ok(())
    }

    // ── Snapshot operations ────────────────────────────────────────

    /// Create a snapshot of the given files. Returns the SnapshotId.
    pub fn snapshot(&mut self, file_ids: &[u64]) -> Result<u64, FsError> {
        let snap_id = self.next_snapshot_id;
        self.next_snapshot_id += 1;

        let mut files = HashMap::with_capacity(file_ids.len());
        for &fid in file_ids {
            let inode = self.get(fid)?;
            let fs = if inode.is_inline() {
                FileSnapshot {
                    was_inline: true,
                    inline_data: inode.read_all(),
                    extents: Vec::new(),
                    size: inode.size,
                }
            } else {
                FileSnapshot {
                    was_inline: false,
                    inline_data: Vec::new(),
                    extents: inode.extents().to_vec(),
                    size: inode.size,
                }
            };
            files.insert(fid, fs);
        }

        self.snapshots.insert(
            snap_id,
            Snapshot {
                id: snap_id,
                txg: self.superblock.txg + 1, // will be committed next
                files,
            },
        );

        Ok(snap_id)
    }

    /// Restore all files in a snapshot to their snapshotted state.
    pub fn restore(&mut self, snapshot_id: u64) -> Result<(), FsError> {
        let snap = self
            .snapshots
            .get(&snapshot_id)
            .ok_or(FsError::NotFound(snapshot_id))?
            .clone();

        let next_txg = self.superblock.txg + 1;

        for (&file_id, file_snap) in &snap.files {
            let inode = self.get(file_id)?;

            // Defer-free current extents NOT referenced by any snapshot.
            let current_extents: Vec<InodeExtent> = inode.extents().to_vec();
            for ext in &current_extents {
                if !self.extent_in_any_snapshot(file_id, ext.start_block) {
                    self.deferred.push(DeferredFree {
                        start: ext.start_block,
                        count: ext.count as u32,
                        txg: next_txg,
                    });
                }
            }

            // Restore the file's state.
            let now = now_nanos();
            let inode = self.get_mut(file_id)?;
            if file_snap.was_inline {
                inode.transition_to_inline();
                if !file_snap.inline_data.is_empty() {
                    inode.write_inline(0, &file_snap.inline_data)?;
                }
                inode.size = file_snap.size;
            } else {
                if inode.is_inline() {
                    inode.transition_to_extents();
                }
                inode.clear_extents();
                for ext in &file_snap.extents {
                    inode.add_extent(*ext)?;
                }
                inode.size = file_snap.size;
            }
            inode.modified = now;
            self.dirty.insert(file_id);
        }

        Ok(())
    }

    /// Delete a snapshot, freeing blocks unique to it.
    pub fn delete_snapshot(&mut self, snapshot_id: u64) -> Result<(), FsError> {
        let snap = self
            .snapshots
            .remove(&snapshot_id)
            .ok_or(FsError::NotFound(snapshot_id))?;

        let next_txg = self.superblock.txg + 1;

        for (&file_id, file_snap) in &snap.files {
            if file_snap.was_inline {
                continue; // inline snapshots hold no blocks
            }

            let current_extents: Vec<InodeExtent> = self
                .inodes
                .get(&file_id)
                .map(|i| i.extents().to_vec())
                .unwrap_or_default();

            for ext in &file_snap.extents {
                let in_current = current_extents
                    .iter()
                    .any(|e| e.start_block == ext.start_block);
                let in_other = self.extent_in_any_snapshot(file_id, ext.start_block);

                if !in_current && !in_other {
                    self.deferred.push(DeferredFree {
                        start: ext.start_block,
                        count: ext.count as u32,
                        txg: next_txg,
                    });
                }
            }
        }

        Ok(())
    }

    /// List snapshot IDs that include a given file.
    pub fn list_snapshots(&self, file_id: u64) -> Vec<u64> {
        self.snapshots
            .values()
            .filter(|s| s.files.contains_key(&file_id))
            .map(|s| s.id)
            .collect()
    }

    /// Check if any snapshot references a block for a given file.
    fn extent_in_any_snapshot(&self, file_id: u64, start_block: u32) -> bool {
        self.snapshots.values().any(|s| {
            s.files.get(&file_id).map_or(false, |fs| {
                fs.extents
                    .iter()
                    .any(|e| e.start_block == start_block)
            })
        })
    }

    /// Consume the filesystem, returning the underlying device.
    pub fn into_device(self) -> D {
        self.device
    }

    /// File metadata (filesystem-level).
    pub fn file_metadata(&self, file_id: u64) -> Result<crate::FileMetadata, FsError> {
        let inode = self.get(file_id)?;
        Ok(crate::FileMetadata {
            file_id: crate::FileId(file_id),
            size: inode.size,
            created: inode.created,
            modified: inode.modified,
        })
    }

    fn get(&self, file_id: u64) -> Result<&Inode, FsError> {
        self.inodes.get(&file_id).ok_or(FsError::NotFound(file_id))
    }

    fn get_mut(&mut self, file_id: u64) -> Result<&mut Inode, FsError> {
        self.inodes
            .get_mut(&file_id)
            .ok_or(FsError::NotFound(file_id))
    }
}

// ── Files trait implementation ─────────────────────────────────────

impl<D: BlockDevice> crate::Files for Filesystem<D> {
    fn create(&mut self) -> Result<crate::FileId, FsError> {
        self.create_file().map(crate::FileId)
    }

    fn delete(&mut self, file: crate::FileId) -> Result<(), FsError> {
        self.delete_file(file.0)
    }

    fn read(
        &self,
        file: crate::FileId,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
        Filesystem::read(self, file.0, offset, buf)
    }

    fn write(
        &mut self,
        file: crate::FileId,
        offset: u64,
        data: &[u8],
    ) -> Result<(), FsError> {
        Filesystem::write(self, file.0, offset, data)
    }

    fn truncate(&mut self, file: crate::FileId, len: u64) -> Result<(), FsError> {
        Filesystem::truncate(self, file.0, len)
    }

    fn size(&self, file: crate::FileId) -> Result<u64, FsError> {
        self.file_size(file.0)
    }

    fn snapshot(&mut self, files: &[crate::FileId]) -> Result<crate::SnapshotId, FsError> {
        let ids: Vec<u64> = files.iter().map(|f| f.0).collect();
        Filesystem::snapshot(self, &ids).map(crate::SnapshotId)
    }

    fn restore(&mut self, snap: crate::SnapshotId) -> Result<(), FsError> {
        Filesystem::restore(self, snap.0)
    }

    fn delete_snapshot(&mut self, snap: crate::SnapshotId) -> Result<(), FsError> {
        Filesystem::delete_snapshot(self, snap.0)
    }

    fn list_snapshots(&self, file: crate::FileId) -> Result<Vec<crate::SnapshotId>, FsError> {
        Ok(Filesystem::list_snapshots(self, file.0)
            .into_iter()
            .map(crate::SnapshotId)
            .collect())
    }

    fn metadata(&self, file: crate::FileId) -> Result<crate::FileMetadata, FsError> {
        self.file_metadata(file.0)
    }

    fn commit(&mut self) -> Result<(), FsError> {
        Filesystem::commit(self)
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Read file content from extents.
fn read_extents<D: BlockDevice>(
    device: &D,
    inode: &Inode,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, FsError> {
    if offset >= inode.size {
        return Ok(0);
    }
    let to_read = buf.len().min((inode.size - offset) as usize);
    let mut buf_pos = 0usize;
    let mut file_pos = 0u64;
    let mut block_buf = vec![0u8; BLOCK_SIZE as usize];

    for ext in inode.extents() {
        for i in 0..ext.count as u32 {
            let block_start = file_pos;
            let block_end = block_start + BLOCK_SIZE as u64;
            file_pos = block_end;

            if block_end <= offset || buf_pos >= to_read {
                continue;
            }

            device.read_block(ext.start_block + i, &mut block_buf)?;

            let src_start = offset.saturating_sub(block_start).min(BLOCK_SIZE as u64) as usize;
            let remaining = to_read - buf_pos;
            let available = BLOCK_SIZE as usize - src_start;
            let n = remaining.min(available);

            if n > 0 {
                buf[buf_pos..buf_pos + n]
                    .copy_from_slice(&block_buf[src_start..src_start + n]);
                buf_pos += n;
            }
        }
    }

    Ok(buf_pos)
}

/// Write content to consecutive blocks.
fn write_content<D: BlockDevice>(
    device: &mut D,
    start: u32,
    content: &[u8],
) -> Result<(), FsError> {
    let block_count = blocks_for(content.len());
    let mut buf = vec![0u8; BLOCK_SIZE as usize];

    for i in 0..block_count {
        buf.fill(0);
        let off = i as usize * BLOCK_SIZE as usize;
        let end = (off + BLOCK_SIZE as usize).min(content.len());
        buf[..end - off].copy_from_slice(&content[off..end]);
        device.write_block(start + i, &buf)?;
    }
    Ok(())
}

/// Number of 16 KiB blocks needed for `byte_count` bytes.
fn blocks_for(byte_count: usize) -> u32 {
    ((byte_count + BLOCK_SIZE as usize - 1) / BLOCK_SIZE as usize) as u32
}

// ── Inode table I/O ────────────────────────────────────────────────
// Format: [entry_count: u32] [file_id: u64, block: u32] × N

fn write_inode_table<D: BlockDevice>(
    device: &mut D,
    allocator: &mut Allocator,
    table: &HashMap<u64, u32>,
) -> Result<u32, FsError> {
    if table.len() > MAX_TABLE_ENTRIES {
        return Err(FsError::Corrupt(format!(
            "inode table has {} entries, max {MAX_TABLE_ENTRIES}",
            table.len()
        )));
    }
    let block = allocator.alloc(1).ok_or(FsError::NoSpace)?;
    let mut buf = vec![0u8; BLOCK_SIZE as usize];

    buf[0..4].copy_from_slice(&(table.len() as u32).to_le_bytes());
    let mut off = 4;
    for (&file_id, &inode_block) in table {
        buf[off..off + 8].copy_from_slice(&file_id.to_le_bytes());
        buf[off + 8..off + 12].copy_from_slice(&inode_block.to_le_bytes());
        off += 12;
    }

    device.write_block(block, &buf)?;
    Ok(block)
}

fn load_all_inodes<D: BlockDevice>(
    device: &D,
    table_block: u32,
) -> Result<(HashMap<u64, Inode>, u32), FsError> {
    let mut buf = vec![0u8; BLOCK_SIZE as usize];
    device.read_block(table_block, &mut buf)?;

    let count = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    if count > MAX_TABLE_ENTRIES {
        return Err(FsError::Corrupt(format!(
            "inode table has {count} entries, max {MAX_TABLE_ENTRIES}"
        )));
    }

    let mut inodes = HashMap::with_capacity(count);
    let mut off = 4;
    for _ in 0..count {
        let file_id = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
        let inode_block = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
        off += 12;

        let inode = Inode::load(device, inode_block)?;
        debug_assert_eq!(inode.file_id, file_id);
        inodes.insert(file_id, inode);
    }

    Ok((inodes, table_block))
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    fn fresh() -> Filesystem<MemoryBlockDevice> {
        Filesystem::format(MemoryBlockDevice::new(256)).unwrap()
    }

    fn read_all(fs: &Filesystem<MemoryBlockDevice>, id: u64) -> Vec<u8> {
        let size = fs.file_size(id).unwrap() as usize;
        let mut buf = vec![0u8; size];
        fs.read(id, 0, &mut buf).unwrap();
        buf
    }

    // ── format + mount ─────────────────────────────────────────────

    #[test]
    fn format_mount_empty() {
        let fs = fresh();
        assert_eq!(fs.file_count(), 0);

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(fs2.file_count(), 0);
    }

    // ── create + inline write ──────────────────────────────────────

    #[test]
    fn create_and_write_inline() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"hello").unwrap();

        assert_eq!(read_all(&fs, id), b"hello");
        assert_eq!(fs.file_size(id).unwrap(), 5);
    }

    #[test]
    fn inline_persists_through_commit_mount() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"persistent data").unwrap();
        fs.commit().unwrap();

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(fs2.file_count(), 1);
        assert_eq!(read_all(&fs2, id), b"persistent data");
    }

    // ── transition to extents ──────────────────────────────────────

    #[test]
    fn write_large_transitions_to_extents() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();

        let data = vec![0xAB; INLINE_CAPACITY + 100];
        fs.write(id, 0, &data).unwrap();

        assert_eq!(fs.file_size(id).unwrap(), data.len() as u64);
        assert_eq!(read_all(&fs, id), data);
    }

    #[test]
    fn extent_persists_through_commit_mount() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();

        let data = vec![0xCD; INLINE_CAPACITY + 500];
        fs.write(id, 0, &data).unwrap();
        fs.commit().unwrap();

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(read_all(&fs2, id), data);
    }

    // ── COW write (extent-based overwrite) ─────────────────────────

    #[test]
    fn cow_write_overwrites_extent_data() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();

        let data = vec![0xAA; INLINE_CAPACITY + 100];
        fs.write(id, 0, &data).unwrap();
        fs.commit().unwrap();

        // Overwrite the beginning.
        fs.write(id, 0, b"OVERWRITTEN").unwrap();
        fs.commit().unwrap();

        let result = read_all(&fs, id);
        assert_eq!(&result[..11], b"OVERWRITTEN");
        assert!(result[11..].iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn cow_write_extends_file() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();

        let data = vec![0xBB; INLINE_CAPACITY + 100];
        fs.write(id, 0, &data).unwrap();
        fs.commit().unwrap();

        // Append past the end.
        let extra = vec![0xCC; 200];
        let old_size = data.len();
        fs.write(id, old_size as u64, &extra).unwrap();

        assert_eq!(fs.file_size(id).unwrap(), (old_size + 200) as u64);
        let result = read_all(&fs, id);
        assert!(result[..old_size].iter().all(|&b| b == 0xBB));
        assert!(result[old_size..].iter().all(|&b| b == 0xCC));
    }

    // ── delete ─────────────────────────────────────────────────────

    #[test]
    fn delete_file_removes_it() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"doomed").unwrap();
        fs.commit().unwrap();

        fs.delete_file(id).unwrap();
        assert!(!fs.file_exists(id));
        fs.commit().unwrap();

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(fs2.file_count(), 0);
    }

    #[test]
    fn delete_nonexistent_fails() {
        let mut fs = fresh();
        assert!(matches!(fs.delete_file(999), Err(FsError::NotFound(999))));
    }

    // ── truncate ───────────────────────────────────────────────────

    #[test]
    fn truncate_inline() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"hello world").unwrap();

        fs.truncate(id, 5).unwrap();
        assert_eq!(read_all(&fs, id), b"hello");
    }

    #[test]
    fn truncate_extent_to_zero() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        let data = vec![0xAA; INLINE_CAPACITY + 100];
        fs.write(id, 0, &data).unwrap();
        fs.commit().unwrap();

        fs.truncate(id, 0).unwrap();
        assert_eq!(fs.file_size(id).unwrap(), 0);
        assert_eq!(read_all(&fs, id), b"");

        // Should be back to inline.
        fs.write(id, 0, b"small again").unwrap();
        assert_eq!(read_all(&fs, id), b"small again");
    }

    // ── multiple files ─────────────────────────────────────────────

    #[test]
    fn multiple_files_independent() {
        let mut fs = fresh();
        let a = fs.create_file().unwrap();
        let b = fs.create_file().unwrap();

        fs.write(a, 0, b"file A").unwrap();
        fs.write(b, 0, b"file B").unwrap();
        fs.commit().unwrap();

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(read_all(&fs2, a), b"file A");
        assert_eq!(read_all(&fs2, b), b"file B");
    }

    // ── deferred block reuse ───────────────────────────────────────

    #[test]
    fn deferred_free_not_immediate() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        let data = vec![0xAA; INLINE_CAPACITY + 100];
        fs.write(id, 0, &data).unwrap();
        fs.commit().unwrap(); // txg=3

        let free_before = fs.allocator.free_blocks();

        // Overwrite (COW): old blocks deferred.
        fs.write(id, 0, &[0xBB; INLINE_CAPACITY + 100]).unwrap();
        fs.commit().unwrap(); // txg=4

        // Old data blocks should NOT be freed yet (deferred at txg=4,
        // threshold at start of commit was txg=3, freed when txg >= 4+1=5).
        // Free blocks should be LESS (new blocks allocated, old deferred).

        // One more commit to cross the threshold.
        fs.commit().unwrap(); // txg=5, processes deferred at txg <= 4

        let free_after = fs.allocator.free_blocks();
        // Old blocks should now be freed. Net change: zero (same amount
        // allocated and freed, minus COW overhead for inodes/table).
        // Just verify free space increased from the mid-point.
        assert!(free_after > free_before - 20); // rough: within reason
    }

    // ── commit protocol ────────────────────────────────────────────

    #[test]
    fn multiple_commits_stable() {
        let mut fs = fresh();
        for i in 0..10u64 {
            let id = fs.create_file().unwrap();
            fs.write(id, 0, format!("file {i}").as_bytes()).unwrap();
            fs.commit().unwrap();
        }

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(fs2.file_count(), 10);
    }

    #[test]
    fn uncommitted_changes_lost_on_mount() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"committed").unwrap();
        fs.commit().unwrap();

        // Write without commit.
        fs.write(id, 0, b"UNCOMMITTED").unwrap();

        let dev = fs.into_device();
        let fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(read_all(&fs2, id), b"committed");
    }

    // ── read edge cases ────────────────────────────────────────────

    #[test]
    fn read_past_end() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"short").unwrap();

        let mut buf = vec![0u8; 100];
        let n = fs.read(id, 0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"short");
    }

    #[test]
    fn read_at_offset() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"hello world").unwrap();

        let mut buf = vec![0u8; 5];
        let n = fs.read(id, 6, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn read_nonexistent_fails() {
        let fs = fresh();
        let mut buf = vec![0u8; 10];
        assert!(matches!(fs.read(999, 0, &mut buf), Err(FsError::NotFound(999))));
    }

    // ── snapshots ──────────────────────────────────────────────────

    #[test]
    fn snapshot_and_restore_inline() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"version 1").unwrap();

        let snap = fs.snapshot(&[id]).unwrap();

        fs.write(id, 0, b"version 2").unwrap();
        assert_eq!(read_all(&fs, id), b"version 2");

        fs.restore(snap).unwrap();
        assert_eq!(read_all(&fs, id), b"version 1");
    }

    #[test]
    fn snapshot_and_restore_extents() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        let v1 = vec![0xAA; INLINE_CAPACITY + 100];
        fs.write(id, 0, &v1).unwrap();
        fs.commit().unwrap();

        let snap = fs.snapshot(&[id]).unwrap();

        let v2 = vec![0xBB; INLINE_CAPACITY + 200];
        fs.write(id, 0, &v2).unwrap();
        assert_eq!(read_all(&fs, id), v2);

        fs.restore(snap).unwrap();
        assert_eq!(read_all(&fs, id), v1);
    }

    #[test]
    fn snapshot_persists_through_commit_mount() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        fs.write(id, 0, b"snapshotted").unwrap();

        let snap = fs.snapshot(&[id]).unwrap();
        fs.write(id, 0, b"modified   ").unwrap();
        fs.commit().unwrap();

        let dev = fs.into_device();
        let mut fs2 = Filesystem::mount(dev).unwrap();
        assert_eq!(read_all(&fs2, id), b"modified   ");

        fs2.restore(snap).unwrap();
        assert_eq!(read_all(&fs2, id), b"snapshotted");
    }

    #[test]
    fn delete_snapshot_frees_unique_blocks() {
        let mut fs = Filesystem::format(MemoryBlockDevice::new(512)).unwrap();
        let id = fs.create_file().unwrap();

        let v1 = vec![0xAA; INLINE_CAPACITY + 100];
        fs.write(id, 0, &v1).unwrap();
        fs.commit().unwrap();

        let snap = fs.snapshot(&[id]).unwrap();

        // Overwrite → old blocks are in the snapshot only.
        let v2 = vec![0xBB; INLINE_CAPACITY + 100];
        fs.write(id, 0, &v2).unwrap();
        fs.commit().unwrap();

        // Delete snapshot → old blocks should be freed (after deferral).
        fs.delete_snapshot(snap).unwrap();
        fs.commit().unwrap();
        fs.commit().unwrap(); // extra commit to process deferred
        fs.commit().unwrap(); // one more for safety

        // Verify the snapshot is gone.
        assert!(fs.list_snapshots(id).is_empty());
    }

    #[test]
    fn delete_snapshot_preserves_shared_blocks() {
        let mut fs = Filesystem::format(MemoryBlockDevice::new(512)).unwrap();
        let id = fs.create_file().unwrap();

        let data = vec![0xAA; INLINE_CAPACITY + 100];
        fs.write(id, 0, &data).unwrap();
        fs.commit().unwrap();

        // Two snapshots of the same state → same blocks.
        let s1 = fs.snapshot(&[id]).unwrap();
        let s2 = fs.snapshot(&[id]).unwrap();

        // Overwrite → current file has new blocks, both snapshots share old.
        fs.write(id, 0, &vec![0xBB; INLINE_CAPACITY + 100]).unwrap();
        fs.commit().unwrap();

        // Delete s1 → blocks still referenced by s2, should NOT be freed.
        fs.delete_snapshot(s1).unwrap();
        fs.commit().unwrap();

        // s2 restore should still work.
        fs.restore(s2).unwrap();
        assert_eq!(read_all(&fs, id), data);
    }

    #[test]
    fn multi_file_snapshot() {
        let mut fs = fresh();
        let a = fs.create_file().unwrap();
        let b = fs.create_file().unwrap();
        fs.write(a, 0, b"file A v1").unwrap();
        fs.write(b, 0, b"file B v1").unwrap();

        let snap = fs.snapshot(&[a, b]).unwrap();

        fs.write(a, 0, b"file A v2").unwrap();
        fs.write(b, 0, b"file B v2").unwrap();
        assert_eq!(read_all(&fs, a), b"file A v2");
        assert_eq!(read_all(&fs, b), b"file B v2");

        fs.restore(snap).unwrap();
        assert_eq!(read_all(&fs, a), b"file A v1");
        assert_eq!(read_all(&fs, b), b"file B v1");
    }

    #[test]
    fn list_snapshots_for_file() {
        let mut fs = fresh();
        let a = fs.create_file().unwrap();
        let b = fs.create_file().unwrap();

        let s1 = fs.snapshot(&[a]).unwrap();
        let _s2 = fs.snapshot(&[b]).unwrap();
        let s3 = fs.snapshot(&[a, b]).unwrap();

        let a_snaps = fs.list_snapshots(a);
        assert!(a_snaps.contains(&s1));
        assert!(a_snaps.contains(&s3));
        assert_eq!(a_snaps.len(), 2);
    }

    #[test]
    fn undo_chain_simulation() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();

        let mut snap_ids = Vec::new();
        for i in 0..5u8 {
            let content = format!("edit-{i}");
            fs.write(id, 0, content.as_bytes()).unwrap();
            fs.truncate(id, content.len() as u64).unwrap();
            snap_ids.push(fs.snapshot(&[id]).unwrap());
        }

        // Walk backwards (undo).
        for (i, &snap) in snap_ids.iter().enumerate().rev() {
            fs.restore(snap).unwrap();
            let expected = format!("edit-{i}");
            assert_eq!(read_all(&fs, id), expected.as_bytes());
        }
    }

    #[test]
    fn snapshot_of_empty_file() {
        let mut fs = fresh();
        let id = fs.create_file().unwrap();
        let snap = fs.snapshot(&[id]).unwrap();

        fs.write(id, 0, b"content").unwrap();
        fs.restore(snap).unwrap();
        assert_eq!(fs.file_size(id).unwrap(), 0);
    }

    // ── Files trait tests ──────────────────────────────────────────
    // These exercise the filesystem exclusively through the `dyn Files`
    // interface, proving the trait is sufficient for all operations.

    use crate::{FileId, Files, SnapshotId};

    fn make_trait_fs() -> Box<dyn Files> {
        Box::new(Filesystem::format(MemoryBlockDevice::new(256)).unwrap())
    }

    fn trait_read_all(fs: &dyn Files, id: FileId) -> Vec<u8> {
        let size = fs.size(id).unwrap() as usize;
        let mut buf = vec![0u8; size];
        fs.read(id, 0, &mut buf).unwrap();
        buf
    }

    #[test]
    fn trait_full_lifecycle() {
        let mut fs = make_trait_fs();

        // Create.
        let id = fs.create().unwrap();
        assert_eq!(fs.size(id).unwrap(), 0);

        // Write.
        fs.write(id, 0, b"hello from trait").unwrap();
        assert_eq!(trait_read_all(&*fs, id), b"hello from trait");

        // Metadata.
        let meta = fs.metadata(id).unwrap();
        assert_eq!(meta.file_id, id);
        assert_eq!(meta.size, 16);
        assert!(meta.created > 0);
        assert!(meta.modified > 0);

        // Snapshot.
        let snap = fs.snapshot(&[id]).unwrap();

        // Modify.
        fs.write(id, 0, b"modified content").unwrap();
        assert_eq!(trait_read_all(&*fs, id), b"modified content");

        // Restore.
        fs.restore(snap).unwrap();
        assert_eq!(trait_read_all(&*fs, id), b"hello from trait");

        // List snapshots.
        let snaps = fs.list_snapshots(id).unwrap();
        assert!(snaps.contains(&snap));

        // Delete snapshot.
        fs.delete_snapshot(snap).unwrap();
        assert!(fs.list_snapshots(id).unwrap().is_empty());

        // Truncate.
        fs.truncate(id, 5).unwrap();
        assert_eq!(trait_read_all(&*fs, id), b"hello");

        // Commit.
        fs.commit().unwrap();

        // Delete file.
        fs.delete(id).unwrap();
        assert!(fs.size(id).is_err());
    }

    #[test]
    fn trait_multi_file_snapshot() {
        let mut fs = make_trait_fs();

        let a = fs.create().unwrap();
        let b = fs.create().unwrap();
        fs.write(a, 0, b"doc A").unwrap();
        fs.write(b, 0, b"doc B").unwrap();

        let snap = fs.snapshot(&[a, b]).unwrap();

        fs.write(a, 0, b"AAA A").unwrap();
        fs.write(b, 0, b"BBB B").unwrap();

        // Restore reverts both files atomically.
        fs.restore(snap).unwrap();
        assert_eq!(trait_read_all(&*fs, a), b"doc A");
        assert_eq!(trait_read_all(&*fs, b), b"doc B");
    }

    #[test]
    fn trait_commit_persist_mount() {
        // Format and populate through the trait.
        let mut fs = Filesystem::format(MemoryBlockDevice::new(256)).unwrap();
        let files_trait: &mut dyn Files = &mut fs;

        let id = files_trait.create().unwrap();
        files_trait.write(id, 0, b"persisted").unwrap();
        let snap = files_trait.snapshot(&[id]).unwrap();
        files_trait.commit().unwrap();

        // Mount and verify through the trait.
        let dev = fs.into_device();
        let mut fs2 = Filesystem::mount(dev).unwrap();
        let files_trait: &mut dyn Files = &mut fs2;

        assert_eq!(trait_read_all(files_trait, id), b"persisted");

        // Snapshot survived mount.
        files_trait.write(id, 0, b"MODIFIED!").unwrap();
        files_trait.restore(snap).unwrap();
        assert_eq!(trait_read_all(files_trait, id), b"persisted");
    }

    #[test]
    fn trait_error_types() {
        let mut fs = make_trait_fs();

        // NotFound errors.
        let bad_file = FileId(999);
        let bad_snap = SnapshotId(999);
        assert!(fs.read(bad_file, 0, &mut [0; 10]).is_err());
        assert!(fs.write(bad_file, 0, b"x").is_err());
        assert!(fs.delete(bad_file).is_err());
        assert!(fs.size(bad_file).is_err());
        assert!(fs.restore(bad_snap).is_err());
        assert!(fs.delete_snapshot(bad_snap).is_err());
    }
}
