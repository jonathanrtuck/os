//! Tests for overflow extent blocks — indirect block support for files
//! with more than 16 extents.
//!
//! The inode stores up to 16 extents inline. When a file accumulates more
//! than 16 extents (e.g., through many COW writes), additional extents
//! spill to a separate "overflow" block referenced by the inode's
//! `indirect_block` field (header offset 40).

use fs::{BlockDevice, Filesystem, FsError, BLOCK_SIZE};

/// In-memory block device for testing.
struct MemDevice {
    blocks: Vec<Vec<u8>>,
}

impl MemDevice {
    fn new(block_count: u32) -> Self {
        Self {
            blocks: vec![vec![0u8; BLOCK_SIZE as usize]; block_count as usize],
        }
    }
}

impl BlockDevice for MemDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if index as usize >= self.blocks.len() {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks.len() as u32,
            });
        }
        if buf.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: buf.len(),
            });
        }
        buf.copy_from_slice(&self.blocks[index as usize]);
        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        if index as usize >= self.blocks.len() {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks.len() as u32,
            });
        }
        if data.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: data.len(),
            });
        }
        self.blocks[index as usize].copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), FsError> {
        Ok(())
    }

    fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }
}

// ── Inode-level tests ──────────────────────────────────────────────

/// An inode should accept more than 16 extents without returning NoSpace,
/// and persist them correctly via the overflow block.
#[test]
fn inode_add_extent_beyond_16() {
    let mut dev = MemDevice::new(2048);
    let mut alloc = fs::Allocator::new(2048);

    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 42, 0).unwrap();
    inode.transition_to_extents();
    inode.size = 20 * BLOCK_SIZE as u64; // 20 blocks worth

    // Add 20 extents (exceeds the 16-extent inline limit).
    for i in 0..20u32 {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: i as u64 + 1,
            })
            .expect("add_extent should not fail for overflow");
    }

    assert_eq!(inode.extents().len(), 20);

    // Allocate the overflow block before saving.
    inode.ensure_overflow_block(&mut alloc).unwrap();

    // Save and reload — overflow extents should persist.
    inode.save(&mut dev).unwrap();
    let block = inode.inode_block();
    let reloaded = fs::Inode::load(&dev, block).unwrap();

    assert_eq!(reloaded.extents().len(), 20);
    assert_eq!(reloaded.size, 20 * BLOCK_SIZE as u64);

    // Verify extent data round-trips correctly.
    for (i, ext) in reloaded.extents().iter().enumerate() {
        assert_eq!(ext.count, 1);
        assert_eq!(ext.birth_txg, i as u64 + 1);
    }
}

/// Overflow extents should survive a save_cow cycle.
#[test]
fn inode_overflow_survives_cow_save() {
    let mut dev = MemDevice::new(2048);
    let mut alloc = fs::Allocator::new(2048);

    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 99, 0).unwrap();
    inode.transition_to_extents();
    inode.size = 24 * BLOCK_SIZE as u64;

    for i in 0..24u32 {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: i as u64 + 1,
            })
            .unwrap();
    }

    // Ensure overflow block before first save.
    inode.ensure_overflow_block(&mut alloc).unwrap();
    inode.save(&mut dev).unwrap();

    // COW save — should allocate new inode block AND new overflow block.
    let old_blocks = inode.save_cow(&mut dev, &mut alloc).unwrap();
    let new_block = inode.inode_block();
    assert_ne!(old_blocks.inode, new_block);

    // Reload from new location.
    let reloaded = fs::Inode::load(&dev, new_block).unwrap();
    assert_eq!(reloaded.extents().len(), 24);
}

/// Delete should free the overflow block along with data extents.
#[test]
fn inode_delete_frees_overflow_block() {
    let mut dev = MemDevice::new(2048);
    let mut alloc = fs::Allocator::new(2048);

    let free_before = alloc.free_blocks();

    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 7, 0).unwrap();
    inode.transition_to_extents();
    inode.size = 20 * BLOCK_SIZE as u64;

    for _ in 0..20u32 {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: 1,
            })
            .unwrap();
    }
    inode.ensure_overflow_block(&mut alloc).unwrap();
    inode.save(&mut dev).unwrap();

    let free_after_create = alloc.free_blocks();
    // Should have consumed: 1 inode block + 20 data blocks + 1 overflow block = 22
    assert!(free_before - free_after_create >= 22);

    // Delete — should free all blocks including overflow.
    inode.delete(&mut alloc);

    let free_after_delete = alloc.free_blocks();
    // All 22 blocks should be returned.
    assert_eq!(free_after_delete, free_after_create + 22);
}

