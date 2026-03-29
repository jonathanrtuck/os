//! Document buffer read-only access and header sync.
//!
//! The view engine (C) reads the document buffer for scene building and
//! navigation. All content mutations go through the document-model (A)
//! via IPC. C writes only the header (cursor position sync).

use super::DOC_HEADER_SIZE;

/// Access the document content as a byte slice (after the header).
pub(crate) fn doc_content() -> &'static [u8] {
    let s = super::state();
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    // doc_len is always <= doc_capacity - DOC_HEADER_SIZE (maintained by
    // the document-model service). doc_buf is set once during init and
    // never null after that point.
    unsafe {
        debug_assert!(!s.doc_buf.is_null());
        debug_assert!(s.doc_len <= s.doc_capacity);
        core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), s.doc_len)
    }
}

pub(crate) fn doc_write_header() {
    let s = super::state();
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        core::ptr::write_volatile(s.doc_buf as *mut u64, s.doc_len as u64);
        core::ptr::write_volatile(s.doc_buf.add(8) as *mut u64, s.cursor.pos as u64);
    }
}

// ── Rich text (text/rich) document read access ────────────────────────
//
// For text/rich documents the shared doc buffer IS the piece table.
// C reads it for scene building; A is the sole writer.

/// Access the raw piece table buffer as a mutable slice.
/// Used for selection/style writes that C still handles directly.
fn rich_buf() -> &'static mut [u8] {
    let s = super::state();
    let cap = s.doc_capacity.saturating_sub(DOC_HEADER_SIZE);
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        debug_assert!(!s.doc_buf.is_null());
        core::slice::from_raw_parts_mut(s.doc_buf.add(DOC_HEADER_SIZE), cap)
    }
}

/// Access the raw piece table buffer as an immutable slice.
pub(crate) fn rich_buf_ref() -> &'static [u8] {
    let s = super::state();
    let cap = s.doc_capacity.saturating_sub(DOC_HEADER_SIZE);
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        debug_assert!(!s.doc_buf.is_null());
        core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), cap)
    }
}

/// Get the logical text length of a rich text document.
pub(crate) fn rich_text_len() -> usize {
    let buf = rich_buf_ref();
    piecetable::text_len(buf) as usize
}

/// Extract the logical text of the rich document into a scratch buffer.
/// Returns the number of bytes copied.
pub(crate) fn rich_copy_text(out: &mut [u8]) -> usize {
    let buf = rich_buf_ref();
    let len = piecetable::text_len(buf);
    piecetable::text_slice(buf, 0, len, out)
}

/// Write selection range to the piece table header for editor reads.
pub(crate) fn rich_set_selection(start: usize, end: usize) {
    let buf = rich_buf();
    piecetable::set_selection(buf, start as u32, end as u32);
}

/// Apply a style to a byte range in a rich text document.
pub(crate) fn rich_apply_style(start: usize, end: usize, style_id: u8) {
    let buf = rich_buf();
    piecetable::apply_style(buf, start as u32, end as u32, style_id);
}

/// Set the current insertion style for a rich text document.
pub(crate) fn rich_set_current_style(style_id: u8) {
    let buf = rich_buf();
    piecetable::set_current_style(buf, style_id);
}

pub(crate) fn format_time_hms(total_seconds: u64, buf: &mut [u8; 8]) {
    let hours = ((total_seconds / 3600) % 24) as u8;
    let minutes = ((total_seconds / 60) % 60) as u8;
    let seconds = (total_seconds % 60) as u8;

    buf[0] = b'0' + hours / 10;
    buf[1] = b'0' + hours % 10;
    buf[2] = b':';
    buf[3] = b'0' + minutes / 10;
    buf[4] = b'0' + minutes % 10;
    buf[5] = b':';
    buf[6] = b'0' + seconds / 10;
    buf[7] = b'0' + seconds % 10;
}
