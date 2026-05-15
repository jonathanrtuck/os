//! Tests for the COW filesystem library.

extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

use crate::{
    alloc_mod::Allocator,
    block::BlockDevice,
    crc32::crc32,
    filesystem::Filesystem,
    inode::{Inode, InodeExtent, INLINE_CAPACITY, MAX_EXTENTS},
    snapshot,
    superblock::{Superblock, DATA_START, RING_SIZE},
    Files, FsError, BLOCK_SIZE,
};

// ── In-memory block device ────────────────────────────────────────

struct RamDisk {
    blocks: Vec<Vec<u8>>,
}

impl RamDisk {
    fn new(block_count: u32) -> Self {
        Self {
            blocks: vec![vec![0u8; BLOCK_SIZE as usize]; block_count as usize],
        }
    }
}

impl BlockDevice for RamDisk {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if buf.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: buf.len(),
            });
        }

        let i = index as usize;

        if i >= self.blocks.len() {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks.len() as u32,
            });
        }

        buf.copy_from_slice(&self.blocks[i]);

        Ok(())
    }

    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        if data.len() != BLOCK_SIZE as usize {
            return Err(FsError::BadBufferSize {
                expected: BLOCK_SIZE,
                actual: data.len(),
            });
        }

        let i = index as usize;

        if i >= self.blocks.len() {
            return Err(FsError::OutOfBounds {
                block: index,
                count: self.blocks.len() as u32,
            });
        }

        self.blocks[i].copy_from_slice(data);

        Ok(())
    }

    fn flush(&mut self) -> Result<(), FsError> {
        Ok(())
    }

    fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }
}

// ── BlockDevice tests ─────────────────────────────────────────────

#[test]
fn ramdisk_read_write_roundtrip() {
    let mut disk = RamDisk::new(32);
    let mut data = vec![0u8; BLOCK_SIZE as usize];

    data[0] = 0xAB;
    data[BLOCK_SIZE as usize - 1] = 0xCD;

    disk.write_block(5, &data).unwrap();

    let mut buf = vec![0u8; BLOCK_SIZE as usize];

    disk.read_block(5, &mut buf).unwrap();

    assert_eq!(buf[0], 0xAB);
    assert_eq!(buf[BLOCK_SIZE as usize - 1], 0xCD);
}

#[test]
fn ramdisk_out_of_bounds() {
    let disk = RamDisk::new(4);
    let mut buf = vec![0u8; BLOCK_SIZE as usize];
    let result = disk.read_block(4, &mut buf);

    assert!(matches!(result, Err(FsError::OutOfBounds { .. })));
}

#[test]
fn ramdisk_bad_buffer_size() {
    let disk = RamDisk::new(4);
    let mut buf = vec![0u8; 100];
    let result = disk.read_block(0, &mut buf);

    assert!(matches!(result, Err(FsError::BadBufferSize { .. })));
}

#[test]
fn ramdisk_block_count() {
    let disk = RamDisk::new(64);

    assert_eq!(disk.block_count(), 64);
}

// ── CRC32 tests ───────────────────────────────────────────────────

#[test]
fn crc32_empty() {
    assert_eq!(crc32(&[]), 0x0000_0000);
}

#[test]
fn crc32_known_vectors() {
    // "123456789" has a well-known CRC32 of 0xCBF43926.
    let data = b"123456789";

    assert_eq!(crc32(data), 0xCBF4_3926);
}

#[test]
fn crc32_single_byte() {
    // CRC32 of a single zero byte.
    let crc = crc32(&[0x00]);

    assert_ne!(crc, 0);
}

#[test]
fn crc32_deterministic() {
    let data = b"hello world";
    let a = crc32(data);
    let b = crc32(data);

    assert_eq!(a, b);
}

#[test]
fn crc32_different_data_different_checksums() {
    let a = crc32(b"foo");
    let b = crc32(b"bar");

    assert_ne!(a, b);
}

