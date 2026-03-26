# v0.4: The Document Store

Design spec for the v0.4 sprint. After this sprint, every piece of persistent content in the OS has identity (FileId), type (media type), queryable metadata, and version history (snapshots).

## Sprint scope

Five phases, foundation-up:

| Phase | What                                        | ~Days |
| ----- | ------------------------------------------- | ----- |
| C     | fs library changes + store library (new)    | 2     |
| D     | Factory disk image builder + minimal IPC    | 2     |
| E     | Boot from native fs (replace 9p dependency) | 1     |
| F     | Multi-document scaffolding                  | 1.5   |
| G     | Undo/redo via snapshots                     | 1.5   |

Phases A (host prototype) and B (bare-metal integration) are already complete.

## Architecture

```text
Core ──[IPC]──→ Document Service ──→ store library ──→ fs library ──→ disk
                 (services/document/)   (libraries/store/)  (libraries/fs/)
```

Services are **translation layers** between core and subsystems that core shouldn't know about. The document service translates document operations into disk I/O. Core never touches the filesystem, the block device, or the catalog. The IPC boundary is the translation boundary.

### Component roles

| Component             | Path                 | Role                                                                                                                                                                 |
| --------------------- | -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| fs library            | `libraries/fs/`      | Generic COW filesystem. BlockDevice trait, inodes, allocator, snapshots, crash consistency. Reusable by anyone. No document or media type concepts.                  |
| store library         | `libraries/store/`   | Metadata layer. Wraps `Box<dyn Files>`. Catalog management, media types, attributes, queries. Specific to this OS. Shared with factory image builder.                |
| Document service      | `services/document/` | Bare-metal process. Thin IPC wrapper — translates IPC messages into store API calls. All document logic lives in the store library. Replaces `services/filesystem/`. |
| Factory image builder | host tool            | Creates pre-populated disk images at build time using the same store library.                                                                                        |

### Why two libraries

The fs library is **generic infrastructure** — a COW filesystem useful to anyone building anything. The store library adds **document-centric semantics** specific to this OS (media types, queryable metadata, catalog). Each can be swapped independently.

### Why `Box<dyn Files>`

The store library wraps `Box<dyn Files>` (a trait object) rather than `Filesystem<D>` (a generic). This means:

- The store library never sees `BlockDevice` or `Filesystem<D>`.
- The generic parameter is fully contained in the fs library and the document service.
- The factory image builder uses `Store::init(Box::new(Filesystem::format(FileBackedDevice::new(path))))`.
- The document service uses `Store::open(Box::new(Filesystem::mount(VirtioBlockDevice::new(...))))`.
- Same store code, different backing stores. The store doesn't know or care what's underneath.

## fs library changes

### New `Files` trait methods

```rust
/// List all file IDs in the filesystem.
fn list_files(&self) -> Result<Vec<FileId>, FsError>;

/// Designate a root file (persisted in superblock).
fn set_root(&mut self, file: FileId) -> Result<(), FsError>;

/// Retrieve the designated root file.
fn root(&self) -> Option<FileId>;
```

`list_files()` is needed for queries. `set_root()`/`root()` enables the store to find its catalog file after reboot — a generic filesystem concept (like root inode in ext4).

### Linked-block inode table

The inode table moves from a single 16 KiB block to a **linked-block chain** (same pattern as the snapshot store). This removes the 1365-file hard limit.

The entire inode table is loaded into `BTreeMap` at mount and rewritten on commit. O(n) in file count. See "Target scale" below for why this is the permanent design, not an interim solution.

### Target scale (permanent design)

This is a personal document-centric OS. It does not have apps, package managers, `.git` directories, or file trees. It has documents — things with media types that humans create or consume.

| Scale      | Inode table       | Catalog memory | Expected use                               |
| ---------- | ----------------- | -------------- | ------------------------------------------ |
| 1K files   | 12 KB, 1 block    | ~128 KB        | Early use                                  |
| 10K files  | 120 KB, 8 blocks  | ~1.3 MB        | Moderate (documents, some photos, music)   |
| 100K files | 1.2 MB, 75 blocks | ~12.8 MB       | Heavy (prolific photographer + everything) |

100K is the realistic ceiling for a personal OS without an app ecosystem. 12.8 MB of catalog memory is well within workstation-class RAM. Full-rewrite of 75 inode blocks on commit is ~1.2 MB of I/O — trivial.

**This is the permanent design, not a stepping stone.** The in-memory BTreeMap catalog and linked-block inode table are the right architecture for the target scale. They are not interim solutions waiting to be replaced by a database engine. The `Files` trait and `Store` API are the stability boundaries — if someone adapted this for a million-file use case, they would replace internals behind those interfaces, but that is not our target.

### Documented design characteristics

The fs library documents these properties:

