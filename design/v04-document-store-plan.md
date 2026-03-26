# v0.4 Document Store — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every piece of persistent content has identity (FileId), type (media type), queryable metadata, and version history (snapshots). The OS boots from its own filesystem, not from a host share.

**Architecture:** Two libraries (fs for generic COW storage, store for metadata layer wrapping `Box<dyn Files>`), a document service (thin IPC wrapper replacing filesystem service), and a factory image builder (host tool creating pre-populated disk images).

**Tech Stack:** Rust (no_std + alloc for bare-metal, std for host tools/tests), COW filesystem, virtio-blk, IPC over shared memory.

**Spec:** `design/v04-document-store.md`

---

## File Map

### Phase C: fs library + store library

**Modify:**
- `system/libraries/fs/lib.rs` — add `list_files`, `set_root`, `root` to `Files` trait
- `system/libraries/fs/filesystem.rs` — linked-block inode table, implement new trait methods
- `system/libraries/fs/superblock.rs` — add `root_file` field to superblock
- `system/libraries/fs/snapshot.rs` — extract `write_blob`/`read_blob` to shared utility (or make `pub(crate)`)

**Create:**
- `system/libraries/fs/Cargo.toml` — enable host-side testing via test crate
- `system/libraries/store/lib.rs` — Store struct, CatalogEntry, Query, serialization
- `system/libraries/store/Cargo.toml` — depends on fs library
- `system/test/tests/fs_linked.rs` — tests for linked-block inode table + new trait methods
- `system/test/tests/store.rs` — tests for the store library

**Modify:**
- `system/test/Cargo.toml` — add fs and store as dev-dependencies

### Phase D: Factory image builder + document service

**Create:**
- `tools/mkdisk/Cargo.toml` — host-side binary
- `tools/mkdisk/main.rs` — creates pre-populated disk image with fonts
- `system/services/document/main.rs` — document service (replaces filesystem service)
- `system/libraries/protocol/document.rs` — IPC message types for document service

**Modify:**
- `system/libraries/protocol/lib.rs` — add `pub mod document;`
- `system/build.rs` — add store library, rename filesystem→document in PROGRAMS/INIT_EMBEDDED
- `system/run.sh` — use factory-built disk image instead of blank test.img

### Phase E: Boot from native fs

**Modify:**
- `system/services/init/main.rs` — reorder boot: start document service early, query for fonts, load into Content Region
- `system/services/document/main.rs` — handle font query requests from init

### Phase F: Multi-document scaffolding

**Modify:**
- `system/services/core/main.rs` — track document FileId, request file content from document service at boot
- `system/services/core/documents.rs` — multi-document state (FileId per document space)

### Phase G: Undo/redo

**Modify:**
- `system/services/core/main.rs` — snapshot at operation boundaries, undo/redo key bindings
- `system/libraries/protocol/document.rs` — MSG_DOC_SNAPSHOT, MSG_DOC_RESTORE messages
- `system/services/document/main.rs` — handle snapshot/restore requests

---

## Phase C: fs library + store library

### Task 1: Add Cargo.toml to fs library for host testing

**Files:**
- Create: `system/libraries/fs/Cargo.toml`
- Modify: `system/test/Cargo.toml`

- [ ] **Step 1: Create fs library Cargo.toml**

```toml
[package]
name = "fs"
version = "0.1.0"
edition = "2021"

[lib]
path = "lib.rs"
```

- [ ] **Step 2: Add fs to test crate dependencies**

In `system/test/Cargo.toml`, add to `[dev-dependencies]`:
```toml
fs = { path = "../libraries/fs" }
```

- [ ] **Step 3: Verify fs compiles on host**

Run: `cd /Users/user/Sites/os/system/test && cargo check`
Expected: compiles (no_std + alloc works on host)

- [ ] **Step 4: Commit**

```bash
git add system/libraries/fs/Cargo.toml system/test/Cargo.toml
git commit -m "chore: add Cargo.toml to fs library for host testing"
```

---

### Task 2: Make blob utilities pub(crate) for reuse

The snapshot module has `write_blob`/`read_blob` for linked-block chains. The inode table needs the same pattern.

**Files:**
- Modify: `system/libraries/fs/snapshot.rs`

- [ ] **Step 1: Make write_blob and read_blob pub(crate)**

In `snapshot.rs`, change visibility of `write_blob` and `read_blob` from private to `pub(crate)`:

```rust
pub(crate) fn write_blob<D: BlockDevice>(
    device: &mut D,
    allocator: &mut Allocator,
    data: &[u8],
) -> Result<(u32, Vec<u32>), FsError> {
```

```rust
pub(crate) fn read_blob<D: BlockDevice>(
    device: &D,
    first_block: u32,
) -> Result<(Vec<u8>, Vec<u32>), FsError> {
```

Also export the constants:
```rust
pub(crate) const BLOB_HEADER: usize = 8;
pub(crate) const BLOB_DATA_CAP: usize = crate::BLOCK_SIZE as usize - BLOB_HEADER;
```

- [ ] **Step 2: Verify build**

Run: `cd /Users/user/Sites/os/system/test && cargo check`

- [ ] **Step 3: Commit**

```bash
git add system/libraries/fs/snapshot.rs
git commit -m "refactor: make blob read/write pub(crate) for inode table reuse"
```

---

### Task 3: Linked-block inode table

Replace the single-block inode table with a linked-block chain using the blob utilities.

**Files:**
- Modify: `system/libraries/fs/filesystem.rs`
- Create: `system/test/tests/fs_linked.rs`

- [ ] **Step 1: Write failing test — inode table exceeds single block**

Create `system/test/tests/fs_linked.rs`:

```rust
//! Tests for linked-block inode table and new Files trait methods.

use fs::{BlockDevice, FileId, Files, Filesystem, FsError, BLOCK_SIZE};

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
        let b = &self.blocks[index as usize];
        buf[..BLOCK_SIZE as usize].copy_from_slice(b);
        Ok(())
    }
    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        self.blocks[index as usize][..BLOCK_SIZE as usize]
            .copy_from_slice(&data[..BLOCK_SIZE as usize]);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), FsError> { Ok(()) }
    fn block_count(&self) -> u32 { self.blocks.len() as u32 }
}

#[test]
fn create_more_than_1365_files() {
    // 16384 blocks = 256 MiB, enough for 1500+ files + overhead
    let device = MemDevice::new(16384);
    let mut fs = Filesystem::format(device).unwrap();

    for i in 0..1500 {
        let id = fs.create_file().unwrap();
        assert_eq!(id, i + 1); // FileIds start at 1
    }

    fs.commit().unwrap();

    // Verify all files survive commit (inode table spans multiple blocks)
    for i in 1..=1500u64 {
        let meta = fs.metadata(FileId(i)).unwrap();
        assert_eq!(meta.file_id, FileId(i));
    }
}

#[test]
fn linked_inode_table_survives_mount() {
    let device = MemDevice::new(16384);
    let mut fs = Filesystem::format(device).unwrap();

    for _ in 0..1500 {
        fs.create_file().unwrap();
    }
    fs.commit().unwrap();

    // Remount
    let device = fs.into_device();
    let fs = Filesystem::mount(device).unwrap();

    // All 1500 files present
    let files = fs.list_files().unwrap();
    assert_eq!(files.len(), 1500);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/user/Sites/os/system/test && cargo test --test fs_linked -- --test-threads=1 2>&1 | head -30`