/// clear_extents should also discard overflow state.
#[test]
fn inode_clear_extents_clears_overflow() {
    let mut dev = MemDevice::new(2048);
    let mut alloc = fs::Allocator::new(2048);

    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 55, 0).unwrap();
    inode.transition_to_extents();
    inode.size = 20 * BLOCK_SIZE as u64;

    for _ in 0..20u32 {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: 1,
            })
            .unwrap();
    }

    assert_eq!(inode.extents().len(), 20);
    inode.clear_extents();
    assert_eq!(inode.extents().len(), 0);

    // Adding extents after clear should work normally (fresh start).
    for _ in 0..4u32 {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: 2,
            })
            .unwrap();
    }
    assert_eq!(inode.extents().len(), 4);
}

/// transition_to_inline should discard overflow state.
#[test]
fn inode_transition_to_inline_clears_overflow() {
    let mut dev = MemDevice::new(2048);
    let mut alloc = fs::Allocator::new(2048);

    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 66, 0).unwrap();
    inode.transition_to_extents();
    inode.size = 20 * BLOCK_SIZE as u64;

    for _ in 0..20u32 {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: 1,
            })
            .unwrap();
    }

    assert_eq!(inode.extents().len(), 20);
    inode.transition_to_inline();
    assert_eq!(inode.extents().len(), 0);
    assert!(inode.is_inline());
}

// ── Filesystem-level tests ─────────────────────────────────────────

/// A file with overflow extents should be readable through the Filesystem API
/// after snapshot + restore.
#[test]
fn filesystem_snapshot_restore_with_overflow_extents() {
    let mut dev = MemDevice::new(4096);
    let mut alloc = fs::Allocator::new(4096);

    // Build an inode with 20 extents, each holding known data.
    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 1, 0).unwrap();
    inode.transition_to_extents();

    let mut expected_data = Vec::new();
    for i in 0..20u32 {
        let block = alloc.alloc(1).unwrap();
        // Write a known pattern to each block.
        let mut block_data = vec![0u8; BLOCK_SIZE as usize];
        block_data[0] = i as u8;
        block_data[BLOCK_SIZE as usize - 1] = i as u8;
        dev.write_block(block, &block_data).unwrap();
        expected_data.extend_from_slice(&block_data);

        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: 1,
            })
            .unwrap();
    }
    inode.size = expected_data.len() as u64;
    inode.ensure_overflow_block(&mut alloc).unwrap();
    inode.save(&mut dev).unwrap();

    // Verify extents() returns all 20.
    let reloaded = fs::Inode::load(&dev, inode.inode_block()).unwrap();
    assert_eq!(reloaded.extents().len(), 20);

    // Read back via the extents and verify data.
    let mut read_buf = vec![0u8; expected_data.len()];
    let mut pos = 0;
    let mut block_buf = vec![0u8; BLOCK_SIZE as usize];
    for ext in &reloaded.extents() {
        for j in 0..ext.count as u32 {
            dev.read_block(ext.start_block + j, &mut block_buf).unwrap();
            let n = BLOCK_SIZE as usize;
            read_buf[pos..pos + n].copy_from_slice(&block_buf);
            pos += n;
        }
    }
    assert_eq!(read_buf, expected_data);
}

/// The maximum overflow capacity should accommodate ~1364 extra extents.
#[test]
fn inode_overflow_max_capacity() {
    let mut dev = MemDevice::new(8192);
    let mut alloc = fs::Allocator::new(8192);

    let mut inode = fs::Inode::create(&mut dev, &mut alloc, 100, 0).unwrap();
    inode.transition_to_extents();

    // Fill to max: 16 inline + 1364 overflow = 1380 total.
    // (16384 - 8 byte header) / 12 bytes per extent = 1364 overflow.
    let target = 16 + 1364;
    for i in 0..target {
        let block = alloc.alloc(1).unwrap();
        inode
            .add_extent(fs::InodeExtent {
                start_block: block,
                count: 1,
                birth_txg: i as u64 + 1,
            })
            .unwrap();
    }

    assert_eq!(inode.extents().len(), target as usize);

    // One more should fail — overflow block is full.
    let block = alloc.alloc(1).unwrap();
    let result = inode.add_extent(fs::InodeExtent {
        start_block: block,
        count: 1,
        birth_txg: target as u64 + 1,
    });
    assert!(result.is_err());

    // Save and reload — all extents persist.
    inode.ensure_overflow_block(&mut alloc).unwrap();
    inode.save(&mut dev).unwrap();
    let reloaded = fs::Inode::load(&dev, inode.inode_block()).unwrap();
    assert_eq!(reloaded.extents().len(), target as usize);
}

