use std::env;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{FileStore, HostFileStore};

/// Monotonic counter to give each test its own directory.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a `HostFileStore` in a unique temp directory.
fn make_store() -> HostFileStore {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!("filestore-test-{}-{}", std::process::id(), n));
    // Clean up any leftover from a previous run.
    let _ = std::fs::remove_dir_all(&dir);
    HostFileStore::new(&dir).expect("failed to create HostFileStore")
}

// ── basic create / write / read ──────────────────────────────────────

#[test]
fn create_write_read() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"hello").unwrap();
    assert_eq!(store.read(f).unwrap(), b"hello");
}

// ── snapshot and verify old content ──────────────────────────────────

#[test]
fn snapshot_preserves_old_content() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"version1").unwrap();

    let snap = store.snapshot(f).unwrap();

    store.write(f, 0, b"version2").unwrap();
    assert_eq!(store.read(f).unwrap(), b"version2");
    assert_eq!(store.read_snapshot(snap).unwrap(), b"version1");
}

// ── restore from snapshot ────────────────────────────────────────────

#[test]
fn restore_from_snapshot() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"original").unwrap();

    let snap = store.snapshot(f).unwrap();
    store.write(f, 0, b"modified").unwrap();

    store.restore(f, snap).unwrap();
    assert_eq!(store.read(f).unwrap(), b"original");
}

// ── clone file, modify clone, verify original unchanged ──────────────

#[test]
fn clone_independence() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"shared").unwrap();

    let cloned = store.clone_file(f).unwrap();
    store.write(cloned, 0, b"CLONED").unwrap();

    assert_eq!(store.read(f).unwrap(), b"shared");
    assert_eq!(store.read(cloned).unwrap(), b"CLONED");
}

// ── delete file ──────────────────────────────────────────────────────

#[test]
fn delete_file() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"data").unwrap();
    store.delete(f).unwrap();
    assert!(store.read(f).is_err());
}

// ── delete file removes its snapshots ────────────────────────────────

#[test]
fn delete_file_removes_snapshots() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"data").unwrap();
    let snap = store.snapshot(f).unwrap();
    store.delete(f).unwrap();
    assert!(store.read_snapshot(snap).is_err());
}

// ── delete snapshot ──────────────────────────────────────────────────

#[test]
fn delete_snapshot() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"data").unwrap();
    let snap = store.snapshot(f).unwrap();

    store.delete_snapshot(snap).unwrap();
    assert!(store.read_snapshot(snap).is_err());

    // File itself is unaffected.
    assert_eq!(store.read(f).unwrap(), b"data");
}

// ── list snapshots ordering ──────────────────────────────────────────

#[test]
fn snapshots_ordered_by_creation_time() {
    let mut store = make_store();
    let f = store.create().unwrap();

    store.write(f, 0, b"v1").unwrap();
    let s1 = store.snapshot(f).unwrap();

    store.write(f, 0, b"v2").unwrap();
    let s2 = store.snapshot(f).unwrap();

    store.write(f, 0, b"v3").unwrap();
    let s3 = store.snapshot(f).unwrap();

    let snaps = store.snapshots(f).unwrap();
    assert_eq!(snaps.len(), 3);
    assert_eq!(snaps[0].id, s1);
    assert_eq!(snaps[1].id, s2);
    assert_eq!(snaps[2].id, s3);

    // Timestamps are non-decreasing.
    assert!(snaps[0].timestamp <= snaps[1].timestamp);
    assert!(snaps[1].timestamp <= snaps[2].timestamp);
}

// ── multiple snapshots simulate undo chain ───────────────────────────

#[test]
fn undo_chain_simulation() {
    let mut store = make_store();
    let f = store.create().unwrap();

    // Build an edit history with snapshots at each step.
    let mut snap_ids = Vec::new();
    for i in 0..5 {
        let content = format!("edit-{}", i);
        store.write(f, 0, content.as_bytes()).unwrap();
        // Resize to exact content length (write may have left the file larger
        // from a previous iteration with a longer string — but in this test
        // all strings are the same length, so this is just defensive).
        store.resize(f, content.len() as u64).unwrap();
        snap_ids.push(store.snapshot(f).unwrap());
    }

    // Walk backwards through the undo chain.
    for (i, &snap) in snap_ids.iter().enumerate().rev() {
        store.restore(f, snap).unwrap();
        let expected = format!("edit-{}", i);
        assert_eq!(store.read(f).unwrap(), expected.as_bytes());
    }
}