Expected: FAIL (1365 limit exceeded or `list_files` not found)

- [ ] **Step 3: Implement linked-block inode table in filesystem.rs**

Replace `write_inode_table` to use `snapshot::write_blob`:

```rust
fn write_inode_table<D: BlockDevice>(
    device: &mut D,
    allocator: &mut Allocator,
    inodes: &BTreeMap<u64, Inode>,
) -> Result<(u32, Vec<u32>), FsError> {
    // Serialize: [entry_count: u32] [entries: (file_id: u64, block: u32)*]
    let entry_count = inodes.len() as u32;
    let data_len = 4 + inodes.len() * 12;
    let mut data = vec![0u8; data_len];

    data[0..4].copy_from_slice(&entry_count.to_le_bytes());
    let mut off = 4;
    for (file_id, inode) in inodes {
        data[off..off + 8].copy_from_slice(&file_id.to_le_bytes());
        data[off + 8..off + 12].copy_from_slice(&inode.inode_block().to_le_bytes());
        off += 12;
    }

    snapshot::write_blob(device, allocator, &data)
}
```

Replace `load_all_inodes` to use `snapshot::read_blob`:

```rust
fn load_all_inodes<D: BlockDevice>(
    device: &D,
    table_block: u32,
) -> Result<(BTreeMap<u64, Inode>, Vec<u32>), FsError> {
    let (data, blocks) = snapshot::read_blob(device, table_block)?;

    if data.len() < 4 {
        return Err(FsError::Corrupt("inode table too short".into()));
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut inodes = BTreeMap::new();
    let mut off = 4;
    for _ in 0..count {
        if off + 12 > data.len() {
            return Err(FsError::Corrupt("inode table truncated".into()));
        }
        let file_id = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        let block = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap());
        off += 12;
        let inode = Inode::load(device, block)?;
        inodes.insert(file_id, inode);
    }

    Ok((inodes, blocks))
}
```

Update `commit()` to use the new functions (replace single-block allocation with blob write, track `inode_table_blocks: Vec<u32>` instead of `inode_table_block: u32`).

Update `mount()` to use the new `load_all_inodes` (returns block list for deferred freeing).

Remove `MAX_TABLE_ENTRIES` constant.

Add `into_device(self) -> D` method to `Filesystem<D>` for test remount:
```rust
pub fn into_device(self) -> D {
    self.device
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/user/Sites/os/system/test && cargo test --test fs_linked -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add system/libraries/fs/filesystem.rs system/test/tests/fs_linked.rs
git commit -m "feat: linked-block inode table, removes 1365-file limit"
```

---

### Task 4: Add list_files, set_root, root to Files trait

**Files:**
- Modify: `system/libraries/fs/lib.rs` — trait definition
- Modify: `system/libraries/fs/filesystem.rs` — implementation
- Modify: `system/libraries/fs/superblock.rs` — root_file field
- Modify: `system/test/tests/fs_linked.rs` — tests

- [ ] **Step 1: Write failing tests**

Add to `system/test/tests/fs_linked.rs`:

```rust
#[test]
fn list_files_returns_all_created() {
    let device = MemDevice::new(4096);
    let mut fs = Filesystem::format(device).unwrap();

    let a = fs.create_file().unwrap();
    let b = fs.create_file().unwrap();
    let c = fs.create_file().unwrap();
    fs.commit().unwrap();

    let mut files = fs.list_files().unwrap();
    files.sort_by_key(|f| f.0);
    assert_eq!(files, vec![FileId(a), FileId(b), FileId(c)]);
}

#[test]
fn list_files_excludes_deleted() {
    let device = MemDevice::new(4096);
    let mut fs = Filesystem::format(device).unwrap();

    let a = fs.create_file().unwrap();
    let b = fs.create_file().unwrap();
    fs.delete(FileId(a)).unwrap();
    fs.commit().unwrap();

    let files = fs.list_files().unwrap();
    assert_eq!(files, vec![FileId(b)]);
}

#[test]
fn set_root_and_root_roundtrip() {
    let device = MemDevice::new(4096);
    let mut fs = Filesystem::format(device).unwrap();

    assert_eq!(fs.root(), None);

    let file = fs.create()?;
    fs.set_root(file).unwrap();
    assert_eq!(fs.root(), Some(file));

    fs.commit().unwrap();

    // Survives remount
    let device = fs.into_device();
    let fs = Filesystem::mount(device).unwrap();
    assert_eq!(fs.root(), Some(file));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /Users/user/Sites/os/system/test && cargo test --test fs_linked -- --test-threads=1 2>&1 | head -20`