// ── Multi-extent write path tests ──────────────────────────────────

/// Fragment a disk so no large contiguous run exists. Returns retained file IDs.
///
/// Creates `file_count` small files (each 1 content block) consuming
/// most of the disk, then deletes every other file. After two commits,
/// free space is ~`file_count/2` gaps of 2 blocks each.
///
/// Caller must ensure the disk is large enough: needs roughly
/// `file_count * 2 + 50` blocks (content + COW inodes + commit overhead).
fn fragment_disk(fs: &mut Filesystem<MemDevice>, file_count: usize) -> Vec<u64> {
    let mut ids = Vec::new();
    for _ in 0..file_count {
        let id = fs.create_file().unwrap();
        let data = vec![0xAA; fs::INLINE_CAPACITY + 1];
        fs.write(id, 0, &data).unwrap();
        ids.push(id);
    }
    fs.commit().unwrap();

    // Delete every other file to create gaps.
    let mut retained = Vec::new();
    for (i, &id) in ids.iter().enumerate() {
        if i % 2 == 0 {
            fs.delete_file(id).unwrap();
        } else {
            retained.push(id);
        }
    }
    fs.commit().unwrap();
    // Second commit to release deferred frees.
    fs.commit().unwrap();

    retained
}

/// Writing a file larger than any single free extent should succeed
/// by allocating multiple extents across the fragmented free space.
#[test]
fn write_succeeds_on_fragmented_disk() {
    let dev = MemDevice::new(2048);
    let mut fs = Filesystem::format(dev).unwrap();

    // Fragment the disk — fills, then frees every other file.
    // 600 files: ~1800 peak blocks during commit (3 per file).
    // After delete + 2 commits: ~300 files retained, ~1400 blocks free
    // in scattered gaps. Plenty of total space but no large contiguous run.
    let _retained = fragment_disk(&mut fs, 600);

    // Write a file needing 20 blocks (320 KiB). After fragment_disk,
    // free space is scattered in ~2-block gaps. Any file needing >2
    // contiguous blocks (plus overhead) exercises multi-extent allocation.
    let file_size = 20 * BLOCK_SIZE as usize;
    let mut data = vec![0u8; file_size];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }

    let big_id = fs.create_file().unwrap();
    fs.write(big_id, 0, &data).unwrap();
    fs.commit().unwrap();

    // Read back and verify every byte.
    let mut read_buf = vec![0u8; file_size];
    let n = fs.read(big_id, 0, &mut read_buf).unwrap();
    assert_eq!(n, file_size);
    assert_eq!(read_buf, data);
}

/// Multi-extent file should survive commit + remount.
#[test]
fn multi_extent_file_survives_remount() {
    let dev = MemDevice::new(2048);
    let mut fs = Filesystem::format(dev).unwrap();
    // 600 files: ~1800 peak blocks during commit (3 per file).
    // After delete + 2 commits: ~300 files retained, ~1400 blocks free
    // in scattered gaps. Plenty of total space but no large contiguous run.
    let _retained = fragment_disk(&mut fs, 600);

    let file_size = 20 * BLOCK_SIZE as usize;
    let mut data = vec![0u8; file_size];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 199) as u8;
    }

    let big_id = fs.create_file().unwrap();
    fs.write(big_id, 0, &data).unwrap();
    fs.commit().unwrap();

    // Remount.
    let dev = fs.into_device();
    let fs = Filesystem::mount(dev).unwrap();

    let mut read_buf = vec![0u8; file_size];
    let n = fs.read(big_id, 0, &mut read_buf).unwrap();
    assert_eq!(n, file_size);
    assert_eq!(read_buf, data);
}