// ── Superblock tests ──────────────────────────────────────────────

#[test]
fn superblock_format_and_mount() {
    let mut disk = RamDisk::new(64);
    let sb = Superblock::format(&mut disk, 1000).unwrap();

    assert_eq!(sb.txg, 1);
    assert_eq!(sb.timestamp, 1000);
    assert_eq!(sb.next_file_id, 1);
    assert_eq!(sb.total_blocks, 64);
    assert_eq!(sb.used_blocks, DATA_START);
    assert!(sb.root_file.is_none());

    let mounted = Superblock::mount(&disk).unwrap();

    assert_eq!(mounted.txg, sb.txg);
    assert_eq!(mounted.timestamp, sb.timestamp);
    assert_eq!(mounted.total_blocks, 64);
}

#[test]
fn superblock_commit_increments_txg() {
    let mut disk = RamDisk::new(64);
    let mut sb = Superblock::format(&mut disk, 0).unwrap();

    assert_eq!(sb.txg, 1);

    sb.commit(&mut disk, 2000).unwrap();

    assert_eq!(sb.txg, 2);
    assert_eq!(sb.timestamp, 2000);

    let mounted = Superblock::mount(&disk).unwrap();

    assert_eq!(mounted.txg, 2);
    assert_eq!(mounted.timestamp, 2000);
}

#[test]
fn superblock_ring_wraps() {
    let mut disk = RamDisk::new(64);
    let mut sb = Superblock::format(&mut disk, 0).unwrap();

    // Commit enough times to wrap around the ring.
    for i in 0..RING_SIZE * 2 {
        sb.commit(&mut disk, i as u64 * 1000).unwrap();
    }

    let mounted = Superblock::mount(&disk).unwrap();

    assert_eq!(mounted.txg, sb.txg);
}

#[test]
fn superblock_device_too_small() {
    let mut disk = RamDisk::new(4);
    let result = Superblock::format(&mut disk, 0);

    assert!(matches!(result, Err(FsError::DeviceTooSmall { .. })));
}

#[test]
fn superblock_mount_empty_fails() {
    let disk = RamDisk::new(64);
    let result = Superblock::mount(&disk);

    assert!(matches!(result, Err(FsError::BadMagic)));
}

#[test]
fn superblock_root_file_roundtrip() {
    let mut disk = RamDisk::new(64);
    let mut sb = Superblock::format(&mut disk, 0).unwrap();

    assert!(sb.root_file.is_none());

    sb.root_file = Some(42);

    sb.commit(&mut disk, 100).unwrap();

    let mounted = Superblock::mount(&disk).unwrap();

    assert_eq!(mounted.root_file, Some(42));
}

// ── Allocator tests ───────────────────────────────────────────────

#[test]
fn allocator_new_reports_correct_free() {
    let alloc = Allocator::new(64);

    assert_eq!(alloc.free_blocks(), 64 - DATA_START);
    assert_eq!(alloc.extent_count(), 1);
}

#[test]
fn allocator_alloc_and_free() {
    let mut alloc = Allocator::new(64);
    let initial = alloc.free_blocks();
    let block = alloc.alloc(1).unwrap();

    assert!(block >= DATA_START);
    assert_eq!(alloc.free_blocks(), initial - 1);

    alloc.free(block, 1);

    assert_eq!(alloc.free_blocks(), initial);
}

#[test]
fn allocator_alloc_zero_returns_none() {
    let mut alloc = Allocator::new(64);

    assert!(alloc.alloc(0).is_none());
}

#[test]
fn allocator_alloc_too_large_returns_none() {
    let mut alloc = Allocator::new(64);
    let too_large = alloc.free_blocks() + 1;

    assert!(alloc.alloc(too_large).is_none());
}