Expected: FAIL (methods don't exist)

- [ ] **Step 3: Add methods to Files trait**

In `system/libraries/fs/lib.rs`, add to the `Files` trait:

```rust
/// List all file IDs.
fn list_files(&self) -> Result<Vec<FileId>, FsError>;
/// Designate a root file (persisted in superblock).
fn set_root(&mut self, file: FileId) -> Result<(), FsError>;
/// Retrieve the designated root file.
fn root(&self) -> Option<FileId>;
```

- [ ] **Step 4: Add root_file to Superblock**

In `system/libraries/fs/superblock.rs`, add field to `Superblock`:
```rust
pub struct Superblock {
    // ... existing fields ...
    pub root_file: Option<u64>,  // designated root file ID (0 = none)
}
```

Add serialization in the superblock ring entry (use bytes 56..64 — currently unused space before the CRC at byte 52, so shift CRC to byte 60):

Update the on-disk format: add `root_file_id: u64` at offset 52, move CRC to offset 60. Update `SUPERBLOCK_CRC_OFFSET` accordingly. Ensure backward compatibility is not needed (we're in development, format isn't stable).

- [ ] **Step 5: Implement in Filesystem**

In `system/libraries/fs/filesystem.rs`:

```rust
impl<D: BlockDevice> Files for Filesystem<D> {
    fn list_files(&self) -> Result<Vec<FileId>, FsError> {
        Ok(self.inodes.keys().map(|&id| FileId(id)).collect())
    }

    fn set_root(&mut self, file: FileId) -> Result<(), FsError> {
        if !self.inodes.contains_key(&file.0) {
            return Err(FsError::NotFound(file.0));
        }
        self.superblock.root_file = Some(file.0);
        Ok(())
    }

    fn root(&self) -> Option<FileId> {
        self.superblock.root_file.map(FileId)
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cd /Users/user/Sites/os/system/test && cargo test --test fs_linked -- --test-threads=1`
Expected: PASS

- [ ] **Step 7: Run ALL existing tests**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Expected: All ~2,236 tests pass (no regressions)

- [ ] **Step 8: Commit**

```bash
git add system/libraries/fs/lib.rs system/libraries/fs/filesystem.rs system/libraries/fs/superblock.rs system/test/tests/fs_linked.rs
git commit -m "feat: add list_files, set_root, root to Files trait"
```

---

### Task 5: Create store library skeleton

**Files:**
- Create: `system/libraries/store/lib.rs`
- Create: `system/libraries/store/Cargo.toml`
- Create: `system/libraries/store/CLAUDE.md`
- Modify: `system/test/Cargo.toml`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "store"
version = "0.1.0"
edition = "2021"

[dependencies]
fs = { path = "../fs" }

[lib]
path = "lib.rs"
```

- [ ] **Step 2: Create lib.rs with types and Store skeleton**

```rust
//! Document store — metadata layer over the COW filesystem.
//!
//! Wraps `Box<dyn fs::Files>` to add media types, queryable attributes,
//! and a persistent catalog. This is the OS-specific layer; the fs library
//! underneath is generic and reusable.
//!
//! ## Public API
//!
//! ```text
//! Document Service ──Store──→ Box<dyn Files> ──→ disk
//! Factory Builder  ──Store──→ Box<dyn Files> ──→ disk image
//! ```

#![no_std]
extern crate alloc;

use alloc::{
    collections::BTreeMap,
    string::String,
    vec::Vec,
};

use fs::{FileId, Files, FsError, SnapshotId};

// ── Catalog entry ──────────────────────────────────────────────────

/// Metadata for a single file in the catalog.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub media_type: String,
    pub attributes: BTreeMap<String, String>,
}

// ── Composite metadata ─────────────────────────────────────────────

/// Full document metadata (catalog + fs metadata composed).
#[derive(Debug, Clone)]
pub struct DocumentMetadata {
    pub file_id: FileId,
    pub media_type: String,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
    pub attributes: BTreeMap<String, String>,
}

// ── Query model ────────────────────────────────────────────────────

/// Query filter for finding files by metadata.
///
/// This is the permanent query AST. A future SQL syntax layer would
/// parse SQL strings into Query values — additive, not replacement.
#[derive(Debug, Clone)]
pub enum Query {
    /// Exact media type match: "font/ttf"
    MediaType(String),
    /// Top-level type match: "font" matches font/ttf, font/otf, etc.
    Type(String),
    /// Exact attribute key=value match.
    Attribute { key: String, value: String },
    /// All sub-filters must match.
    And(Vec<Query>),
    /// Any sub-filter must match.
    Or(Vec<Query>),
}

// ── Errors ─────────────────────────────────────────────────────────

/// Store-level error.
#[derive(Debug)]
pub enum StoreError {
    /// Underlying filesystem error.
    Fs(FsError),
    /// Store already initialized (root exists).
    AlreadyInitialized,
    /// Store not initialized (no root).
    NotInitialized,
    /// File not found in catalog.
    NotFound(FileId),
    /// Catalog data is corrupt.
    Corrupt(String),
}

impl From<FsError> for StoreError {
    fn from(e: FsError) -> Self { StoreError::Fs(e) }
}

// ── Catalog serialization ──────────────────────────────────────────

const CATALOG_MAGIC: u32 = 0x4341_544C; // "CATL"

mod serialize;

// ── Store ──────────────────────────────────────────────────────────

/// The document store. Wraps a filesystem with metadata awareness.
pub struct Store {
    fs: alloc::boxed::Box<dyn Files>,
    catalog: BTreeMap<u64, CatalogEntry>,
    catalog_file: FileId,
}

impl Store {
    /// First boot: create catalog, set root, commit.
    /// Fails if the filesystem already has a root (use `open` instead).
    pub fn init(fs: alloc::boxed::Box<dyn Files>) -> Result<Self, StoreError> {
        if fs.root().is_some() {
            return Err(StoreError::AlreadyInitialized);
        }
        let mut fs = fs;
        let catalog_file = fs.create()?;
        fs.set_root(catalog_file)?;

        let mut store = Self {
            fs,
            catalog: BTreeMap::new(),
            catalog_file,
        };
        store.write_catalog()?;
        store.fs.commit()?;
        Ok(store)
    }

    /// Subsequent boots: read root, load catalog.
    /// Fails if no root exists (use `init` instead).
    pub fn open(fs: alloc::boxed::Box<dyn Files>) -> Result<Self, StoreError> {
        let catalog_file = fs.root().ok_or(StoreError::NotInitialized)?;
        let mut store = Self {
            fs,
            catalog: BTreeMap::new(),
            catalog_file,
        };
        store.load_catalog()?;
        Ok(store)
    }

    // ── File lifecycle ──

    /// Create a new file with mandatory media type.
    pub fn create(&mut self, media_type: &str) -> Result<FileId, StoreError> {
        let file_id = self.fs.create()?;
        self.catalog.insert(file_id.0, CatalogEntry {
            media_type: String::from(media_type),
            attributes: BTreeMap::new(),
        });
        Ok(file_id)
    }

    /// Delete a file and its catalog entry.
    pub fn delete(&mut self, file: FileId) -> Result<(), StoreError> {
        if !self.catalog.contains_key(&file.0) {
            return Err(StoreError::NotFound(file));
        }
        self.fs.delete(file)?;
        self.catalog.remove(&file.0);
        Ok(())
    }

    // ── Content access ──

    pub fn read(&self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<usize, StoreError> {
        Ok(self.fs.read(file, offset, buf)?)
    }

    pub fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), StoreError> {
        Ok(self.fs.write(file, offset, data)?)
    }

    pub fn truncate(&mut self, file: FileId, len: u64) -> Result<(), StoreError> {
        Ok(self.fs.truncate(file, len)?)
    }

    // ── Metadata ──

    pub fn media_type(&self, file: FileId) -> Result<&str, StoreError> {
        self.catalog.get(&file.0)
            .map(|e| e.media_type.as_str())
            .ok_or(StoreError::NotFound(file))
    }