- **Target scale:** Personal workloads (up to ~100K files, single writer).
- **Inode table:** Linked-block chain, loaded into in-memory BTreeMap at mount, full-rewrite on commit. O(n) mount and commit.
- **Allocator:** Single-block free-extent list (~2047 extents). Adequate for expected fragmentation levels.
- **COW granularity:** Entire-file COW on write. O(file_size) per write. Per-block COW is a future optimization.
- **Stability boundary:** The `Files` trait. Consumers are unaffected by changes to storage internals.

## Store library design

### Catalog

A single **catalog file** stored in the filesystem holds metadata for all documents. The catalog is "just a file" — the fs library doesn't know it's special.

**Discovery:** The store calls `set_root(catalog_id)` during `init()`. On `open()`, `root()` returns the catalog FileId in O(1).

**Schema per entry:**

| Field      | Type                     | Source                          |
| ---------- | ------------------------ | ------------------------------- |
| file_id    | u64                      | Catalog (key)                   |
| media_type | String                   | Catalog (mandatory at creation) |
| attributes | BTreeMap<String, String> | Catalog (optional key-value)    |
| size       | u64                      | fs library (`FileMetadata`)     |
| created    | u64                      | fs library (`FileMetadata`)     |
| modified   | u64                      | fs library (`FileMetadata`)     |

Size, created, and modified come from the fs library — no duplication. The catalog only stores what the fs doesn't already know: media type and attributes.

**Serialization:** Variable-length entries, sequentially packed into the catalog file:

```text
[entry_count: u32]
[Entry]*:
  [file_id: u64]
  [media_type_len: u16] [media_type: [u8]]
  [attr_count: u16]
  [Attr]*:
    [key_len: u16] [key: [u8]]
    [val_len: u16] [val: [u8]]
```

Loaded entirely into `BTreeMap<FileId, CatalogEntry>` at mount. Written entirely on commit. Same pattern as inode table and snapshot store.

**Snapshots include the catalog.** When the store creates a snapshot, it transparently includes the catalog file. Restoring a snapshot restores metadata in sync with content.

### Public API

```rust
pub struct Store {
    fs: Box<dyn Files>,
    catalog: BTreeMap<FileId, CatalogEntry>,
    catalog_file: FileId,
}

impl Store {
    /// First boot: create catalog, set root, commit.
    /// Fails if root already exists (use open() for existing stores).
    pub fn init(fs: Box<dyn Files>) -> Result<Self, StoreError>;

    /// Subsequent boots: read root, load catalog.
    /// Fails if no root exists (use init() for new stores).
    pub fn open(fs: Box<dyn Files>) -> Result<Self, StoreError>;

    // ── File lifecycle ──

    /// Create a new file. Media type is mandatory.
    pub fn create(&mut self, media_type: &str) -> Result<FileId, StoreError>;

    /// Delete a file and its catalog entry.
    pub fn delete(&mut self, file: FileId) -> Result<(), StoreError>;

    // ── Content access (delegates to fs) ──

    pub fn read(&self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<usize, StoreError>;
    pub fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), StoreError>;
    pub fn truncate(&mut self, file: FileId, len: u64) -> Result<(), StoreError>;

    // ── Metadata ──

    pub fn media_type(&self, file: FileId) -> Result<&str, StoreError>;
    pub fn set_attribute(&mut self, file: FileId, key: &str, val: &str) -> Result<(), StoreError>;
    pub fn attribute(&self, file: FileId, key: &str) -> Option<&str>;
    pub fn metadata(&self, file: FileId) -> Result<DocumentMetadata, StoreError>;

    // ── Query ──

    pub fn query(&self, filter: &Query) -> Result<Vec<FileId>, StoreError>;

    // ── Versioning (delegates to fs, includes catalog) ──

    pub fn snapshot(&mut self, files: &[FileId]) -> Result<SnapshotId, StoreError>;
    pub fn restore(&mut self, snapshot: SnapshotId) -> Result<(), StoreError>;

    // ── Persistence ──

    /// Write catalog to disk, then delegate commit to fs.
    pub fn commit(&mut self) -> Result<(), StoreError>;
}
```

### Query model

```rust
pub enum Query {
    /// Exact media type match: "font/ttf"
    MediaType(String),
    /// Top-level type match: "font" matches font/ttf, font/otf, etc.
    Type(String),
    /// Exact attribute match: key = value
    Attribute { key: String, value: String },
    /// All filters must match.
    And(Vec<Query>),
    /// Any filter must match.
    Or(Vec<Query>),
}
```

Uses standard MIME terminology (RFC 6838): **type** = top-level category (font, image, text), **subtype** = specific format (ttf, png, plain), **media type** = full string (font/ttf).

**This is the permanent internal representation.** The `Query` enum is the query AST. A future SQL syntax layer would parse SQL strings into `Query` values — additive, not replacement. The execution engine always operates on `Query`.

### Composite metadata

```rust
pub struct DocumentMetadata {
    pub file_id: FileId,
    pub media_type: String,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
    pub attributes: BTreeMap<String, String>,
}
```

Composed from fs library metadata (size, created, modified) and catalog metadata (media_type, attributes).