#[test]
fn allocator_coalesces_on_free() {
    let mut alloc = Allocator::new(64);
    let a = alloc.alloc(1).unwrap();
    let b = alloc.alloc(1).unwrap();
    // Two allocations reduce free count by 2.
    let after_alloc = alloc.free_blocks();

    // Free in reverse order — should coalesce back to one extent.
    alloc.free(b, 1);
    alloc.free(a, 1);

    assert_eq!(alloc.free_blocks(), after_alloc + 2);
    assert_eq!(alloc.extent_count(), 1);
}

#[test]
fn allocator_persist_and_load() {
    let mut disk = RamDisk::new(128);
    let mut alloc = Allocator::new(128);

    // Allocate a few blocks to make it non-trivial.
    alloc.alloc(3).unwrap();
    alloc.alloc(5).unwrap();

    let expected_free = alloc.free_blocks();
    let expected_extents = alloc.extent_count();
    let block = alloc.persist(&mut disk).unwrap();
    let loaded = Allocator::load(&disk, block).unwrap();

    assert_eq!(loaded.free_blocks(), expected_free - 1); // persist allocs 1
    assert_eq!(loaded.extent_count(), expected_extents);
}

#[test]
fn allocator_multi_contiguous() {
    let mut alloc = Allocator::new(128);
    let result = alloc.alloc_multi(4, 16).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].1, 4);
}

#[test]
fn allocator_multi_fragmented() {
    let mut alloc = Allocator::new(128);
    // Fragment the free list: alloc 10, free 2 from middle, etc.
    let a = alloc.alloc(10).unwrap();
    let b = alloc.alloc(10).unwrap();
    let _c = alloc.alloc(10).unwrap();

    alloc.free(a, 10);
    alloc.free(b, 10);

    // Now there are two free extents of 10 each, then a large tail.
    let result = alloc.alloc_multi(15, 16).unwrap();
    let total: u32 = result.iter().map(|r| r.1).sum();

    assert_eq!(total, 15);
}

// ── Inode tests ───────────────────────────────────────────────────

#[test]
fn inode_create_and_load() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let inode = Inode::create(&mut disk, &mut alloc, 1, 5000).unwrap();

    assert_eq!(inode.file_id, 1);
    assert_eq!(inode.size, 0);
    assert_eq!(inode.created, 5000);
    assert!(inode.is_inline());

    let loaded = Inode::load(&disk, inode.inode_block()).unwrap();

    assert_eq!(loaded.file_id, 1);
    assert_eq!(loaded.size, 0);
    assert_eq!(loaded.created, 5000);
    assert!(loaded.is_inline());
}

#[test]
fn inode_inline_write_and_read() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"hello").unwrap();

    assert_eq!(inode.size, 5);

    let mut buf = [0u8; 5];
    let n = inode.read(0, &mut buf);

    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");
}

#[test]
fn inode_inline_read_all() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"world").unwrap();

    assert_eq!(inode.read_all(), b"world");
}

#[test]
fn inode_inline_write_at_offset() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"hello").unwrap();
    inode.write_inline(5, b" world").unwrap();

    assert_eq!(inode.read_all(), b"hello world");
}

#[test]
fn inode_inline_capacity_limit() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();
    // Writing exactly at capacity should work.
    let data = vec![0xAA; INLINE_CAPACITY];

    inode.write_inline(0, &data).unwrap();

    assert_eq!(inode.size, INLINE_CAPACITY as u64);

    // One byte past capacity should fail.
    let result = inode.write_inline(0, &vec![0xBB; INLINE_CAPACITY + 1]);

    assert!(matches!(result, Err(FsError::NoSpace)));
}

#[test]
fn inode_truncate_inline() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"hello world").unwrap();
    inode.truncate(5).unwrap();

    assert_eq!(inode.size, 5);
    assert_eq!(inode.read_all(), b"hello");
}