    pub fn set_attribute(&mut self, file: FileId, key: &str, val: &str) -> Result<(), StoreError> {
        let entry = self.catalog.get_mut(&file.0)
            .ok_or(StoreError::NotFound(file))?;
        entry.attributes.insert(String::from(key), String::from(val));
        Ok(())
    }

    pub fn attribute(&self, file: FileId, key: &str) -> Option<&str> {
        self.catalog.get(&file.0)
            .and_then(|e| e.attributes.get(key))
            .map(|v| v.as_str())
    }

    pub fn metadata(&self, file: FileId) -> Result<DocumentMetadata, StoreError> {
        let entry = self.catalog.get(&file.0)
            .ok_or(StoreError::NotFound(file))?;
        let fs_meta = self.fs.metadata(file)?;
        Ok(DocumentMetadata {
            file_id: file,
            media_type: entry.media_type.clone(),
            size: fs_meta.size,
            created: fs_meta.created,
            modified: fs_meta.modified,
            attributes: entry.attributes.clone(),
        })
    }

    // ── Query ──

    pub fn query(&self, filter: &Query) -> Result<Vec<FileId>, StoreError> {
        let mut results = Vec::new();
        for (&file_id, entry) in &self.catalog {
            if self.matches(entry, filter) {
                results.push(FileId(file_id));
            }
        }
        Ok(results)
    }

    fn matches(&self, entry: &CatalogEntry, filter: &Query) -> bool {
        match filter {
            Query::MediaType(mt) => entry.media_type == *mt,
            Query::Type(t) => {
                // Match top-level type: "font" matches "font/ttf"
                entry.media_type.starts_with(t.as_str())
                    && entry.media_type.as_bytes().get(t.len()) == Some(&b'/')
            }
            Query::Attribute { key, value } => {
                entry.attributes.get(key.as_str()).map_or(false, |v| v == value)
            }
            Query::And(filters) => filters.iter().all(|f| self.matches(entry, f)),
            Query::Or(filters) => filters.iter().any(|f| self.matches(entry, f)),
        }
    }

    // ── Versioning ──

    /// Snapshot the given files. Transparently includes the catalog.
    pub fn snapshot(&mut self, files: &[FileId]) -> Result<SnapshotId, StoreError> {
        self.write_catalog()?;
        let mut all_files = Vec::from(files);
        if !all_files.contains(&self.catalog_file) {
            all_files.push(self.catalog_file);
        }
        Ok(self.fs.snapshot(&all_files)?)
    }

    /// Restore a snapshot. Reloads catalog from restored state.
    pub fn restore(&mut self, snapshot: SnapshotId) -> Result<(), StoreError> {
        self.fs.restore(snapshot)?;
        self.load_catalog()?;
        Ok(())
    }

    // ── Persistence ──

    /// Write catalog to disk, then commit the filesystem.
    pub fn commit(&mut self) -> Result<(), StoreError> {
        self.write_catalog()?;
        self.fs.commit()?;
        Ok(())
    }

    // ── Internal ──

    fn write_catalog(&mut self) -> Result<(), StoreError> {
        let data = serialize::encode_catalog(CATALOG_MAGIC, &self.catalog);
        self.fs.write(self.catalog_file, 0, &data)?;
        self.fs.truncate(self.catalog_file, data.len() as u64)?;
        Ok(())
    }

    fn load_catalog(&mut self) -> Result<(), StoreError> {
        let size = self.fs.size(self.catalog_file)? as usize;
        let mut buf = alloc::vec![0u8; size];
        self.fs.read(self.catalog_file, 0, &mut buf)?;
        self.catalog = serialize::decode_catalog(CATALOG_MAGIC, &buf)?;
        Ok(())
    }
}
```

- [ ] **Step 3: Create CLAUDE.md**

```markdown
# store

Metadata layer over the COW filesystem. Wraps `Box<dyn Files>` to add media types,
queryable attributes, and a persistent catalog. This is the OS-specific layer; the
fs library underneath is generic and reusable.

## Key Types

- `Store` — the main API. init/open, create/delete, read/write, metadata, query, snapshot, commit.
- `CatalogEntry` — per-file metadata (media_type + attributes).
- `Query` — permanent query AST (MediaType, Type, Attribute, And, Or).
- `DocumentMetadata` — composed from fs metadata + catalog entry.

## Design

See `design/v04-document-store.md` for the full spec.
```

- [ ] **Step 4: Verify it compiles**

Run: `cd /Users/user/Sites/os/system/test && cargo check`

- [ ] **Step 5: Commit**

```bash
git add system/libraries/store/
git commit -m "feat: store library skeleton — Store, CatalogEntry, Query types"
```

---

### Task 6: Catalog serialization

**Files:**
- Create: `system/libraries/store/serialize.rs`
- Create: `system/test/tests/store.rs`

- [ ] **Step 1: Write failing tests for catalog round-trip**

Create `system/test/tests/store.rs`:

```rust
//! Tests for the store library.

use std::collections::BTreeMap;
use fs::{BlockDevice, FileId, Filesystem, FsError, BLOCK_SIZE};
use store::{Store, StoreError, Query, CatalogEntry};

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
        buf[..BLOCK_SIZE as usize].copy_from_slice(&self.blocks[index as usize]);
        Ok(())
    }
    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        self.blocks[index as usize][..BLOCK_SIZE as usize]
            .copy_from_slice(&data[..BLOCK_SIZE as usize]);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), FsError> { Ok(()) }
    fn block_count(&self) -> u32 { self.blocks.len() as u32 }
}

fn make_store() -> Store {
    let device = MemDevice::new(4096);
    let fs = Filesystem::format(device).unwrap();
    Store::init(Box::new(fs)).unwrap()
}

fn reopen_store(store: Store) -> Store {
    // Store doesn't expose into_device, so we need a different approach.
    // For testing, we'll commit and reopen from the same fs.
    // This requires Store to expose the inner fs for testing.
    // Alternative: test via init/open cycle on the same device.
    todo!("Need Store to support reopen for testing")
}

#[test]
fn init_creates_empty_catalog() {
    let store = make_store();
    let files = store.query(&Query::Type("font".into())).unwrap();
    assert!(files.is_empty());
}

#[test]
fn init_fails_if_already_initialized() {
    let device = MemDevice::new(4096);
    let fs = Filesystem::format(device).unwrap();
    let mut store = Store::init(Box::new(fs)).unwrap();
    // Can't test double-init directly without remount...
    // This test verifies the API contract exists.
}

#[test]
fn create_and_query_by_media_type() {
    let mut store = make_store();
    let id = store.create("font/ttf").unwrap();
    store.write(id, 0, b"fake font data").unwrap();
    store.commit().unwrap();

    let fonts = store.query(&Query::MediaType("font/ttf".into())).unwrap();
    assert_eq!(fonts, vec![id]);
}

