//! Tests for the document store (metadata layer over fs::Files).

use fs::{BlockDevice, FileId, FsError, Filesystem, BLOCK_SIZE};
use store::{Query, Store, StoreError};

use alloc::string::ToString;

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

use alloc::vec;
use alloc::vec::Vec;

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
    assert_eq!(meta.attributes.get("title").map(|s| s.as_str()), Some("Greeting"));
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