## Factory disk image

A build-time host tool creates a pre-populated disk image using the store library.

**Contents:**

| File           | Media type | Attributes                                    |
| -------------- | ---------- | --------------------------------------------- |
| JetBrains Mono | font/ttf   | name=JetBrains Mono, role=system, style=mono  |
| Inter          | font/ttf   | name=Inter, role=system, style=sans           |
| Source Serif 4 | font/ttf   | name=Source Serif 4, role=system, style=serif |
| test.png       | image/png  | name=test, role=test                          |

**Flow:**

```text
Host: FileBackedDevice → Filesystem::format() → Store::init() → create fonts → commit()
                                                                        ↓
                                                                   disk.img
Guest: VirtioBlockDevice → Filesystem::mount() → Store::open() → query("font") → fonts
```

Same store library, same code paths, host and guest.

## Boot sequence changes

Current boot (9p dependency):

```text
Phase 1.5: 9p driver → read fonts from host → Content Region
Phase 10:  filesystem service (format, IPC loop)
```

New boot (native fs):

```text
Early:  Document service starts → mounts disk → Store::open()
Then:   Init queries document service for Type("font") files
        → loads font data into Content Region
Later:  Core starts, uses Content Region as before
```

9p is no longer a boot dependency. It remains as a **development/import tool** (clearly marked as external tooling, not part of the OS architecture).

## IPC protocol (scaffolding)

The IPC protocol between core and the document service stays **minimal scaffolding** for v0.4. Core's architecture has not been deliberately designed — investing in a rich document-management IPC protocol now would encode premature UX assumptions.

For v0.4, the protocol needs just enough to:

- Load fonts at boot (init → document service)
- Persist the current document (core → document service, extending current MSG_FS_COMMIT pattern)
- Request undo/redo (core → document service)

The full Store API does NOT need to be projected over IPC yet. That design happens when core is deliberately designed.

## Atomic multi-file writes

`commit()` is the transaction boundary. All writes between commits land atomically or not at all (two-flush protocol: crash before second flush → old superblock wins → old state). This means compound document creation — writing multiple content files plus updating the catalog — is atomic without any additional machinery. This property is inherited from the COW filesystem design and is required for future compound document support.

## Undo/redo

Wire the store's snapshot/restore to core's existing operation boundaries:

- Core calls snapshot at operation boundaries (after draining editor messages), same as current MSG_FS_COMMIT timing.
- Undo = restore previous snapshot. Redo = restore next snapshot (if available).
- Global sequential undo — walks snapshot history regardless of which editor.
- The snapshot infrastructure already exists in the fs library. This phase wires it to core.

## Key design decisions

| Decision                            | Choice                                               | Rationale                                           |
| ----------------------------------- | ---------------------------------------------------- | --------------------------------------------------- |
| fs and store are separate libraries | Clean boundary, independent swappability             | fs is generic, store is OS-specific                 |
| Store uses `Box<dyn Files>`         | No generic parameter leaking                         | Store doesn't know about BlockDevice                |
| Single catalog file in the fs       | fs library unchanged, COW atomicity for free         | Simplest correct approach                           |
| Catalog found via `set_root`/`root` | Explicit, O(1), generic fs concept                   | Any fs swap requires preparation anyway             |
| Linked-block inode table            | Removes 1365-file limit                              | Full-rewrite OK at personal scale                   |
| Media type mandatory at creation    | No typeless files                                    | Design: declaration at creation                     |
| IPC stays minimal scaffolding       | Core not yet deliberately designed                   | Don't encode UX assumptions in plumbing             |
| No file paths, ever                 | Files found by metadata query                        | Paths don't exist in this OS                        |
| Factory disk image for first boot   | No chicken-and-egg, no first-boot special path       | Simplest thing that works                           |
| Services are translation layers     | Each service translates between core and a subsystem | Core never touches hardware or low-level primitives |

## Reframed principle: "everything is a file"

Original premise (early design): "everything is a file" (Unix-style universal interface).

Revised: **"All persistent content with a media type is a file."** The litmus test: does it have a media type and does it persist? If yes, it belongs in the filesystem. If no (scene graph, IPC channels, process state), it doesn't. This keeps the filesystem meaningful as the persistent content store without forcing runtime state through it for philosophical purity.

## What this spec does NOT cover

These are deliberately deferred:

- **Core architecture.** Core is scaffolding. Its deliberate design is a future sprint.
- **Full Store-over-IPC API.** Depends on core's design.
- **Content extraction** (EXIF, ID3). Future metadata enhancement.
- **User-applied tags via UI.** Requires interaction model design.
- **Compound documents / manifest model.** Has open design questions (#14).
- **Import/export at runtime.** Requires networking or other guest I/O.
- **SQL syntax layer.** Would parse SQL into `Query` AST. Additive — the `Query` enum is the permanent internal representation.
- **Per-block COW.** Future fs optimization, behind the Files trait boundary.
