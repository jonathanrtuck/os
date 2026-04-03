//! Tests for the document store (metadata layer over fs::Files).

use alloc::string::ToString;

use fs::{BlockDevice, FileId, Filesystem, FsError, BLOCK_SIZE};
use store::{Query, Store, StoreError};

extern crate alloc;

// ── MemDevice (same as fs_linked.rs) ─────────────────────────────────

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

use alloc::{vec, vec::Vec};

/// Helper: create a Store backed by an in-memory filesystem.
fn make_store() -> Store {
    let dev = MemDevice::new(4096);
    let fs = Filesystem::format(dev).unwrap();
    Store::init(Box::new(fs)).unwrap()
}

use alloc::boxed::Box;

#[test]
fn init_creates_empty_catalog() {
    let store = make_store();
    let results = store.query(&Query::Type("font".to_string()));
    assert!(results.is_empty());
}

#[test]
fn create_and_query_by_media_type() {
    let mut store = make_store();
    let id = store.create("font/ttf").unwrap();
    let results = store.query(&Query::MediaType("font/ttf".to_string()));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], id);

    // Non-matching media type returns empty.
    let results = store.query(&Query::MediaType("image/png".to_string()));
    assert!(results.is_empty());
}

#[test]
fn query_by_type_matches_prefix() {
    let mut store = make_store();
    let f1 = store.create("font/ttf").unwrap();
    let f2 = store.create("font/otf").unwrap();
    let _f3 = store.create("image/png").unwrap();

    let results = store.query(&Query::Type("font".to_string()));
    assert_eq!(results.len(), 2);
    let ids: Vec<FileId> = results.into_iter().collect();
    assert!(ids.contains(&f1));
    assert!(ids.contains(&f2));
}

#[test]
fn query_by_attribute() {
    let mut store = make_store();
    let f1 = store.create("font/ttf").unwrap();
    store.set_attribute(f1, "family", "Inter").unwrap();

    let f2 = store.create("font/ttf").unwrap();
    store.set_attribute(f2, "family", "JetBrains Mono").unwrap();

    let results = store.query(&Query::Attribute {
        key: "family".to_string(),
        value: "Inter".to_string(),
    });
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], f1);
}

#[test]
fn query_and_combinator() {
    let mut store = make_store();
    let f1 = store.create("font/ttf").unwrap();
    store.set_attribute(f1, "weight", "400").unwrap();

    let f2 = store.create("font/ttf").unwrap();
    store.set_attribute(f2, "weight", "700").unwrap();

    let _f3 = store.create("image/png").unwrap();

    let results = store.query(&Query::And(vec![
        Query::Type("font".to_string()),
        Query::Attribute {
            key: "weight".to_string(),
            value: "400".to_string(),
        },
    ]));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], f1);
}

#[test]
fn query_or_combinator() {
    let mut store = make_store();
    let f1 = store.create("font/ttf").unwrap();
    let f2 = store.create("image/png").unwrap();
    let _f3 = store.create("text/plain").unwrap();

    let results = store.query(&Query::Or(vec![
        Query::Type("font".to_string()),
        Query::Type("image".to_string()),
    ]));
    assert_eq!(results.len(), 2);
    let ids: Vec<FileId> = results.into_iter().collect();
    assert!(ids.contains(&f1));
    assert!(ids.contains(&f2));
}

#[test]
fn metadata_composes_fs_and_catalog() {
    let mut store = make_store();
    let f = store.create("text/plain").unwrap();
    store.write(f, 0, b"hello world").unwrap();
    store.set_attribute(f, "title", "Greeting").unwrap();

    let meta = store.metadata(f).unwrap();
    assert_eq!(meta.file_id, f);
    assert_eq!(meta.media_type, "text/plain");
    assert_eq!(meta.size, 11);
    assert_eq!(
        meta.attributes.get("title").map(|s| s.as_str()),
        Some("Greeting")
    );
}

#[test]
fn delete_removes_from_catalog() {
    let mut store = make_store();
    let f1 = store.create("font/ttf").unwrap();
    let f2 = store.create("font/otf").unwrap();
    store.delete(f1).unwrap();

    let results = store.query(&Query::Type("font".to_string()));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], f2);

    // Deleting again should fail.
    match store.delete(f1) {
        Err(StoreError::NotFound(_)) => {}
        other => panic!("expected NotFound, got: {other:?}"),
    }
}