#[test]
fn inode_truncate_extend() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"hi").unwrap();
    inode.truncate(5).unwrap();

    assert_eq!(inode.size, 5);

    let data = inode.read_all();

    assert_eq!(&data[..2], b"hi");
    assert_eq!(&data[2..], &[0, 0, 0]);
}

#[test]
fn inode_read_past_end_returns_zero() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"hi").unwrap();

    let mut buf = [0u8; 10];
    let n = inode.read(100, &mut buf);

    assert_eq!(n, 0);
}

#[test]
fn inode_save_and_reload_preserves_data() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"persistent data").unwrap();
    inode.save(&mut disk).unwrap();

    let loaded = Inode::load(&disk, inode.inode_block()).unwrap();

    assert_eq!(loaded.read_all(), b"persistent data");
}

#[test]
fn inode_add_extents() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.transition_to_extents();

    assert!(!inode.is_inline());

    for i in 0..MAX_EXTENTS {
        inode
            .add_extent(InodeExtent {
                start_block: 100 + i as u32,
                count: 1,
                birth_txg: 1,
            })
            .unwrap();
    }

    assert_eq!(inode.extent_count(), MAX_EXTENTS);

    // Overflow extent.
    inode
        .add_extent(InodeExtent {
            start_block: 200,
            count: 1,
            birth_txg: 1,
        })
        .unwrap();

    assert_eq!(inode.extent_count(), MAX_EXTENTS + 1);
}

#[test]
fn inode_transition_to_inline() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.transition_to_extents();

    assert!(!inode.is_inline());

    inode.transition_to_inline();

    assert!(inode.is_inline());
    assert_eq!(inode.size, 0);
}

#[test]
fn inode_cow_save() {
    let mut disk = RamDisk::new(128);
    let mut alloc = Allocator::new(128);
    let mut inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();

    inode.write_inline(0, b"data").unwrap();

    let old_block = inode.inode_block();
    let old = inode.save_cow(&mut disk, &mut alloc).unwrap();

    assert_eq!(old.inode, old_block);
    assert_ne!(inode.inode_block(), old_block);

    let loaded = Inode::load(&disk, inode.inode_block()).unwrap();

    assert_eq!(loaded.read_all(), b"data");
}

#[test]
fn inode_delete_frees_blocks() {
    let mut disk = RamDisk::new(128);
    let mut alloc = Allocator::new(128);
    let before = alloc.free_blocks();
    let inode = Inode::create(&mut disk, &mut alloc, 1, 0).unwrap();
    let after_create = alloc.free_blocks();

    assert_eq!(after_create, before - 1);

    inode.delete(&mut alloc);

    assert_eq!(alloc.free_blocks(), before);
}

// ── Snapshot serialization tests ──────────────────────────────────

#[test]
fn snapshot_serialize_deserialize_empty() {
    let snaps = BTreeMap::new();
    let data = snapshot::serialize(&snaps, 1);
    let (loaded, next_id) = snapshot::deserialize(&data).unwrap();

    assert!(loaded.is_empty());
    assert_eq!(next_id, 1);
}

#[test]
fn snapshot_serialize_deserialize_inline() {
    let mut snaps = BTreeMap::new();
    let mut files = BTreeMap::new();

    files.insert(
        10,
        snapshot::FileSnapshot {
            was_inline: true,
            inline_data: vec![1, 2, 3, 4, 5],
            extents: Vec::new(),
            size: 5,
        },
    );
    snaps.insert(
        1,
        snapshot::Snapshot {
            id: 1,
            txg: 10,
            files,
        },
    );

    let data = snapshot::serialize(&snaps, 2);
    let (loaded, next_id) = snapshot::deserialize(&data).unwrap();

    assert_eq!(next_id, 2);
    assert_eq!(loaded.len(), 1);

    let snap = loaded.get(&1).unwrap();

    assert_eq!(snap.txg, 10);

    let fs = snap.files.get(&10).unwrap();

    assert!(fs.was_inline);
    assert_eq!(fs.inline_data, vec![1, 2, 3, 4, 5]);
    assert_eq!(fs.size, 5);
}