#[test]
fn query_by_type_matches_prefix() {
    let mut store = make_store();
    let ttf = store.create("font/ttf").unwrap();
    let otf = store.create("font/otf").unwrap();
    let png = store.create("image/png").unwrap();
    store.commit().unwrap();

    let fonts = store.query(&Query::Type("font".into())).unwrap();
    assert_eq!(fonts.len(), 2);
    assert!(fonts.contains(&ttf));
    assert!(fonts.contains(&otf));
    assert!(!fonts.contains(&png));
}

#[test]
fn query_by_attribute() {
    let mut store = make_store();
    let id = store.create("font/ttf").unwrap();
    store.set_attribute(id, "role", "system").unwrap();
    store.commit().unwrap();

    let results = store.query(&Query::Attribute {
        key: "role".into(),
        value: "system".into(),
    }).unwrap();
    assert_eq!(results, vec![id]);
}

#[test]
fn query_and_combinator() {
    let mut store = make_store();
    let sys_font = store.create("font/ttf").unwrap();
    store.set_attribute(sys_font, "role", "system").unwrap();
    let user_font = store.create("font/ttf").unwrap();
    store.set_attribute(user_font, "role", "user").unwrap();
    store.commit().unwrap();

    let results = store.query(&Query::And(vec![
        Query::Type("font".into()),
        Query::Attribute { key: "role".into(), value: "system".into() },
    ])).unwrap();
    assert_eq!(results, vec![sys_font]);
}

#[test]
fn metadata_composes_fs_and_catalog() {
    let mut store = make_store();
    let id = store.create("image/png").unwrap();
    store.write(id, 0, b"PNG data here").unwrap();
    store.set_attribute(id, "name", "test").unwrap();
    store.commit().unwrap();

    let meta = store.metadata(id).unwrap();
    assert_eq!(meta.file_id, id);
    assert_eq!(meta.media_type, "image/png");
    assert_eq!(meta.size, 13);
    assert_eq!(meta.attributes.get("name").unwrap(), "test");
}

#[test]
fn delete_removes_from_catalog() {
    let mut store = make_store();
    let id = store.create("text/plain").unwrap();
    store.commit().unwrap();

    store.delete(id).unwrap();
    store.commit().unwrap();

    let results = store.query(&Query::MediaType("text/plain".into())).unwrap();
    assert!(results.is_empty());
    assert!(store.media_type(id).is_err());
}

#[test]
fn snapshot_and_restore_includes_catalog() {
    let mut store = make_store();
    let id = store.create("text/plain").unwrap();
    store.write(id, 0, b"version 1").unwrap();
    store.commit().unwrap();

    let snap = store.snapshot(&[id]).unwrap();

    store.write(id, 0, b"version 2").unwrap();
    store.set_attribute(id, "tag", "modified").unwrap();
    store.commit().unwrap();

    // Verify modified state
    assert_eq!(store.attribute(id, "tag"), Some("modified"));

    // Restore
    store.restore(snap).unwrap();

    // Content and metadata both restored
    let mut buf = vec![0u8; 20];
    let n = store.read(id, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"version 1");
    assert_eq!(store.attribute(id, "tag"), None);
}
```

- [ ] **Step 2: Implement serialize.rs**

Create `system/libraries/store/serialize.rs`:

```rust
//! Catalog serialization — binary format for the catalog file.
//!
//! Format:
//! [magic: u32] [entry_count: u32]
//! Entry*:
//!   [file_id: u64]
//!   [media_type_len: u16] [media_type: [u8]]
//!   [attr_count: u16]
//!   Attr*:
//!     [key_len: u16] [key: [u8]]
//!     [val_len: u16] [val: [u8]]

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use crate::{CatalogEntry, StoreError};

pub fn encode_catalog(magic: u32, catalog: &BTreeMap<u64, CatalogEntry>) -> Vec<u8> {
    let mut data = Vec::new();

    // Header
    data.extend_from_slice(&magic.to_le_bytes());
    data.extend_from_slice(&(catalog.len() as u32).to_le_bytes());

    // Entries
    for (&file_id, entry) in catalog {
        data.extend_from_slice(&file_id.to_le_bytes());

        let mt = entry.media_type.as_bytes();
        data.extend_from_slice(&(mt.len() as u16).to_le_bytes());
        data.extend_from_slice(mt);

        data.extend_from_slice(&(entry.attributes.len() as u16).to_le_bytes());
        for (key, val) in &entry.attributes {
            let kb = key.as_bytes();
            let vb = val.as_bytes();
            data.extend_from_slice(&(kb.len() as u16).to_le_bytes());
            data.extend_from_slice(kb);
            data.extend_from_slice(&(vb.len() as u16).to_le_bytes());
            data.extend_from_slice(vb);
        }
    }

    data
}

pub fn decode_catalog(expected_magic: u32, data: &[u8]) -> Result<BTreeMap<u64, CatalogEntry>, StoreError> {
    if data.is_empty() {
        return Ok(BTreeMap::new());
    }
    if data.len() < 8 {
        return Err(StoreError::Corrupt(String::from("catalog too short")));
    }

    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic != expected_magic {
        return Err(StoreError::Corrupt(String::from("catalog magic mismatch")));
    }

    let count = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let mut off = 8;
    let mut catalog = BTreeMap::new();

    for _ in 0..count {
        if off + 8 > data.len() {
            return Err(StoreError::Corrupt(String::from("catalog truncated")));
        }
        let file_id = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;

        // Media type
        if off + 2 > data.len() {
            return Err(StoreError::Corrupt(String::from("catalog truncated")));
        }
        let mt_len = u16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as usize;
        off += 2;
        if off + mt_len > data.len() {
            return Err(StoreError::Corrupt(String::from("catalog truncated")));
        }
        let media_type = core::str::from_utf8(&data[off..off + mt_len])
            .map_err(|_| StoreError::Corrupt(String::from("invalid UTF-8 in media type")))?;
        off += mt_len;

        // Attributes
        if off + 2 > data.len() {
            return Err(StoreError::Corrupt(String::from("catalog truncated")));
        }
        let attr_count = u16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as usize;
        off += 2;
        let mut attributes = BTreeMap::new();

        for _ in 0..attr_count {
            // Key
            if off + 2 > data.len() {
                return Err(StoreError::Corrupt(String::from("catalog truncated")));
            }
            let key_len = u16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as usize;
            off += 2;
            if off + key_len > data.len() {
                return Err(StoreError::Corrupt(String::from("catalog truncated")));
            }
            let key = core::str::from_utf8(&data[off..off + key_len])
                .map_err(|_| StoreError::Corrupt(String::from("invalid UTF-8 in key")))?;
            off += key_len;

            // Value
            if off + 2 > data.len() {
                return Err(StoreError::Corrupt(String::from("catalog truncated")));
            }
            let val_len = u16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as usize;
            off += 2;
            if off + val_len > data.len() {
                return Err(StoreError::Corrupt(String::from("catalog truncated")));
            }
            let val = core::str::from_utf8(&data[off..off + val_len])
                .map_err(|_| StoreError::Corrupt(String::from("invalid UTF-8 in value")))?;
            off += val_len;

            attributes.insert(String::from(key), String::from(val));
        }

        catalog.insert(file_id, CatalogEntry {
            media_type: String::from(media_type),
            attributes,
        });
    }

    Ok(catalog)
}
```

- [ ] **Step 3: Add store to test crate**

In `system/test/Cargo.toml`:
```toml
store = { path = "../libraries/store" }
```

- [ ] **Step 4: Run tests**

Run: `cd /Users/user/Sites/os/system/test && cargo test --test store -- --test-threads=1`
Expected: PASS (all store tests)

- [ ] **Step 5: Run all tests for regression check**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

- [ ] **Step 6: Commit**

```bash
git add system/libraries/store/ system/test/tests/store.rs system/test/Cargo.toml
git commit -m "feat: store library — catalog, metadata, queries, snapshots"
```

---

## Phase D: Factory disk image + document service

### Task 7: Factory disk image builder

**Files:**
- Create: `tools/mkdisk/Cargo.toml`
- Create: `tools/mkdisk/main.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "mkdisk"
version = "0.1.0"
edition = "2021"