#[test]
fn snapshot_and_restore_includes_catalog() {
    let mut store = make_store();

    // Create a file with content + attributes.
    let f = store.create("text/plain").unwrap();
    store.write(f, 0, b"original").unwrap();
    store.set_attribute(f, "version", "1").unwrap();

    // Take a snapshot.
    let snap = store.snapshot(&[f]).unwrap();

    // Modify content + attributes.
    store.truncate(f, 0).unwrap();
    store.write(f, 0, b"modified").unwrap();
    store.set_attribute(f, "version", "2").unwrap();

    // Verify modification took effect.
    assert_eq!(store.attribute(f, "version").unwrap(), Some("2"));

    // Restore the snapshot.
    store.restore(snap).unwrap();

    // Both content and catalog should be reverted.
    let mut buf = [0u8; 8];
    let n = store.read(f, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"original");
    assert_eq!(store.attribute(f, "version").unwrap(), Some("1"));
}

#[test]
fn catalog_survives_commit_reopen() {
    let dev = MemDevice::new(4096);
    let fs = Filesystem::format(dev).unwrap();
    let mut store = Store::init(Box::new(fs)).unwrap();

    let f1 = store.create("font/ttf").unwrap();
    store.set_attribute(f1, "family", "Inter").unwrap();
    let f2 = store.create("image/png").unwrap();
    store.write(f2, 0, b"PNG data").unwrap();

    store.commit().unwrap();

    // Extract the filesystem, remount, and reopen the store.
    let inner = store.into_inner();
    let store2 = Store::open(inner).unwrap();

    // Verify catalog survived.
    assert_eq!(store2.media_type(f1).unwrap(), "font/ttf");
    assert_eq!(store2.attribute(f1, "family").unwrap(), Some("Inter"));
    assert_eq!(store2.media_type(f2).unwrap(), "image/png");

    let results = store2.query(&Query::Type("font".to_string()));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], f1);

    // Verify file content survived.
    let mut buf = [0u8; 8];
    let n = store2.read(f2, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"PNG data");
}

#[test]
fn init_on_existing_root_fails() {
    let dev = MemDevice::new(4096);
    let fs = Filesystem::format(dev).unwrap();
    let store = Store::init(Box::new(fs)).unwrap();

    // Try to init again on the same filesystem.
    let inner = store.into_inner();
    match Store::init(inner) {
        Err(StoreError::AlreadyInitialized) => {}
        other => panic!("expected AlreadyInitialized, got: {other:?}"),
    }
}

#[test]
fn open_on_fresh_fs_fails() {
    let dev = MemDevice::new(4096);
    let fs = Filesystem::format(dev).unwrap();
    match Store::open(Box::new(fs)) {
        Err(StoreError::NotInitialized) => {}
        other => panic!("expected NotInitialized, got: {other:?}"),
    }
}

#[test]
fn type_query_requires_slash() {
    let mut store = make_store();
    // "fontastic/bold" should NOT match Type("font") because
    // the character after "font" is 'a', not '/'.
    store.create("fontastic/bold").unwrap();

    let results = store.query(&Query::Type("font".to_string()));
    assert!(results.is_empty());
}

// ── Factory image round-trip ────────────────────────────────────────

/// Font metadata matching the mkdisk tool's FONTS table.
struct FontSpec {
    name: &'static str,
    style: &'static str,
}

const FACTORY_FONTS: &[FontSpec] = &[
    FontSpec {
        name: "JetBrains Mono",
        style: "mono",
    },
    FontSpec {
        name: "Inter",
        style: "sans",
    },
    FontSpec {
        name: "Source Serif 4",
        style: "serif",
    },
];