#[test]
fn snapshot_serialize_deserialize_extents() {
    let mut snaps = BTreeMap::new();
    let mut files = BTreeMap::new();

    files.insert(
        20,
        snapshot::FileSnapshot {
            was_inline: false,
            inline_data: Vec::new(),
            extents: vec![
                InodeExtent {
                    start_block: 100,
                    count: 5,
                    birth_txg: 3,
                },
                InodeExtent {
                    start_block: 200,
                    count: 10,
                    birth_txg: 7,
                },
            ],
            size: 245760,
        },
    );
    snaps.insert(
        1,
        snapshot::Snapshot {
            id: 1,
            txg: 5,
            files,
        },
    );

    let data = snapshot::serialize(&snaps, 3);
    let (loaded, next_id) = snapshot::deserialize(&data).unwrap();

    assert_eq!(next_id, 3);

    let fs = loaded.get(&1).unwrap().files.get(&20).unwrap();

    assert!(!fs.was_inline);
    assert_eq!(fs.extents.len(), 2);
    assert_eq!(fs.extents[0].start_block, 100);
    assert_eq!(fs.extents[0].count, 5);
    assert_eq!(fs.extents[0].birth_txg, 3);
    assert_eq!(fs.extents[1].start_block, 200);
    assert_eq!(fs.extents[1].count, 10);
    assert_eq!(fs.extents[1].birth_txg, 7);
    assert_eq!(fs.size, 245760);
}

#[test]
fn snapshot_blob_write_read_roundtrip() {
    let mut disk = RamDisk::new(128);
    let mut alloc = Allocator::new(128);
    let payload = vec![0xAB; 40000]; // Spans multiple blocks.
    let (first, blocks) = snapshot::write_blob(&mut disk, &mut alloc, &payload).unwrap();

    assert!(blocks.len() > 1);

    let (data, read_blocks) = snapshot::read_blob(&disk, first).unwrap();

    assert_eq!(data, payload);
    assert_eq!(read_blocks, blocks);
}

#[test]
fn snapshot_blob_empty() {
    let mut disk = RamDisk::new(64);
    let mut alloc = Allocator::new(64);
    let (first, _) = snapshot::write_blob(&mut disk, &mut alloc, &[]).unwrap();
    let (data, _) = snapshot::read_blob(&disk, first).unwrap();

    assert!(data.is_empty());
}

// ── Filesystem integration tests ──────────────────────────────────

fn make_fs(blocks: u32) -> Filesystem<RamDisk> {
    Filesystem::format(RamDisk::new(blocks)).unwrap()
}

#[test]
fn fs_create_and_read_empty() {
    let mut fs = make_fs(128);
    let fid = fs.create_file().unwrap();

    assert_eq!(fs.file_size(fid).unwrap(), 0);

    let mut buf = [0u8; 10];
    let n = fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(n, 0);
}

#[test]
fn fs_write_and_read_inline() {
    let mut fs = make_fs(128);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"hello fs").unwrap();

    let mut buf = [0u8; 8];
    let n = fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(n, 8);
    assert_eq!(&buf, b"hello fs");
}

#[test]
fn fs_write_at_offset() {
    let mut fs = make_fs(128);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"hello").unwrap();
    fs.write(fid, 5, b" world").unwrap();

    let mut buf = [0u8; 11];
    let n = fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(n, 11);
    assert_eq!(&buf, b"hello world");
}

#[test]
fn fs_truncate_inline() {
    let mut fs = make_fs(128);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"hello world").unwrap();
    fs.truncate(fid, 5).unwrap();

    assert_eq!(fs.file_size(fid).unwrap(), 5);

    let mut buf = [0u8; 5];

    fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(&buf, b"hello");
}

