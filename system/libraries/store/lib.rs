//! Document store — metadata layer over `fs::Files`.
//!
//! Wraps a `Box<dyn Files>` and adds media types, queryable attributes,
//! and a persistent catalog. The catalog is itself stored as a file
//! within the filesystem, referenced via the root pointer.

#![no_std]
extern crate alloc;

mod serialize;

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use fs::{FileId, Files, FsError, SnapshotId};
use serialize::{decode_catalog, encode_catalog};

/// Magic number for the catalog binary format ("CATL").
const CATALOG_MAGIC: u32 = 0x4341_544C;

// ── Public types ─────────────────────────────────────────────────────

/// Per-file catalog entry (media type + attributes).
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub media_type: String,
    pub attributes: BTreeMap<String, String>,
}

/// Document metadata composed from filesystem metadata + catalog entry.
#[derive(Debug, Clone)]
pub struct DocumentMetadata {
    pub file_id: FileId,
    pub media_type: String,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
    pub attributes: BTreeMap<String, String>,
}

/// Query filter for scanning the catalog.
#[derive(Debug, Clone)]
pub enum Query {
    /// Exact media type match (e.g. "font/ttf").
    MediaType(String),
    /// Type prefix match (e.g. "font" matches "font/ttf", "font/otf").
    Type(String),
    /// Attribute key-value match.
    Attribute { key: String, value: String },
    /// All sub-queries must match.
    And(Vec<Query>),
    /// At least one sub-query must match.
    Or(Vec<Query>),
}

/// Store error.
#[derive(Debug)]
pub enum StoreError {
    /// Underlying filesystem error.
    Fs(FsError),
    /// Store already initialized (root exists).
    AlreadyInitialized,
    /// Store not initialized (no root).
    NotInitialized,
    /// File not in catalog.
    NotFound(FileId),
    /// Catalog data corrupt.
    Corrupt(String),
}

impl From<FsError> for StoreError {
    fn from(e: FsError) -> Self {
        StoreError::Fs(e)
    }
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fs(e) => write!(f, "fs: {e}"),
            Self::AlreadyInitialized => write!(f, "store already initialized"),
            Self::NotInitialized => write!(f, "store not initialized"),
            Self::NotFound(id) => write!(f, "file {:?} not in catalog", id),
            Self::Corrupt(msg) => write!(f, "catalog corrupt: {msg}"),
        }
    }
}

// ── Store ────────────────────────────────────────────────────────────

/// Document store wrapping a filesystem with metadata catalog.
pub struct Store {
    fs: Box<dyn Files>,
    catalog: BTreeMap<u64, CatalogEntry>,
    catalog_file: FileId,
}

impl core::fmt::Debug for Store {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Store")
            .field("catalog_file", &self.catalog_file)
            .field("catalog_len", &self.catalog.len())
            .finish()
    }
}

impl Store {
    /// Initialize a new store on a fresh filesystem.
    ///
    /// Fails if a root pointer already exists.
    pub fn init(mut fs: Box<dyn Files>) -> Result<Self, StoreError> {
        if fs.root().is_some() {
            return Err(StoreError::AlreadyInitialized);
        }

        let catalog_file = fs.create()?;
        fs.set_root(catalog_file)?;

        let catalog = BTreeMap::new();
        let data = encode_catalog(CATALOG_MAGIC, &catalog);
        fs.write(catalog_file, 0, &data)?;
        fs.commit()?;

        Ok(Self {
            fs,
            catalog,
            catalog_file,
        })
    }

    /// Open an existing store.
    ///
    /// Fails if no root pointer exists.
    pub fn open(fs: Box<dyn Files>) -> Result<Self, StoreError> {
        let catalog_file = fs.root().ok_or(StoreError::NotInitialized)?;

        let size = fs.size(catalog_file)? as usize;
        let mut data = vec![0u8; size];
        fs.read(catalog_file, 0, &mut data)?;

        let catalog = decode_catalog(CATALOG_MAGIC, &data)?;

        Ok(Self {
            fs,
            catalog,
            catalog_file,
        })
    }

    /// Create a new file with the given media type.
    pub fn create(&mut self, media_type: &str) -> Result<FileId, StoreError> {
        let file_id = self.fs.create()?;
        self.catalog.insert(
            file_id.0,
            CatalogEntry {
                media_type: media_type.to_string(),
                attributes: BTreeMap::new(),
            },
        );
        Ok(file_id)
    }

    /// Delete a file and remove it from the catalog.
    pub fn delete(&mut self, file: FileId) -> Result<(), StoreError> {
        if self.catalog.remove(&file.0).is_none() {
            return Err(StoreError::NotFound(file));
        }
        self.fs.delete(file)?;
        Ok(())
    }