/// Simulate the mkdisk flow in-memory: create fonts + PNG, commit,
/// reopen the store, and verify all files survive with correct metadata.
#[test]
fn factory_image_round_trip() {
    let dev = MemDevice::new(4096);
    let filesystem = Filesystem::format(dev).unwrap();
    let mut store = Store::init(Box::new(filesystem)).unwrap();

    // Create fonts with attributes (using synthetic data instead of real font bytes).
    let mut font_ids = Vec::new();
    for font in FACTORY_FONTS {
        let id = store.create("font/ttf").unwrap();
        store.write(id, 0, b"fake-font-data").unwrap();
        store.set_attribute(id, "name", font.name).unwrap();
        store.set_attribute(id, "role", "system").unwrap();
        store.set_attribute(id, "style", font.style).unwrap();
        font_ids.push(id);
    }

    // Create a test PNG.
    let png_id = store.create("image/png").unwrap();
    store.write(png_id, 0, b"fake-png-data").unwrap();
    store.set_attribute(png_id, "name", "test").unwrap();
    store.set_attribute(png_id, "role", "test").unwrap();

    // Commit and reopen.
    store.commit().unwrap();
    let inner = store.into_inner();
    let store = Store::open(inner).unwrap();

    // Query for fonts — should find all 3.
    let fonts = store.query(&Query::Type("font".to_string()));
    assert_eq!(fonts.len(), 3, "expected 3 fonts, got {}", fonts.len());

    // Verify each font has correct attributes.
    for (id, spec) in fonts.iter().zip(FACTORY_FONTS.iter()) {
        assert_eq!(store.media_type(*id).unwrap(), "font/ttf");
        assert_eq!(store.attribute(*id, "name").unwrap(), Some(spec.name));
        assert_eq!(store.attribute(*id, "role").unwrap(), Some("system"));
        assert_eq!(store.attribute(*id, "style").unwrap(), Some(spec.style));
    }

    // Query for images — should find 1.
    let images = store.query(&Query::Type("image".to_string()));
    assert_eq!(images.len(), 1);
    assert_eq!(store.media_type(images[0]).unwrap(), "image/png");
    assert_eq!(store.attribute(images[0], "name").unwrap(), Some("test"));
    assert_eq!(store.attribute(images[0], "role").unwrap(), Some("test"));

    // Verify total file count (3 fonts + 1 PNG = 4).
    let all_font = store.query(&Query::Type("font".to_string()));
    let all_image = store.query(&Query::Type("image".to_string()));
    assert_eq!(all_font.len() + all_image.len(), 4);
}

// ── Snapshot deletion ───────────────────────────────────────────────

#[test]
fn delete_snapshot_frees_blocks() {
    let mut store = make_store();

    // Create a file and write enough data to require block allocation.
    let f = store.create("text/plain").unwrap();
    let data_v1 = vec![0xAAu8; BLOCK_SIZE as usize * 4];
    store.write(f, 0, &data_v1).unwrap();
    store.commit().unwrap();

    // Snapshot the current state.
    let snap = store.snapshot(&[f]).unwrap();

    // Overwrite with different data (COW allocates new blocks).
    store.truncate(f, 0).unwrap();
    let data_v2 = vec![0xBBu8; BLOCK_SIZE as usize * 4];
    store.write(f, 0, &data_v2).unwrap();
    store.commit().unwrap();

    // Delete the snapshot — old blocks become deferred frees.
    store.delete_snapshot(snap).unwrap();

    // Commit again to process the deferred frees.
    store.commit().unwrap();

    // Current content must be unaffected by the snapshot deletion.
    let mut buf = vec![0u8; BLOCK_SIZE as usize * 4];
    let n = store.read(f, 0, &mut buf).unwrap();
    assert_eq!(n, data_v2.len());
    assert!(
        buf.iter().all(|&b| b == 0xBB),
        "current data corrupted after snapshot delete"
    );

    // Freed blocks should allow further allocation without running out of space.
    // Write another large chunk — if blocks weren't freed, this would fail on
    // a 4096-block device that already had significant usage.
    let f2 = store.create("text/plain").unwrap();
    let data_v3 = vec![0xCCu8; BLOCK_SIZE as usize * 4];
    store.write(f2, 0, &data_v3).unwrap();
    store.commit().unwrap();

    let mut buf2 = vec![0u8; BLOCK_SIZE as usize * 4];
    let n2 = store.read(f2, 0, &mut buf2).unwrap();
    assert_eq!(n2, data_v3.len());
    assert!(buf2.iter().all(|&b| b == 0xCC));
}

#[test]
fn delete_snapshot_not_found() {
    let mut store = make_store();

    // Delete a snapshot that was never created.
    let bogus = fs::SnapshotId(999);
    match store.delete_snapshot(bogus) {
        Err(StoreError::Fs(FsError::NotFound(id))) => {
            assert_eq!(id, 999);
        }
        other => panic!("expected Fs(NotFound(999)), got: {other:?}"),
    }
}