#[test]
fn fs_delete_file() {
    let mut fs = make_fs(128);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"data").unwrap();
    fs.delete_file(fid).unwrap();

    assert!(!fs.file_exists(fid));
}

#[test]
fn fs_delete_nonexistent_fails() {
    let mut fs = make_fs(128);
    let result = fs.delete_file(999);

    assert!(matches!(result, Err(FsError::NotFound(999))));
}

#[test]
fn fs_commit_and_remount() {
    let mut fs = make_fs(256);
    let fid = fs.create_file().unwrap();
    fs.write(fid, 0, b"persistent").unwrap();

    fs.commit().unwrap();

    let device = fs.into_device();
    let fs2 = Filesystem::mount(device).unwrap();
    let mut buf = [0u8; 10];
    let n = fs2.read(fid, 0, &mut buf).unwrap();

    assert_eq!(n, 10);
    assert_eq!(&buf, b"persistent");
}

#[test]
fn fs_multiple_files() {
    let mut fs = make_fs(256);
    let a = fs.create_file().unwrap();
    let b = fs.create_file().unwrap();

    fs.write(a, 0, b"file a").unwrap();
    fs.write(b, 0, b"file b").unwrap();

    let mut buf = [0u8; 6];

    fs.read(a, 0, &mut buf).unwrap();

    assert_eq!(&buf, b"file a");

    fs.read(b, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"file b");
}

#[test]
fn fs_list_files() {
    let mut fs = make_fs(256);
    let a = fs.create_file().unwrap();
    let b = fs.create_file().unwrap();
    let files = fs.list_files();
    let ids: Vec<u64> = files.iter().map(|f| f.0).collect();

    assert!(ids.contains(&a));
    assert!(ids.contains(&b));
}

#[test]
fn fs_set_and_get_root() {
    let mut fs = make_fs(128);

    assert!(fs.root().is_none());

    let fid = fs.create_file().unwrap();

    fs.set_root(fid).unwrap();

    assert_eq!(fs.root().unwrap().0, fid);
}

#[test]
fn fs_set_root_nonexistent_fails() {
    let mut fs = make_fs(128);
    let result = fs.set_root(999);

    assert!(matches!(result, Err(FsError::NotFound(999))));
}

#[test]
fn fs_file_metadata() {
    let mut fs = make_fs(128);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"hello").unwrap();

    let meta = fs.file_metadata(fid).unwrap();

    assert_eq!(meta.file_id.0, fid);
    assert_eq!(meta.size, 5);
}

// ── Extent-based (large file) tests ───────────────────────────────

#[test]
fn fs_large_file_write_and_read() {
    let mut fs = make_fs(512);
    let fid = fs.create_file().unwrap();
    let data = vec![0x42; INLINE_CAPACITY + 100];

    fs.write(fid, 0, &data).unwrap();

    assert_eq!(fs.file_size(fid).unwrap(), data.len() as u64);

    let mut buf = vec![0u8; data.len()];
    let n = fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(n, data.len());
    assert_eq!(buf, data);
}

#[test]
fn fs_large_file_read_at_offset() {
    let mut fs = make_fs(512);
    let fid = fs.create_file().unwrap();
    let mut data = vec![0u8; INLINE_CAPACITY + 1000];

    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }

    fs.write(fid, 0, &data).unwrap();

    let offset = 1000u64;
    let len = 500;
    let mut buf = vec![0u8; len];
    let n = fs.read(fid, offset, &mut buf).unwrap();

    assert_eq!(n, len);
    assert_eq!(&buf, &data[offset as usize..offset as usize + len]);
}

#[test]
fn fs_large_file_truncate_to_zero() {
    let mut fs = make_fs(512);
    let fid = fs.create_file().unwrap();
    let data = vec![0xAA; INLINE_CAPACITY + 100];

    fs.write(fid, 0, &data).unwrap();
    fs.truncate(fid, 0).unwrap();

    assert_eq!(fs.file_size(fid).unwrap(), 0);

    let mut buf = [0u8; 1];
    let n = fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(n, 0);
}

