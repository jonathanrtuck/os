//! Document buffer operations.
//!
//! Provides insert, delete, and content access for the shared document
//! buffer. The buffer is owned by CoreState; these functions are the
//! sole writers (via init's shared memory mapping).

use super::DOC_HEADER_SIZE;

/// Access the document content as a byte slice (after the header).
pub(crate) fn doc_content() -> &'static [u8] {
    let s = super::state();
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    // doc_len is always <= doc_capacity - DOC_HEADER_SIZE (maintained by
    // doc_insert/doc_delete/doc_delete_range). doc_buf is set once during
    // init and never null after that point.
    unsafe {
        debug_assert!(!s.doc_buf.is_null());
        debug_assert!(s.doc_len <= s.doc_capacity);
        core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), s.doc_len)
    }
}

pub(crate) fn doc_delete(pos: usize) -> bool {
    let s = super::state();
    if s.doc_len == 0 || pos >= s.doc_len {
        return false;
    }
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if pos + 1 < s.doc_len {
            core::ptr::copy(base.add(pos + 1), base.add(pos), s.doc_len - pos - 1);
        }
    }
    s.doc_len -= 1;
    doc_write_header();
    true
}

pub(crate) fn doc_delete_range(start: usize, end: usize) -> bool {
    let s = super::state();
    if start >= end || start >= s.doc_len || end > s.doc_len {
        return false;
    }
    let del_count = end - start;
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if end < s.doc_len {
            core::ptr::copy(base.add(end), base.add(start), s.doc_len - end);
        }
    }
    s.doc_len -= del_count;
    doc_write_header();
    true
}

pub(crate) fn doc_insert(pos: usize, byte: u8) -> bool {
    let s = super::state();
    if s.doc_len >= s.doc_capacity || pos > s.doc_len {
        return false;
    }
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if pos < s.doc_len {
            core::ptr::copy(base.add(pos), base.add(pos + 1), s.doc_len - pos);
        }
        *base.add(pos) = byte;
    }
    s.doc_len += 1;
    doc_write_header();
    true
}

pub(crate) fn doc_write_header() {
    let s = super::state();
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        core::ptr::write_volatile(s.doc_buf as *mut u64, s.doc_len as u64);
        core::ptr::write_volatile(s.doc_buf.add(8) as *mut u64, s.cursor_pos as u64);
    }
}

// ── Rich text (text/rich) document operations ───────────────────────
//
// For text/rich documents the shared doc buffer IS the piece table.
// All mutations go through the piecetable library. The DOC_HEADER_SIZE
// prefix is NOT used — the entire buffer is the piece table.

/// Access the raw piece table buffer as a mutable slice.
/// The piece table lives at doc_buf + DOC_HEADER_SIZE, same as flat text.
/// Capacity = doc_capacity - DOC_HEADER_SIZE.
/// Only valid when doc_format == Rich.
pub(crate) fn rich_buf() -> &'static mut [u8] {
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

/// Compute the total serialized size of the piece table (for doc_len).
/// This is the number of bytes the document service needs to persist.
pub(crate) fn rich_total_size() -> usize {
    let buf = rich_buf_ref();
    let h = piecetable::header(buf);
    piecetable::HEADER_SIZE
        + h.style_count as usize * 12 // Style is 12 bytes
        + h.piece_count as usize * 16 // Piece is 16 bytes
        + h.original_len as usize
        + h.add_len as usize
}

/// Update the shared buffer header with the current piece table size.
/// Called after each piece table mutation so the document service knows
/// how many bytes to commit.
pub(crate) fn rich_sync_header() {
    let s = super::state();
    let total = rich_total_size();
    s.doc_len = total;
    // Write doc_len to the shared header (same as doc_write_header).
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        core::ptr::write_volatile(s.doc_buf as *mut u64, total as u64);
        core::ptr::write_volatile(s.doc_buf.add(8) as *mut u64, s.cursor_pos as u64);
    }
}

/// Insert a single byte into a rich text document at the cursor position.
pub(crate) fn rich_insert(pos: usize, byte: u8) -> bool {
    let buf = rich_buf();
    let ok = piecetable::insert(buf, pos as u32, byte);
    if ok {
        rich_sync_header();
    }
    ok
}

/// Delete a single byte from a rich text document.
pub(crate) fn rich_delete(pos: usize) -> bool {
    let buf = rich_buf();
    let ok = piecetable::delete(buf, pos as u32);
    if ok {
        rich_sync_header();
    }
    ok
}

/// Delete a byte range from a rich text document.
pub(crate) fn rich_delete_range(start: usize, end: usize) -> bool {
    let buf = rich_buf();
    let ok = piecetable::delete_range(buf, start as u32, end as u32);
    if ok {
        rich_sync_header();
    }
    ok
}

/// Apply a style to a byte range in a rich text document.
pub(crate) fn rich_apply_style(start: usize, end: usize, style_id: u8) {
    let buf = rich_buf();
    piecetable::apply_style(buf, start as u32, end as u32, style_id);
    rich_sync_header();
}

/// Set the current insertion style for a rich text document.
pub(crate) fn rich_set_current_style(style_id: u8) {
    let buf = rich_buf();
    piecetable::set_current_style(buf, style_id);
}

/// Get the logical text length of a rich text document.
pub(crate) fn rich_text_len() -> usize {
    let buf = rich_buf_ref();
    piecetable::text_len(buf) as usize
}

/// Get the cursor position stored in the piece table header.
pub(crate) fn rich_cursor_pos() -> usize {
    let buf = rich_buf_ref();
    piecetable::cursor_pos(buf) as usize
}

/// Set the cursor position in the piece table header.
pub(crate) fn rich_set_cursor_pos(pos: usize) {
    let buf = rich_buf();
    piecetable::set_cursor_pos(buf, pos as u32);
}

/// Write selection range to the piece table header for editor reads.
pub(crate) fn rich_set_selection(start: usize, end: usize) {
    let buf = rich_buf();
    piecetable::set_selection(buf, start as u32, end as u32);
}

/// Advance the piece table operation_id (called at snapshot boundaries).
pub(crate) fn rich_next_operation() -> u32 {
    let buf = rich_buf();
    piecetable::next_operation(buf)
}

/// Extract the logical text of the rich document into a scratch buffer.
/// Returns the number of bytes copied.
pub(crate) fn rich_copy_text(out: &mut [u8]) -> usize {
    let buf = rich_buf_ref();
    let len = piecetable::text_len(buf);
    piecetable::text_slice(buf, 0, len, out)
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