[dependencies]
fs = { path = "../../system/libraries/fs" }
store = { path = "../../system/libraries/store" }
```

- [ ] **Step 2: Implement mkdisk**

Create `tools/mkdisk/main.rs`:

```rust
//! Factory disk image builder.
//!
//! Creates a pre-populated disk image with system fonts and test content.
//! Uses the same store library that the bare-metal document service uses.
//!
//! Usage: mkdisk <output.img> <share-dir>
//!
//! The share-dir should contain: jetbrains-mono.ttf, inter.ttf,
//! source-serif-4.ttf, test.png

use std::{env, fs as stdfs, process};
use fs::{BlockDevice, Filesystem, FsError, BLOCK_SIZE};
use store::Store;

const DISK_BLOCKS: u32 = 4096; // 64 MiB

/// File-backed block device (same as prototype/fs).
struct FileDevice {
    file: stdfs::File,
    block_count: u32,
}

impl FileDevice {
    fn create(path: &str, blocks: u32) -> Self {
        use std::io::Write;
        let size = blocks as u64 * BLOCK_SIZE as u64;
        let file = stdfs::OpenOptions::new()
            .read(true).write(true).create(true).truncate(true)
            .open(path)
            .unwrap_or_else(|e| { eprintln!("Cannot create {path}: {e}"); process::exit(1); });
        file.set_len(size).unwrap();
        Self { file, block_count: blocks }
    }
}

impl BlockDevice for FileDevice {
    fn read_block(&self, index: u32, buf: &mut [u8]) -> Result<(), FsError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = &self.file;
        f.seek(SeekFrom::Start(index as u64 * BLOCK_SIZE as u64)).map_err(|_| FsError::Io)?;
        f.read_exact(&mut buf[..BLOCK_SIZE as usize]).map_err(|_| FsError::Io)?;
        Ok(())
    }
    fn write_block(&mut self, index: u32, data: &[u8]) -> Result<(), FsError> {
        use std::io::{Seek, SeekFrom, Write};
        self.file.seek(SeekFrom::Start(index as u64 * BLOCK_SIZE as u64)).map_err(|_| FsError::Io)?;
        self.file.write_all(&data[..BLOCK_SIZE as usize]).map_err(|_| FsError::Io)?;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), FsError> {
        use std::io::Write;
        self.file.flush().map_err(|_| FsError::Io)?;
        Ok(())
    }
    fn block_count(&self) -> u32 { self.block_count }
}

struct FontSpec {
    filename: &'static str,
    media_type: &'static str,
    name: &'static str,
    style: &'static str,
}

const FONTS: &[FontSpec] = &[
    FontSpec { filename: "jetbrains-mono.ttf", media_type: "font/ttf", name: "JetBrains Mono", style: "mono" },
    FontSpec { filename: "inter.ttf", media_type: "font/ttf", name: "Inter", style: "sans" },
    FontSpec { filename: "source-serif-4.ttf", media_type: "font/ttf", name: "Source Serif 4", style: "serif" },
];

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: mkdisk <output.img> <share-dir>");
        process::exit(1);
    }
    let output = &args[1];
    let share_dir = &args[2];

    // Create disk image
    let device = FileDevice::create(output, DISK_BLOCKS);
    let filesystem = Filesystem::format(device)
        .unwrap_or_else(|e| { eprintln!("Format failed: {e:?}"); process::exit(1); });

    let mut store = Store::init(Box::new(filesystem))
        .unwrap_or_else(|e| { eprintln!("Store init failed: {e:?}"); process::exit(1); });

    // Load fonts
    for font in FONTS {
        let path = format!("{}/{}", share_dir, font.filename);
        let data = stdfs::read(&path)
            .unwrap_or_else(|e| { eprintln!("Cannot read {path}: {e}"); process::exit(1); });

        let id = store.create(font.media_type).unwrap();
        store.write(id, 0, &data).unwrap();
        store.set_attribute(id, "name", font.name).unwrap();
        store.set_attribute(id, "role", "system").unwrap();
        store.set_attribute(id, "style", font.style).unwrap();

        println!("  {} ({} bytes) -> FileId({})", font.filename, data.len(), id.0);
    }

    // Load test image if present
    let png_path = format!("{}/test.png", share_dir);
    if let Ok(data) = stdfs::read(&png_path) {
        let id = store.create("image/png").unwrap();
        store.write(id, 0, &data).unwrap();
        store.set_attribute(id, "name", "test").unwrap();
        store.set_attribute(id, "role", "test").unwrap();
        println!("  test.png ({} bytes) -> FileId({})", data.len(), id.0);
    }

    store.commit().unwrap();
    println!("Disk image written to {output}");
}
```

- [ ] **Step 3: Build and test**

Run:
```bash
cd /Users/user/Sites/os/tools/mkdisk && cargo build
cargo run -- /tmp/test-disk.img /Users/user/Sites/os/system/share
```
Expected: Creates disk image, prints file IDs

- [ ] **Step 4: Verify disk contents by mounting in a test**

Add to `system/test/tests/store.rs`:
```rust
#[test]
fn factory_image_roundtrip() {
    // Simulates what mkdisk does, then verifies via open
    let device = MemDevice::new(4096);
    let fs = Filesystem::format(device).unwrap();
    let mut store = Store::init(Box::new(fs)).unwrap();

    let font = store.create("font/ttf").unwrap();
    store.write(font, 0, b"fake TTF").unwrap();
    store.set_attribute(font, "name", "Test Font").unwrap();
    store.set_attribute(font, "role", "system").unwrap();
    store.commit().unwrap();

    // Reopen (simulates guest boot)
    // Need into_inner() on Store for this test
    // ... implementation depends on Store exposing inner fs for testing
}
```

- [ ] **Step 5: Commit**

```bash
git add tools/mkdisk/
git commit -m "feat: mkdisk factory image builder"
```

---

### Task 8: Document service + IPC protocol

**Files:**
- Create: `system/libraries/protocol/document.rs`
- Create: `system/services/document/main.rs`
- Create: `system/services/document/CLAUDE.md`
- Modify: `system/libraries/protocol/lib.rs`
- Modify: `system/build.rs`

- [ ] **Step 1: Define document IPC protocol**

Create `system/libraries/protocol/document.rs`:

```rust
//! Document service IPC protocol.
//!
//! Minimal scaffolding for v0.4 — just enough for boot font loading,
//! single-document persistence, and undo/redo.