// ── Snapshot tests ────────────────────────────────────────────────

#[test]
fn fs_snapshot_and_restore_inline() {
    let mut fs = make_fs(256);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"version 1").unwrap();

    let snap_id = fs.snapshot(&[fid]).unwrap();

    fs.write(fid, 0, b"version 2").unwrap();

    let mut buf = [0u8; 9];

    fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(&buf, b"version 2");

    fs.restore(snap_id).unwrap();
    fs.read(fid, 0, &mut buf).unwrap();

    assert_eq!(&buf, b"version 1");
}

#[test]
fn fs_snapshot_list() {
    let mut fs = make_fs(256);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"data").unwrap();

    let s1 = fs.snapshot(&[fid]).unwrap();
    let s2 = fs.snapshot(&[fid]).unwrap();
    let snaps = fs.list_snapshots(fid);

    assert!(snaps.contains(&s1));
    assert!(snaps.contains(&s2));
}

#[test]
fn fs_delete_snapshot() {
    let mut fs = make_fs(256);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"data").unwrap();

    let snap_id = fs.snapshot(&[fid]).unwrap();

    fs.delete_snapshot(snap_id).unwrap();

    let snaps = fs.list_snapshots(fid);

    assert!(!snaps.contains(&snap_id));
}

#[test]
fn fs_snapshot_persists_across_commit() {
    let mut fs = make_fs(512);
    let fid = fs.create_file().unwrap();

    fs.write(fid, 0, b"original").unwrap();

    let snap_id = fs.snapshot(&[fid]).unwrap();

    fs.write(fid, 0, b"modified").unwrap();
    fs.commit().unwrap();

    let device = fs.into_device();
    let mut fs2 = Filesystem::mount(device).unwrap();

    fs2.restore(snap_id).unwrap();

    let mut buf = [0u8; 8];

    fs2.read(fid, 0, &mut buf).unwrap();

    assert_eq!(&buf, b"original");
}

// ── Files trait tests ─────────────────────────────────────────────

#[test]
fn files_trait_create_write_read() {
    let mut fs: Box<dyn Files> = Box::new(make_fs(256));
    let file = fs.create().unwrap();

    fs.write(file, 0, b"trait test").unwrap();

    let mut buf = [0u8; 10];
    let n = fs.read(file, 0, &mut buf).unwrap();

    assert_eq!(n, 10);
    assert_eq!(&buf, b"trait test");
}

#[test]
fn files_trait_snapshot_restore() {
    let mut fs: Box<dyn Files> = Box::new(make_fs(256));
    let file = fs.create().unwrap();

    fs.write(file, 0, b"v1").unwrap();

    let snap = fs.snapshot(&[file]).unwrap();

    fs.write(file, 0, b"v2").unwrap();
    fs.restore(snap).unwrap();

    let mut buf = [0u8; 2];

    fs.read(file, 0, &mut buf).unwrap();

    assert_eq!(&buf, b"v1");
}

// ── FsError Display tests ────────────────────────────────────────

#[test]
fn error_display_coverage() {
    use alloc::format;

    let errors = [
        FsError::OutOfBounds { block: 5, count: 4 },
        FsError::Io,
        FsError::BadBufferSize {
            expected: 16384,
            actual: 100,
        },
        FsError::BadMagic,
        FsError::ChecksumMismatch {
            expected: 0xDEAD,
            actual: 0xBEEF,
        },
        FsError::NoValidSuperblock,
        FsError::DeviceTooSmall {
            blocks: 4,
            minimum: 17,
        },
        FsError::NoSpace,
        FsError::Corrupt("test".into()),
        FsError::NotFound(42),
    ];

    for err in &errors {
        let s = format!("{err}");

        assert!(!s.is_empty());
    }
}