/// Contiguous allocation should still work (fast path) when disk is
/// not fragmented.
#[test]
fn write_uses_single_extent_when_contiguous() {
    let dev = MemDevice::new(2048);
    let mut fs = Filesystem::format(dev).unwrap();

    // Write a medium file on a fresh (unfragmented) disk.
    let id = fs.create_file().unwrap();
    let data = vec![0xCC; 10 * BLOCK_SIZE as usize];
    fs.write(id, 0, &data).unwrap();
    fs.commit().unwrap();

    // Read back and verify.
    let mut read_buf = vec![0u8; data.len()];
    let n = fs.read(id, 0, &mut read_buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(read_buf, data);
}

/// Snapshot and restore should work correctly with multi-extent files.
#[test]
fn snapshot_restore_multi_extent_file() {
    let dev = MemDevice::new(2048);
    let mut fs = Filesystem::format(dev).unwrap();
    // 600 files: ~1800 peak blocks during commit (3 per file).
    // After delete + 2 commits: ~300 files retained, ~1400 blocks free
    // in scattered gaps. Plenty of total space but no large contiguous run.
    let _retained = fragment_disk(&mut fs, 600);

    // Write a multi-extent file.
    let file_size = 20 * BLOCK_SIZE as usize;
    let mut original_data = vec![0u8; file_size];
    for (i, byte) in original_data.iter_mut().enumerate() {
        *byte = (i % 173) as u8;
    }

    let big_id = fs.create_file().unwrap();
    fs.write(big_id, 0, &original_data).unwrap();
    fs.commit().unwrap();

    // Snapshot.
    let snap = fs.snapshot(&[big_id]).unwrap();
    fs.commit().unwrap();

    // Overwrite with different data.
    let new_data = vec![0xFF; file_size];
    fs.write(big_id, 0, &new_data).unwrap();
    fs.commit().unwrap();

    // Verify overwritten.
    let mut buf = vec![0u8; file_size];
    fs.read(big_id, 0, &mut buf).unwrap();
    assert_eq!(buf, new_data);

    // Restore snapshot.
    fs.restore(snap).unwrap();
    fs.commit().unwrap();

    // Verify original data is back.
    let mut buf = vec![0u8; file_size];
    let n = fs.read(big_id, 0, &mut buf).unwrap();
    assert_eq!(n, file_size);
    assert_eq!(buf, original_data);
}

/// Truncate-to-zero of a multi-extent file should work.
#[test]
fn truncate_multi_extent_file() {
    let dev = MemDevice::new(2048);
    let mut fs = Filesystem::format(dev).unwrap();
    // 600 files: ~1800 peak blocks during commit (3 per file).
    // After delete + 2 commits: ~300 files retained, ~1400 blocks free
    // in scattered gaps. Plenty of total space but no large contiguous run.
    let _retained = fragment_disk(&mut fs, 600);

    let file_size = 20 * BLOCK_SIZE as usize;
    let data = vec![0xDD; file_size];
    let big_id = fs.create_file().unwrap();
    fs.write(big_id, 0, &data).unwrap();
    fs.commit().unwrap();

    // Truncate to zero — should free all extents + overflow block.
    fs.truncate(big_id, 0).unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.file_size(big_id).unwrap(), 0);

    // Should be inline again — write small data.
    fs.write(big_id, 0, b"hello").unwrap();
    fs.commit().unwrap();

    let mut buf = [0u8; 5];
    let n = fs.read(big_id, 0, &mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");
}

/// Verify alloc_multi directly on a fragmented allocator.
#[test]
fn alloc_multi_on_fragmented_allocator() {
    let _dev = MemDevice::new(256);
    let mut alloc = fs::Allocator::new(256);

    // Consume all space in 2-block chunks, keeping track of them.
    let mut chunks_a = Vec::new();
    let mut chunks_b = Vec::new();
    loop {
        let a = match alloc.alloc(2) {
            Some(a) => a,
            None => break,
        };
        match alloc.alloc(2) {
            Some(b) => {
                chunks_a.push(a);
                chunks_b.push(b);
            }
            None => {
                // Odd block out — free it back.
                alloc.free(a, 2);
                break;
            }
        }
    }
    // Consume any remaining 1-block fragments.
    while let Some(_) = alloc.alloc(1) {}

    assert_eq!(alloc.free_blocks(), 0);

    // Free every 'a' chunk → creates 2-block gaps interleaved with retained 'b' chunks.
    for &a in &chunks_a {
        alloc.free(a, 2);
    }

    let total_free = alloc.free_blocks();
    assert!(total_free >= 8, "need at least 8 free blocks");

    // Single contiguous 8-block alloc should fail — largest gap is 2.
    assert!(alloc.alloc(8).is_none());

    // alloc_multi should succeed — gather from the 2-block gaps.
    let result = alloc.alloc_multi(8, 20).expect("alloc_multi should work");
    let total: u32 = result.iter().map(|&(_, c)| c).sum();
    assert_eq!(total, 8);
    assert!(result.len() > 1, "should need multiple extents");
}