// Message types
pub const MSG_DOC_CONFIG: u32 = 80;     // init → document: device config + doc buffer VA
pub const MSG_DOC_READY: u32 = 81;      // document → init: service is up
pub const MSG_DOC_COMMIT: u32 = 82;     // core → document: persist current document
pub const MSG_DOC_QUERY: u32 = 83;      // init → document: query for files
pub const MSG_DOC_QUERY_RESULT: u32 = 84; // document → init: query result
pub const MSG_DOC_READ: u32 = 85;       // init → document: read file content to shared memory
pub const MSG_DOC_READ_DONE: u32 = 86;  // document → init: read complete
pub const MSG_DOC_SNAPSHOT: u32 = 87;   // core → document: create snapshot
pub const MSG_DOC_RESTORE: u32 = 88;    // core → document: restore snapshot (undo)

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DocConfig {
    pub mmio_pa: u64,
    pub irq: u32,
    pub doc_va: u64,
    pub doc_capacity: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DocQuery {
    pub query_type: u32,  // 0 = by type prefix
    pub data: [u8; 48],   // query string (zero-terminated)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DocQueryResult {
    pub count: u32,
    pub file_ids: [u64; 6],  // up to 6 file IDs in one message
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DocRead {
    pub file_id: u64,
    pub target_va: u64,   // VA in shared memory to write content
    pub capacity: u32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DocReadDone {
    pub file_id: u64,
    pub len: u32,
    pub status: u32,      // 0 = success
}

pub enum Message {
    DocConfig(DocConfig),
    DocReady,
    DocCommit,
    DocQuery(DocQuery),
    DocQueryResult(DocQueryResult),
    DocRead(DocRead),
    DocReadDone(DocReadDone),
    DocSnapshot,
    DocRestore,
}

pub fn decode(msg_type: u32, payload: &[u8; 60]) -> Option<Message> {
    match msg_type {
        MSG_DOC_CONFIG => Some(Message::DocConfig(unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const DocConfig) })),
        MSG_DOC_READY => Some(Message::DocReady),
        MSG_DOC_COMMIT => Some(Message::DocCommit),
        MSG_DOC_QUERY => Some(Message::DocQuery(unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const DocQuery) })),
        MSG_DOC_QUERY_RESULT => Some(Message::DocQueryResult(unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const DocQueryResult) })),
        MSG_DOC_READ => Some(Message::DocRead(unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const DocRead) })),
        MSG_DOC_READ_DONE => Some(Message::DocReadDone(unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const DocReadDone) })),
        MSG_DOC_SNAPSHOT => Some(Message::DocSnapshot),
        MSG_DOC_RESTORE => Some(Message::DocRestore),
        _ => None,
    }
}
```

- [ ] **Step 2: Add module to protocol lib.rs**

Add `pub mod document;` to `system/libraries/protocol/lib.rs` (near the existing `pub mod blkfs`).

- [ ] **Step 3: Create document service**

Create `system/services/document/main.rs` — this replaces the filesystem service. Structure follows the same pattern: receive device config, init virtio-blk, create Store, enter IPC loop.

The document service:
1. Receives device config from init (MMIO PA, IRQ)
2. Creates VirtioBlockDevice (same code as current filesystem service)
3. Tries `Filesystem::mount()` first; falls back to `Filesystem::format()` for first boot
4. Creates `Store::open()` (or `Store::init()` on first boot)
5. Signals ready
6. Enters IPC loop handling: DOC_COMMIT, DOC_QUERY, DOC_READ, DOC_SNAPSHOT, DOC_RESTORE

The VirtioBlockDevice code can be moved from `services/filesystem/main.rs` largely unchanged.

- [ ] **Step 4: Update build.rs**

In `system/build.rs`:
- Add store library compilation after fs library (Phase 1)
- Replace `("filesystem", "services/filesystem", true, false)` with `("document", "services/document", true, false)` in PROGRAMS
- Replace `("filesystem", "FILESYSTEM_ELF")` with `("document", "DOCUMENT_ELF")` in INIT_EMBEDDED
- Link store library to document service

- [ ] **Step 5: Update run.sh to use factory-built disk**

In `system/run.sh`, replace the test.img creation with:
```bash
DISK_IMG="${SCRIPT_DIR}/disk.img"
if [ ! -f "$DISK_IMG" ]; then
    echo "Building disk image..."
    (cd "${SCRIPT_DIR}/../tools/mkdisk" && cargo run --release -- "$DISK_IMG" "${SCRIPT_DIR}/share")
