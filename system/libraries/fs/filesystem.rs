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

use alloc::{collections::BTreeMap, collections::BTreeSet, format, vec, vec::Vec};

use crate::alloc_mod::Allocator;
use crate::block::BlockDevice;
use crate::inode::{Inode, InodeExtent, INLINE_CAPACITY};
use crate::snapshot::{self, FileSnapshot, Snapshot};
use crate::superblock::Superblock;
use crate::{now_nanos, FsError, BLOCK_SIZE};

/// A COW filesystem with per-file snapshots.
pub struct Filesystem<D: BlockDevice> {
    device: D,
    superblock: Superblock,
    allocator: Allocator,
    inodes: BTreeMap<u64, Inode>,
    dirty: BTreeSet<u64>,
    deferred: Vec<DeferredFree>,
    /// Blocks occupied by the current on-disk inode table chain.
    inode_table_blocks: Vec<u32>,
    // Snapshot state
    snapshots: BTreeMap<u64, Snapshot>,
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

        let (first_block, table_blocks) =
            write_inode_table(&mut device, &mut alloc, &BTreeMap::new())?;
        let free_block = alloc.persist(&mut device)?;

        sb.root_inode_table = first_block;
        sb.root_free_list = free_block;
        sb.used_blocks = sb.total_blocks - alloc.free_blocks();
        device.flush()?;
        sb.commit(&mut device)?;

        Ok(Self {
            device,
            superblock: sb,
            allocator: alloc,
            inodes: BTreeMap::new(),
            dirty: BTreeSet::new(),
            deferred: Vec::new(),
            inode_table_blocks: table_blocks,
            snapshots: BTreeMap::new(),
            next_snapshot_id: 1,
            snap_store_blocks: Vec::new(),
        })
    }

    /// Mount an existing filesystem.
    pub fn mount(device: D) -> Result<Self, FsError> {
        let sb = Superblock::mount(&device)?;
        let alloc = Allocator::load(&device, sb.root_free_list)?;
        let (inodes, table_blocks) = load_all_inodes(&device, sb.root_inode_table)?;

        // Load snapshot store.
        let (snapshots, next_snapshot_id, snap_store_blocks) =
            if sb.root_snapshot_index != 0 {
                let (data, blocks) = snapshot::read_blob(&device, sb.root_snapshot_index)?;
                let (snaps, next_id) = snapshot::deserialize(&data)?;
                (snaps, next_id, blocks)
            } else {
                (BTreeMap::new(), 1, Vec::new())
            };

        Ok(Self {
            device,
            superblock: sb,
            allocator: alloc,
            inodes,
            dirty: BTreeSet::new(),
            deferred: Vec::new(),
            inode_table_blocks: table_blocks,
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
        for file_id in self.dirty.iter().copied().collect::<Vec<_>>() {
            if let Some(inode) = self.inodes.get_mut(&file_id) {
                let old_block = inode.save_cow(&mut self.device, &mut self.allocator)?;
                self.deferred.push(DeferredFree {
                    start: old_block,
                    count: 1,
                    txg: next_txg,
                });
            }
        }
        self.dirty.clear();

        // 3. COW-save inode table.
        let table: BTreeMap<u64, u32> = self
            .inodes
            .iter()
            .map(|(&id, inode)| (id, inode.inode_block()))
            .collect();
        let (new_first, new_table_blocks) =
            write_inode_table(&mut self.device, &mut self.allocator, &table)?;
        for &b in &self.inode_table_blocks {
            self.deferred.push(DeferredFree {
                start: b,
                count: 1,
                txg: next_txg,
            });
        }
        self.inode_table_blocks = new_table_blocks;

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
        self.superblock.root_inode_table = new_first;
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

        let mut files = BTreeMap::new();
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

    /// List all file IDs.
    pub fn list_files(&self) -> Vec<crate::FileId> {
        self.inodes.keys().map(|&id| crate::FileId(id)).collect()
    }

    /// Set the root file. The file must exist.
    pub fn set_root(&mut self, file_id: u64) -> Result<(), FsError> {
        if !self.inodes.contains_key(&file_id) {
            return Err(FsError::NotFound(file_id));
        }
        self.superblock.root_file = Some(file_id);
        Ok(())
    }

    /// Get the root file, if one has been set.
    pub fn root(&self) -> Option<crate::FileId> {
        self.superblock.root_file.map(crate::FileId)
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

    fn list_files(&self) -> Result<Vec<crate::FileId>, FsError> {
        Ok(Filesystem::list_files(self))
    }

    fn set_root(&mut self, file: crate::FileId) -> Result<(), FsError> {
        Filesystem::set_root(self, file.0)
    }

    fn root(&self) -> Option<crate::FileId> {
        Filesystem::root(self)
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
// Stored as a linked-block chain via snapshot::write_blob/read_blob.

fn write_inode_table<D: BlockDevice>(
    device: &mut D,
    allocator: &mut Allocator,
    table: &BTreeMap<u64, u32>,
) -> Result<(u32, Vec<u32>), FsError> {
    let mut buf = vec![0u8; 4 + table.len() * 12];
    buf[0..4].copy_from_slice(&(table.len() as u32).to_le_bytes());
    let mut off = 4;
    for (&file_id, &inode_block) in table {
        buf[off..off + 8].copy_from_slice(&file_id.to_le_bytes());
        buf[off + 8..off + 12].copy_from_slice(&inode_block.to_le_bytes());
        off += 12;
    }
    snapshot::write_blob(device, allocator, &buf)
}

fn load_all_inodes<D: BlockDevice>(
    device: &D,
    first_block: u32,
) -> Result<(BTreeMap<u64, Inode>, Vec<u32>), FsError> {
    let (data, block_list) = snapshot::read_blob(device, first_block)?;
    if data.len() < 4 {
        return Err(FsError::Corrupt(format!(
            "inode table too small: {} bytes",
            data.len()
        )));
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let expected_len = 4 + count * 12;
    if data.len() < expected_len {
        return Err(FsError::Corrupt(format!(
            "inode table truncated: {count} entries need {expected_len} bytes, got {}",
            data.len()
        )));
    }

    let mut inodes = BTreeMap::new();
    let mut off = 4;
    for _ in 0..count {
        let file_id = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        let inode_block = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap());
        off += 12;

        let inode = Inode::load(device, inode_block)?;
        debug_assert_eq!(inode.file_id, file_id);
        inodes.insert(file_id, inode);
    }

    Ok((inodes, block_list))
}
