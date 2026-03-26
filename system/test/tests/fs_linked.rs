//! Tests for the linked-block inode table, list_files(), and set_root()/root().

use fs::{BlockDevice, FileId, FsError, Filesystem, BLOCK_SIZE};

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

#[test]
fn create_more_than_1365_files() {
    // The old single-block inode table held at most 1365 entries.
    // With linked-block chains, we can exceed that.
    let dev = MemDevice::new(8192);
    let mut fs = Filesystem::format(dev).unwrap();

    for _ in 0..1500 {
        fs.create_file().unwrap();
    }
    fs.commit().unwrap();

    // Verify all 1500 files are retrievable.
    assert_eq!(fs.file_count(), 1500);
    for id in 1..=1500u64 {
        assert!(fs.file_exists(id), "file {id} should exist");
    }
}

#[test]
fn linked_inode_table_survives_mount() {
    let dev = MemDevice::new(8192);
    let mut fs = Filesystem::format(dev).unwrap();

    for _ in 0..1500 {
        fs.create_file().unwrap();
    }
    fs.commit().unwrap();

    // Remount from the same device.
    let dev = fs.into_device();
    let fs = Filesystem::mount(dev).unwrap();

    assert_eq!(fs.file_count(), 1500);
    let files = fs.list_files();
    assert_eq!(files.len(), 1500);
}

#[test]
fn list_files_returns_all_created() {
    let dev = MemDevice::new(256);
    let mut fs = Filesystem::format(dev).unwrap();

    let a = fs.create_file().unwrap();
    let b = fs.create_file().unwrap();
    let c = fs.create_file().unwrap();
    fs.commit().unwrap();

    let files = fs.list_files();
    assert_eq!(files.len(), 3);
    let ids: Vec<u64> = files.iter().map(|f| f.0).collect();
    assert!(ids.contains(&a));
    assert!(ids.contains(&b));
    assert!(ids.contains(&c));
}

#[test]
fn list_files_excludes_deleted() {
    let dev = MemDevice::new(256);
    let mut fs = Filesystem::format(dev).unwrap();

    let a = fs.create_file().unwrap();
    let b = fs.create_file().unwrap();
    fs.delete_file(a).unwrap();
    fs.commit().unwrap();

    let files = fs.list_files();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, b);
}

#[test]
fn set_root_and_root_roundtrip() {
    let dev = MemDevice::new(256);
    let mut fs = Filesystem::format(dev).unwrap();

    let file_id = fs.create_file().unwrap();
    fs.set_root(file_id).unwrap();
    fs.commit().unwrap();

    // Remount and verify.
    let dev = fs.into_device();
    let fs = Filesystem::mount(dev).unwrap();
    assert_eq!(fs.root(), Some(FileId(file_id)));
}

#[test]
fn root_none_on_fresh() {
    let dev = MemDevice::new(256);
    let fs = Filesystem::format(dev).unwrap();
    assert_eq!(fs.root(), None);
}

#[test]
fn set_root_nonexistent_fails() {
    let dev = MemDevice::new(256);
    let mut fs = Filesystem::format(dev).unwrap();

    let result = fs.set_root(9999);
    assert!(result.is_err());
    match result.unwrap_err() {
        FsError::NotFound(id) => assert_eq!(id, 9999),
        other => panic!("expected NotFound, got: {other}"),
    }
}