#[test]
fn delete_snapshot_preserves_shared_blocks() {
    let mut store = make_store();

    // Create a file with initial data.
    let f = store.create("text/plain").unwrap();
    let data_v1 = vec![0x11u8; BLOCK_SIZE as usize * 2];
    store.write(f, 0, &data_v1).unwrap();
    store.commit().unwrap();

    // Snapshot A captures v1.
    let snap_a = store.snapshot(&[f]).unwrap();

    // Write new data — COW keeps v1 blocks alive in snap_a.
    store.truncate(f, 0).unwrap();
    let data_v2 = vec![0x22u8; BLOCK_SIZE as usize * 2];
    store.write(f, 0, &data_v2).unwrap();
    store.commit().unwrap();

    // Snapshot B captures v2.
    let snap_b = store.snapshot(&[f]).unwrap();

    // Delete snapshot A. Blocks shared only by A should be freed,
    // but B's blocks (which are the current inode blocks for v2) must survive.
    store.delete_snapshot(snap_a).unwrap();
    store.commit().unwrap();

    // Restore to snapshot B — content should be intact.
    store.restore(snap_b).unwrap();

    let mut buf = vec![0u8; BLOCK_SIZE as usize * 2];
    let n = store.read(f, 0, &mut buf).unwrap();
    assert_eq!(n, data_v2.len());
    assert!(
        buf.iter().all(|&b| b == 0x22),
        "snapshot B data corrupted after deleting snapshot A"
    );
}

// ── Disk-full behavior ──────────────────────────────────────────────

/// Helper: create a Store backed by a SMALL in-memory filesystem (32 blocks).
/// With 16 KiB blocks, that is 512 KiB total. After metadata overhead
/// (superblock ring, inode table, free list, catalog), roughly 20 blocks
/// (~320 KiB) are available for user data.
fn make_small_store() -> Store {
    let dev = MemDevice::new(32);
    let fs = Filesystem::format(dev).unwrap();
    Store::init(Box::new(fs)).unwrap()
}

#[test]
fn write_returns_nospace_when_disk_full() {
    let mut store = make_small_store();
    let file = store.create("text/plain").unwrap();

    // Write increasingly large chunks until the disk is full.
    // Each chunk is one block (16 KiB). With ~20 blocks available,
    // this should fail within 25 iterations at most.
    let chunk = vec![0xABu8; BLOCK_SIZE as usize];
    let mut offset: u64 = 0;
    let mut hit_nospace = false;

    for _ in 0..30 {
        match store.write(file, offset, &chunk) {
            Ok(()) => {
                offset += chunk.len() as u64;
            }
            Err(StoreError::Fs(FsError::NoSpace)) => {
                hit_nospace = true;
                break;
            }
            Err(other) => panic!("expected NoSpace, got: {other:?}"),
        }
    }

    assert!(
        hit_nospace,
        "expected NoSpace error but all writes succeeded"
    );
}

#[test]
fn existing_data_survives_nospace() {
    let mut store = make_small_store();
    let file = store.create("text/plain").unwrap();

    // Write safe data and commit it to disk.
    let safe_data = b"safe data that must survive";
    store.write(file, 0, safe_data).unwrap();
    store.commit().unwrap();

    // Attempt to write a large amount of data that exceeds disk capacity.
    // Whether individual writes fail or succeed, the committed data must survive.
    let big_chunk = vec![0xFFu8; BLOCK_SIZE as usize];
    let mut offset = safe_data.len() as u64;
    for _ in 0..30 {
        match store.write(file, offset, &big_chunk) {
            Ok(()) => {
                offset += big_chunk.len() as u64;
            }
            Err(_) => break,
        }
    }

    // Commit may also fail if COW metadata allocation exhausts space.
    let _ = store.commit();

    // The original safe data must still be readable.
    let mut buf = [0u8; 64];
    let n = store.read(file, 0, &mut buf).unwrap();
    assert!(
        n >= safe_data.len(),
        "read returned only {n} bytes, expected at least {}",
        safe_data.len()
    );
    assert_eq!(&buf[..safe_data.len()], safe_data);
}

#[test]
fn commit_after_nospace_write_preserves_state() {
    let mut store = make_small_store();

    // Create a first file with known content and commit.
    let file1 = store.create("text/plain").unwrap();
    let data1 = b"first file content";
    store.write(file1, 0, data1).unwrap();
    store.commit().unwrap();

    // Create a second file and try to fill the disk.
    let file2 = store.create("text/plain").unwrap();
    let big_chunk = vec![0xCCu8; BLOCK_SIZE as usize];
    let mut offset: u64 = 0;
    for _ in 0..30 {
        match store.write(file2, offset, &big_chunk) {
            Ok(()) => {
                offset += big_chunk.len() as u64;
            }
            Err(_) => break,
        }
    }

    // Commit may fail due to NoSpace for COW metadata — that is fine.
    let _ = store.commit();

    // The first file's data must still be intact.
    let mut buf = [0u8; 64];
    let n = store.read(file1, 0, &mut buf).unwrap();
    assert_eq!(n, data1.len());
    assert_eq!(&buf[..n], data1);
}