// ── write at offset ──────────────────────────────────────────────────

#[test]
fn write_at_offset() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"hello world").unwrap();

    // Overwrite "world" with "rust!"
    store.write(f, 6, b"rust!").unwrap();
    assert_eq!(store.read(f).unwrap(), b"hello rust!");
}

#[test]
fn write_at_offset_extends_file() {
    let mut store = make_store();
    let f = store.create().unwrap();
    // Write past the end of an empty file — should auto-extend.
    store.write(f, 5, b"abc").unwrap();
    let data = store.read(f).unwrap();
    assert_eq!(data.len(), 8);
    // Bytes 0..5 are zero-filled by set_len.
    assert_eq!(&data[..5], &[0u8; 5]);
    assert_eq!(&data[5..], b"abc");
}

// ── resize grow and shrink ───────────────────────────────────────────

#[test]
fn resize_grow() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"hi").unwrap();
    assert_eq!(store.size(f).unwrap(), 2);

    store.resize(f, 10).unwrap();
    assert_eq!(store.size(f).unwrap(), 10);

    let data = store.read(f).unwrap();
    assert_eq!(&data[..2], b"hi");
    // Extended region is zero-filled.
    assert!(data[2..].iter().all(|&b| b == 0));
}

#[test]
fn resize_shrink() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"hello world").unwrap();
    assert_eq!(store.size(f).unwrap(), 11);

    store.resize(f, 5).unwrap();
    assert_eq!(store.size(f).unwrap(), 5);
    assert_eq!(store.read(f).unwrap(), b"hello");
}

// ── size of empty file ───────────────────────────────────────────────

#[test]
fn size_empty_file() {
    let mut store = make_store();
    let f = store.create().unwrap();
    assert_eq!(store.size(f).unwrap(), 0);
}

// ── error: read nonexistent file ─────────────────────────────────────

#[test]
fn read_nonexistent_file() {
    let store = make_store();
    let bad = crate::FileId(99999);
    assert!(store.read(bad).is_err());
}

// ── error: snapshot nonexistent file ─────────────────────────────────

#[test]
fn snapshot_nonexistent_file() {
    let mut store = make_store();
    let bad = crate::FileId(99999);
    assert!(store.snapshot(bad).is_err());
}

// ── error: restore with wrong file ───────────────────────────────────

#[test]
fn restore_wrong_file() {
    let mut store = make_store();
    let a = store.create().unwrap();
    let b = store.create().unwrap();
    store.write(a, 0, b"a-data").unwrap();
    let snap = store.snapshot(a).unwrap();
    // Restoring file b from file a's snapshot should fail.
    assert!(store.restore(b, snap).is_err());
}

// ── snapshot info has correct file size ──────────────────────────────

#[test]
fn snapshot_info_file_size() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"12345").unwrap();
    let snap = store.snapshot(f).unwrap();

    let infos = store.snapshots(f).unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, snap);
    assert_eq!(infos[0].file_size, 5);
    // Timestamp should be recent (within the last minute).
    let elapsed = infos[0].timestamp.elapsed().unwrap_or_default();
    assert!(elapsed.as_secs() < 60);
}

// ── snapshots on empty file ──────────────────────────────────────────

#[test]
fn snapshots_empty_list() {
    let mut store = make_store();
    let f = store.create().unwrap();
    let snaps = store.snapshots(f).unwrap();
    assert!(snaps.is_empty());
}

// ── delete snapshot then list ────────────────────────────────────────

#[test]
fn delete_snapshot_then_list() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"data").unwrap();
    let s1 = store.snapshot(f).unwrap();
    let s2 = store.snapshot(f).unwrap();

    store.delete_snapshot(s1).unwrap();
    let snaps = store.snapshots(f).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].id, s2);
}

// ── clone preserves snapshot independence ─────────────────────────────

#[test]
fn clone_does_not_share_snapshots() {
    let mut store = make_store();
    let f = store.create().unwrap();
    store.write(f, 0, b"original").unwrap();
    let snap = store.snapshot(f).unwrap();

    let cloned = store.clone_file(f).unwrap();
    // The clone has no snapshots of its own.
    let cloned_snaps = store.snapshots(cloned).unwrap();
    assert!(cloned_snaps.is_empty());

    // Original's snapshot is still accessible.
    assert_eq!(store.read_snapshot(snap).unwrap(), b"original");
}