    /// Read file content.
    pub fn read(&self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<usize, StoreError> {
        Ok(self.fs.read(file, offset, buf)?)
    }

    /// Write file content.
    pub fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), StoreError> {
        Ok(self.fs.write(file, offset, data)?)
    }

    /// Truncate a file.
    pub fn truncate(&mut self, file: FileId, len: u64) -> Result<(), StoreError> {
        Ok(self.fs.truncate(file, len)?)
    }

    /// Get a file's media type.
    pub fn media_type(&self, file: FileId) -> Result<&str, StoreError> {
        self.catalog
            .get(&file.0)
            .map(|e| e.media_type.as_str())
            .ok_or(StoreError::NotFound(file))
    }

    /// Set an attribute on a file.
    pub fn set_attribute(
        &mut self,
        file: FileId,
        key: &str,
        value: &str,
    ) -> Result<(), StoreError> {
        let entry = self
            .catalog
            .get_mut(&file.0)
            .ok_or(StoreError::NotFound(file))?;
        entry.attributes.insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Get an attribute value.
    pub fn attribute(&self, file: FileId, key: &str) -> Result<Option<&str>, StoreError> {
        let entry = self
            .catalog
            .get(&file.0)
            .ok_or(StoreError::NotFound(file))?;
        Ok(entry.attributes.get(key).map(|v| v.as_str()))
    }

    /// Get document metadata (filesystem metadata + catalog entry).
    pub fn metadata(&self, file: FileId) -> Result<DocumentMetadata, StoreError> {
        let entry = self
            .catalog
            .get(&file.0)
            .ok_or(StoreError::NotFound(file))?;
        let fm = self.fs.metadata(file)?;
        Ok(DocumentMetadata {
            file_id: fm.file_id,
            media_type: entry.media_type.clone(),
            size: fm.size,
            created: fm.created,
            modified: fm.modified,
            attributes: entry.attributes.clone(),
        })
    }

    /// Query the catalog. Returns file IDs matching the filter.
    pub fn query(&self, filter: &Query) -> Vec<FileId> {
        self.catalog
            .iter()
            .filter(|(_, entry)| matches_query(entry, filter))
            .map(|(&id, _)| FileId(id))
            .collect()
    }

    /// Snapshot the given files. Writes catalog to disk first, then
    /// includes the catalog file in the snapshot.
    pub fn snapshot(&mut self, files: &[FileId]) -> Result<SnapshotId, StoreError> {
        self.write_catalog()?;

        let mut all = Vec::with_capacity(files.len() + 1);
        all.extend_from_slice(files);
        all.push(self.catalog_file);

        Ok(self.fs.snapshot(&all)?)
    }

    /// Restore a snapshot, then reload the catalog from disk.
    pub fn restore(&mut self, snapshot: SnapshotId) -> Result<(), StoreError> {
        self.fs.restore(snapshot)?;
        self.reload_catalog()?;
        Ok(())
    }

    /// Delete a snapshot, freeing its blocks.
    pub fn delete_snapshot(&mut self, snapshot: SnapshotId) -> Result<(), StoreError> {
        self.fs.delete_snapshot(snapshot).map_err(StoreError::Fs)
    }

    /// Commit: write catalog to disk, then commit the filesystem.
    pub fn commit(&mut self) -> Result<(), StoreError> {
        self.write_catalog()?;
        self.fs.commit()?;
        Ok(())
    }

    /// Consume the store, returning the inner filesystem.
    pub fn into_inner(self) -> Box<dyn Files> {
        self.fs
    }

    // ── Private helpers ──────────────────────────────────────────────

    fn write_catalog(&mut self) -> Result<(), StoreError> {
        let data = encode_catalog(CATALOG_MAGIC, &self.catalog);
        self.fs.truncate(self.catalog_file, 0)?;
        self.fs.write(self.catalog_file, 0, &data)?;
        Ok(())
    }

    fn reload_catalog(&mut self) -> Result<(), StoreError> {
        let size = self.fs.size(self.catalog_file)? as usize;
        let mut data = vec![0u8; size];
        self.fs.read(self.catalog_file, 0, &mut data)?;
        self.catalog = decode_catalog(CATALOG_MAGIC, &data)?;
        Ok(())
    }
}

// ── Query matching ───────────────────────────────────────────────────

fn matches_query(entry: &CatalogEntry, query: &Query) -> bool {
    match query {
        Query::MediaType(mt) => entry.media_type == *mt,
        Query::Type(t) => {
            entry.media_type.starts_with(t.as_str())
                && entry.media_type.as_bytes().get(t.len()) == Some(&b'/')
        }
        Query::Attribute { key, value } => entry
            .attributes
            .get(key.as_str())
            .map_or(false, |v| v == value),
        Query::And(qs) => qs.iter().all(|q| matches_query(entry, q)),
        Query::Or(qs) => qs.iter().any(|q| matches_query(entry, q)),
    }
}
