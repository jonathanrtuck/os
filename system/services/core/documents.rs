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