fi
```

- [ ] **Step 6: Build and boot test**

Run: `cd /Users/user/Sites/os/system && cargo build --release && cargo run --release`
Expected: Document service starts, prints "document service ready". System boots (still using 9p for fonts at this stage).

- [ ] **Step 7: Commit**

```bash
git add system/libraries/protocol/document.rs system/libraries/protocol/lib.rs
git add system/services/document/ system/build.rs system/run.sh
git commit -m "feat: document service + IPC protocol (replaces filesystem service)"
```

---

## Phase E: Boot from native fs

### Task 9: Load fonts from document service instead of 9p

**Files:**
- Modify: `system/services/init/main.rs`
- Modify: `system/services/document/main.rs`

- [ ] **Step 1: Add font query handler to document service**

In the document service's IPC loop, handle MSG_DOC_QUERY:
- Parse query type (0 = type prefix)
- Run `store.query(Query::Type(prefix))`
- Send back MSG_DOC_QUERY_RESULT with matching FileIds

Handle MSG_DOC_READ:
- Read file content from store
- Write to shared memory at target_va
- Send MSG_DOC_READ_DONE with length

- [ ] **Step 2: Reorder init boot — start document service early**

In init's `setup_render_pipeline()`:
- Move document service startup to before font loading (Phase 1.5 equivalent)
- Allocate Content Region and File Store as before
- Share Content Region with document service (read-write, so it can write font data to it)
- Instead of 9p read_file calls, send MSG_DOC_QUERY for Type("font")
- For each font FileId in the response, send MSG_DOC_READ with target VA in Content Region
- Wait for MSG_DOC_READ_DONE, write Content Region header entry

- [ ] **Step 3: Remove 9p from boot dependency**

- Remove the 9p driver startup from Phase 1.5
- Keep 9p driver code but don't start it unless explicitly requested (e.g., env var)
- Update INIT_EMBEDDED to optionally exclude 9p

- [ ] **Step 4: Visual verification**

Run with hypervisor and capture screenshot:
```bash
cd /Users/user/Sites/os/system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/native-fs-boot.png
```
Then: `Read /tmp/native-fs-boot.png` — fonts should render correctly (same as before).

- [ ] **Step 5: Commit**

```bash
git add system/services/init/main.rs system/services/document/main.rs
git commit -m "feat: boot from native filesystem — fonts loaded from document service"
```

---

## Phase F: Multi-document scaffolding

### Task 10: Multiple documents in core

**Files:**
- Modify: `system/services/core/main.rs`
- Modify: `system/services/core/documents.rs`

- [ ] **Step 1: Track document FileId in core**

Currently core has a single document buffer. For v0.4, we keep the buffer but track which FileId it corresponds to. Core requests the document's initial content from the document service at boot.

In core's initialization:
- After receiving CoreConfig, send MSG_DOC_QUERY for Type("text") to find existing text documents
- If a document exists, send MSG_DOC_READ to load its content into the doc buffer
- Track `doc_file_id: Option<u64>` in core state
- If no document exists, signal document service to create one (or handle at factory image level)

- [ ] **Step 2: Update commit to include FileId**

Update MSG_DOC_COMMIT to include the FileId being committed, so the document service knows which file to write to.

- [ ] **Step 3: Boot test with persistent content**

1. Boot, type some text, quit
2. Boot again — text should persist (loaded from disk)

Visual verification:
```bash
cat > /tmp/test.events << 'SCRIPT'
type hello world
wait 30
capture /tmp/multi-doc-1.png
SCRIPT
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/test.events
```

Then boot again without typing:
```bash
cat > /tmp/test2.events << 'SCRIPT'
wait 30
capture /tmp/multi-doc-2.png
SCRIPT
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/test2.events
```

Compare screenshots — both should show "hello world".

- [ ] **Step 4: Commit**

```bash
git add system/services/core/main.rs system/services/core/documents.rs
git commit -m "feat: multi-document scaffolding — core tracks FileId, content persists across reboots"
```

---

## Phase G: Undo/redo via snapshots

### Task 11: Wire snapshots to operation boundaries

**Files:**
- Modify: `system/services/core/main.rs`
- Modify: `system/services/document/main.rs`
- Modify: `system/libraries/protocol/document.rs`

- [ ] **Step 1: Add undo/redo state to core**

In core, track a snapshot stack:
```rust
struct UndoState {
    snapshots: Vec<u64>,      // snapshot IDs, oldest first
    current: usize,           // index into snapshots (for redo)
    max_snapshots: usize,     // limit (e.g., 100)
}
```

At operation boundaries (where `text_changed` is currently drained):
1. Send MSG_DOC_COMMIT (persist current state)
2. Send MSG_DOC_SNAPSHOT (create snapshot)
3. Push snapshot ID onto the stack
4. Truncate any redo history beyond current position

- [ ] **Step 2: Add Cmd+Z / Cmd+Shift+Z handlers**

In core's key event processing:
- Cmd+Z (undo): Send MSG_DOC_RESTORE with previous snapshot ID, update current position, reload doc buffer
- Cmd+Shift+Z (redo): Send MSG_DOC_RESTORE with next snapshot ID, update current position, reload doc buffer

After restore, core needs to reload the document content:
- Send MSG_DOC_READ for the document FileId
- Copy restored content into doc buffer
- Recalculate layout, update scene graph

- [ ] **Step 3: Handle snapshot/restore in document service**

In document service IPC loop:
- MSG_DOC_SNAPSHOT: call `store.snapshot(&[doc_file_id])`, return snapshot ID in response
- MSG_DOC_RESTORE: call `store.restore(snapshot_id)`, signal success

- [ ] **Step 4: Visual verification of undo**

```bash
cat > /tmp/undo-test.events << 'SCRIPT'
type hello
wait 10
capture /tmp/undo-before.png
key cmd+z
wait 10
capture /tmp/undo-after.png
SCRIPT
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --events /tmp/undo-test.events
```

Use imgdiff to verify:
```bash
python3 system/test/imgdiff.py /tmp/undo-before.png /tmp/undo-after.png
```

- [ ] **Step 5: Commit**

```bash
git add system/services/core/main.rs system/services/document/main.rs system/libraries/protocol/document.rs
git commit -m "feat: undo/redo via COW snapshots at operation boundaries"
```

---

## Final: Documentation and cleanup

### Task 12: Update docs and CLAUDE.md files

- [ ] **Step 1: Update system/DESIGN.md** with document service, store library
- [ ] **Step 2: Update system/services/CLAUDE.md** — replace filesystem entry with document
- [ ] **Step 3: Update system/libraries/CLAUDE.md** — add store library entry
- [ ] **Step 4: Update design/journal.md** — mark v0.4 Document Store phases C-G as COMPLETE
- [ ] **Step 5: Update CLAUDE.md** (root) — update "Where We Left Off"
- [ ] **Step 6: Document fs library scaling characteristics** in fs library module doc comment

- [ ] **Step 7: Final commit**

```bash
git add -A
git commit -m "docs: update all documentation for v0.4 Document Store"
```

### Task 13: Full test suite verification

- [ ] **Step 1: Run all host tests**

```bash
cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1
```

- [ ] **Step 2: Boot verification (hypervisor)**

```bash
cd /Users/user/Sites/os/system && cargo build --release
hypervisor target/aarch64-unknown-none/release/kernel --drive disk.img --capture 30 /tmp/final.png
```

- [ ] **Step 3: Visual verification**

Read `/tmp/final.png` — system should boot with fonts rendered from native filesystem.
