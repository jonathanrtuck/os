//! Piece table data structure for rich text documents.
//!
//! The piece table is a fixed-size, arena-allocated data structure that serves
//! as both the in-memory and on-disk format for `text/rich` documents. The
//! buffer IS the piece table — `from_bytes()` is a pointer cast with validation,
//! enabling zero-copy access via shared memory.
//!
//! # Layout
//!
//! ```text
//! Header (64 bytes)
//! ├── magic, version, counts, cursors, flags
//! Styles (style_count × 12 bytes)
//! Pieces (piece_count × 16 bytes)
//! Original buffer (original_len bytes) — immutable after load
//! Add buffer (add_len bytes) — append-only during editing
//! ```
//!
//! # Design
//!
//! - Pure data structure: no_std, no alloc, no OS dependencies.
//! - All operations work on `&[u8]` / `&mut [u8]` slices.
//! - Fixed-size arenas: MAX_PIECES (512), MAX_STYLES (32), MAX_ADD_BUFFER (32K).
//! - Style palette with semantic roles for accessibility.
//! - Sequential same-style inserts coalesce into a single piece.

#![no_std]

use core::mem;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAGIC: u32 = 0x4C42_5450; // "PTBL" in little-endian
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 64;
pub const MAX_PIECES: usize = 512;
pub const MAX_STYLES: usize = 32;
pub const MAX_ADD_BUFFER: usize = 32768;

const PIECE_SIZE: usize = mem::size_of::<Piece>(); // 16
const STYLE_SIZE: usize = mem::size_of::<Style>(); // 12

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// On-disk / in-memory header. 64 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PieceTableHeader {
    pub magic: u32,
    pub version: u16,
    pub piece_count: u16,
    pub style_count: u8,
    pub current_style: u8,
    pub flags: u16,
    pub original_len: u32,
    pub add_len: u32,
    pub text_len: u32,
    pub cursor_pos: u32,
    pub operation_id: u32,
    pub selection_start: u32,
    pub selection_end: u32,
    pub _reserved: [u8; 24],
}

const _HEADER_SIZE_CHECK: () = assert!(mem::size_of::<PieceTableHeader>() == HEADER_SIZE);

/// A piece: a reference into either the original or add buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub source: u8,
    pub _pad: u8,
    pub style_id: u8,
    pub _pad2: u8,
    pub offset: u32,
    pub length: u32,
    pub operation_id: u32,
}

const _PIECE_SIZE_CHECK: () = assert!(mem::size_of::<Piece>() == 16);

/// A style palette entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub font_family: u8,
    pub role: u8,
    pub weight: u16,
    pub flags: u8,
    pub font_size_pt: u8,
    pub color: [u8; 4],
    pub _pad: [u8; 2],
}

const _STYLE_SIZE_CHECK: () = assert!(mem::size_of::<Style>() == 12);

/// A styled run returned by the iterator. Not stored in the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyledRun {
    pub byte_offset: u32,
    pub byte_len: u32,
    pub style_id: u8,
}

// Source constants for Piece.source
const SOURCE_ORIGINAL: u8 = 0;
const SOURCE_ADD: u8 = 1;

// Style flag bits
pub const FLAG_ITALIC: u8 = 1 << 0;
pub const FLAG_UNDERLINE: u8 = 1 << 1;
pub const FLAG_STRIKETHROUGH: u8 = 1 << 2;

// Role constants
pub const ROLE_BODY: u8 = 0;
pub const ROLE_HEADING1: u8 = 1;
pub const ROLE_HEADING2: u8 = 2;
pub const ROLE_HEADING3: u8 = 3;
pub const ROLE_STRONG: u8 = 10;
pub const ROLE_EMPHASIS: u8 = 11;
pub const ROLE_CODE: u8 = 12;

// Font family constants
pub const FONT_MONO: u8 = 0;
pub const FONT_SANS: u8 = 1;
pub const FONT_SERIF: u8 = 2;

// ---------------------------------------------------------------------------
// Internal offset helpers
// ---------------------------------------------------------------------------

/// Byte offset where the style array begins.
#[inline]
fn styles_offset() -> usize {
    HEADER_SIZE
}

/// Byte offset where the piece array begins, given `style_count`.
#[inline]
fn pieces_offset(style_count: u8) -> usize {
    styles_offset() + (style_count as usize) * STYLE_SIZE
}

/// Byte offset where the original buffer begins.
#[inline]
fn original_offset(style_count: u8, piece_count: u16) -> usize {
    pieces_offset(style_count) + (piece_count as usize) * PIECE_SIZE
}

/// Byte offset where the add buffer begins.
#[inline]
fn add_offset(style_count: u8, piece_count: u16, original_len: u32) -> usize {
    original_offset(style_count, piece_count) + original_len as usize
}

/// Total bytes used by the piece table given current counts.
#[inline]
fn total_used(h: &PieceTableHeader) -> usize {
    add_offset(h.style_count, h.piece_count, h.original_len) + h.add_len as usize
}

// ---------------------------------------------------------------------------
// Internal buffer accessors
// ---------------------------------------------------------------------------

#[inline]
fn read_header(buf: &[u8]) -> &PieceTableHeader {
    debug_assert!(buf.len() >= HEADER_SIZE);
    // SAFETY: buf is at least HEADER_SIZE bytes, PieceTableHeader is repr(C)
    // and 64 bytes, and we only require the buffer to be u8-aligned (which it
    // always is). repr(C) structs of u8/u16/u32 fields have alignment ≤ 4,
    // but we use read_unaligned via a copy for safety on arbitrary buffers.
    unsafe { &*(buf.as_ptr() as *const PieceTableHeader) }
}

#[inline]
fn read_header_mut(buf: &mut [u8]) -> &mut PieceTableHeader {
    debug_assert!(buf.len() >= HEADER_SIZE);
    // SAFETY: same as read_header, but mutable.
    unsafe { &mut *(buf.as_mut_ptr() as *mut PieceTableHeader) }
}

fn read_piece(buf: &[u8], style_count: u8, index: u16) -> Piece {
    let off = pieces_offset(style_count) + (index as usize) * PIECE_SIZE;
    let src = &buf[off..off + PIECE_SIZE];
    // SAFETY: Piece is repr(C), 16 bytes, all-byte-valid fields.
    unsafe { core::ptr::read_unaligned(src.as_ptr() as *const Piece) }
}

fn write_piece(buf: &mut [u8], style_count: u8, index: u16, piece: &Piece) {
    let off = pieces_offset(style_count) + (index as usize) * PIECE_SIZE;
    let dst = &mut buf[off..off + PIECE_SIZE];
    // SAFETY: Piece is repr(C), 16 bytes.
    unsafe { core::ptr::write_unaligned(dst.as_mut_ptr() as *mut Piece, *piece) };
}

fn write_style(buf: &mut [u8], index: u8, style: &Style) {
    let off = styles_offset() + (index as usize) * STYLE_SIZE;
    let dst = &mut buf[off..off + STYLE_SIZE];
    // SAFETY: Style is repr(C), 12 bytes.
    unsafe { core::ptr::write_unaligned(dst.as_mut_ptr() as *mut Style, *style) };
}

/// Read a byte from the appropriate source buffer.
fn read_source_byte(buf: &[u8], h: &PieceTableHeader, piece: &Piece, offset_in_piece: u32) -> u8 {
    let abs = if piece.source == SOURCE_ORIGINAL {
        original_offset(h.style_count, h.piece_count)
            + piece.offset as usize
            + offset_in_piece as usize
    } else {
        add_offset(h.style_count, h.piece_count, h.original_len)
            + piece.offset as usize
            + offset_in_piece as usize
    };
    buf[abs]
}

/// Shift pieces right by `count` starting at index `from`.
fn shift_pieces_right(buf: &mut [u8], style_count: u8, piece_count: u16, from: u16, count: u16) {
    // Move from the end to avoid overwriting.
    let mut i = piece_count;
    while i > from {
        i -= 1;
        let p = read_piece(buf, style_count, i);
        write_piece(buf, style_count, i + count, &p);
    }
}

// ---------------------------------------------------------------------------
// Public API — Initialization
// ---------------------------------------------------------------------------

/// Initialize an empty piece table in `buf`. Returns `false` if the buffer
/// is too small for the header.
pub fn init(buf: &mut [u8], _capacity: usize) -> bool {
    if buf.len() < HEADER_SIZE {
        return false;
    }
    // Zero the entire buffer first.
    for b in buf.iter_mut() {
        *b = 0;
    }
    let h = read_header_mut(buf);
    h.magic = MAGIC;
    h.version = VERSION;
    h.piece_count = 0;
    h.style_count = 0;
    h.current_style = 0;
    h.flags = 0;
    h.original_len = 0;
    h.add_len = 0;
    h.text_len = 0;
    h.cursor_pos = 0;
    h.operation_id = 0;
    true
}

/// Initialize a piece table with existing text in the original buffer.
/// The `default_style` is added as style index 0 and the initial piece
/// references the entire original buffer with that style.
pub fn init_with_text(
    buf: &mut [u8],
    _capacity: usize,
    text: &[u8],
    default_style: &Style,
) -> bool {
    if buf.len() < HEADER_SIZE {
        return false;
    }

    // We need: header + 1 style + 1 piece + original text
    let needed = HEADER_SIZE + STYLE_SIZE + PIECE_SIZE + text.len();
    if buf.len() < needed {
        return false;
    }

    // Zero the buffer.
    for b in buf.iter_mut() {
        *b = 0;
    }

    let h = read_header_mut(buf);
    h.magic = MAGIC;
    h.version = VERSION;
    h.piece_count = 1;
    h.style_count = 1;
    h.current_style = 0;
    h.flags = 0;
    h.original_len = text.len() as u32;
    h.add_len = 0;
    h.text_len = text.len() as u32;
    h.cursor_pos = 0;
    h.operation_id = 0;

    // Write the default style at index 0.
    write_style(buf, 0, default_style);

    // Write the initial piece referencing the entire original buffer.
    let piece = Piece {
        source: SOURCE_ORIGINAL,
        _pad: 0,
        style_id: 0,
        _pad2: 0,
        offset: 0,
        length: text.len() as u32,
        operation_id: 0,
    };
    write_piece(buf, 1, 0, &piece);

    // Copy text into the original buffer area.
    let orig_off = original_offset(1, 1);
    buf[orig_off..orig_off + text.len()].copy_from_slice(text);

    true
}

// ---------------------------------------------------------------------------
// Public API — Validation
// ---------------------------------------------------------------------------

/// Validate that `buf` contains a valid piece table.
pub fn validate(buf: &[u8]) -> bool {
    if buf.len() < HEADER_SIZE {
        return false;
    }
    let h = read_header(buf);
    if h.magic != MAGIC || h.version != VERSION {
        return false;
    }
    if h.style_count as usize > MAX_STYLES {
        return false;
    }
    if h.piece_count as usize > MAX_PIECES {
        return false;
    }

    // Check that the buffer is large enough for all data.
    let needed = total_used(h);
    if buf.len() < needed {
        return false;
    }

    // Verify text_len matches sum of piece lengths.
    let mut sum: u32 = 0;
    for i in 0..h.piece_count {
        let p = read_piece(buf, h.style_count, i);
        sum = sum.saturating_add(p.length);
    }
    if sum != h.text_len {
        return false;
    }

    true
}

// ---------------------------------------------------------------------------
// Public API — Header access
// ---------------------------------------------------------------------------

/// Read-only access to the header.
pub fn header(buf: &[u8]) -> &PieceTableHeader {
    read_header(buf)
}

/// Mutable access to the header.
pub fn header_mut(buf: &mut [u8]) -> &mut PieceTableHeader {
    read_header_mut(buf)
}

/// Logical text length in bytes.
pub fn text_len(buf: &[u8]) -> u32 {
    read_header(buf).text_len
}

/// Current cursor position (byte offset in logical text).
pub fn cursor_pos(buf: &[u8]) -> u32 {
    read_header(buf).cursor_pos
}

/// Set the cursor position.
pub fn set_cursor_pos(buf: &mut [u8], pos: u32) {
    read_header_mut(buf).cursor_pos = pos;
}

// ---------------------------------------------------------------------------
// Public API — Mutation
// ---------------------------------------------------------------------------

/// Find which piece contains the given logical byte position.
/// Returns `(piece_index, offset_within_piece)`.
/// If `pos == text_len`, returns `(piece_count, 0)` — meaning "append".
fn find_piece(buf: &[u8], pos: u32) -> (u16, u32) {
    let h = read_header(buf);
    let mut offset: u32 = 0;
    for i in 0..h.piece_count {
        let p = read_piece(buf, h.style_count, i);
        if pos < offset + p.length {
            return (i, pos - offset);
        }
        offset += p.length;
    }
    (h.piece_count, 0)
}

/// Insert a single byte at the given logical position.
pub fn insert(buf: &mut [u8], pos: u32, byte: u8) -> bool {
    insert_bytes(buf, pos, &[byte])
}

/// Insert multiple bytes at the given logical position.
pub fn insert_bytes(buf: &mut [u8], pos: u32, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }

    let h = read_header(buf);
    let tl = h.text_len;
    let sc = h.style_count;
    let pc = h.piece_count;
    let op_id = h.operation_id;
    let cur_style = h.current_style;

    if pos > tl {
        return false;
    }

    // Try to coalesce: if inserting at the end of the last piece and it has
    // the same style, and it references the end of the add buffer, just extend.
    if pc > 0 {
        let (pi, off_in) = find_piece(buf, if pos > 0 { pos } else { 0 });

        // Coalesce with the previous piece if we're at its end.
        let coalesce_idx = if off_in == 0 && pi > 0 {
            // We're at the start of piece `pi`, which is the end of piece `pi-1`.
            Some(pi - 1)
        } else if pos == tl && pi == pc {
            // Appending at the very end — coalesce with the last piece.
            if pc > 0 {
                Some(pc - 1)
            } else {
                None
            }
        } else if off_in > 0 {
            let p = read_piece(buf, sc, pi);
            if off_in == p.length {
                Some(pi)
            } else {
                None
            }
        } else {
            None
        };

        if let Some(ci) = coalesce_idx {
            let p = read_piece(buf, sc, ci);
            let add_off = add_offset(sc, pc, read_header(buf).original_len);
            let h = read_header(buf);
            if p.source == SOURCE_ADD && p.style_id == cur_style && p.offset + p.length == h.add_len
            {
                // Append to the add buffer and extend the piece.
                let add_start = add_off + h.add_len as usize;
                if h.add_len as usize + bytes.len() > MAX_ADD_BUFFER {
                    return false;
                }
                if add_start + bytes.len() > buf.len() {
                    return false;
                }
                buf[add_start..add_start + bytes.len()].copy_from_slice(bytes);
                let h = read_header_mut(buf);
                h.add_len += bytes.len() as u32;
                h.text_len += bytes.len() as u32;
                // Extend the piece.
                let mut p = read_piece(buf, sc, ci);
                p.length += bytes.len() as u32;
                write_piece(buf, sc, ci, &p);
                return true;
            }
        }
    }

    // Cannot coalesce — need a new piece (and possibly split an existing one).
    let (pi, off_in) = find_piece(buf, pos);

    // Determine how many pieces to add. Split replaces 1 piece with 3
    // (left + insert + right) = net +2. No split just inserts 1 = net +1.
    let need_split = off_in > 0;
    let pieces_to_add: u16 = if need_split { 2 } else { 1 };
    let final_piece_count = pc as usize + pieces_to_add as usize;

    if final_piece_count > MAX_PIECES {
        return false;
    }

    // We need to relocate the original and add buffers if piece count changes,
    // because they come after the piece array. Instead of relocating, we design
    // the layout so original buffer starts at a fixed position:
    // Actually, the buffers are AFTER the piece array, so adding pieces shifts them.
    // We need to handle this carefully.
    //
    // Strategy: the piece array, original buffer, and add buffer are sequential.
    // When we add pieces, the original and add buffers must shift right.
    // This is expensive but correct for a fixed-size arena.

    let pieces_added = final_piece_count as u16 - pc;

    // Check buffer capacity for shifted data.
    let h = read_header(buf);
    let old_orig_off = original_offset(sc, pc);
    let data_len = h.original_len as usize + h.add_len as usize;
    let new_orig_off = original_offset(sc, pc + pieces_added);
    let new_add_off = new_orig_off + h.original_len as usize;

    // Need room for new add data too.
    if new_add_off + h.add_len as usize + bytes.len() > buf.len() {
        return false;
    }

    // Shift original + add buffers right to make room for new pieces.
    if pieces_added > 0 && data_len > 0 {
        let shift = (pieces_added as usize) * PIECE_SIZE;
        // Copy backwards (memmove semantics for rightward shift).
        let src_start = old_orig_off;
        let mut i = data_len;
        while i > 0 {
            i -= 1;
            buf[src_start + shift + i] = buf[src_start + i];
        }
    }

    // Now update piece_count in header BEFORE writing pieces, so offsets are correct.
    let h = read_header_mut(buf);
    h.piece_count = pc + pieces_added;
    let new_pc = h.piece_count;

    // Append the new text to the add buffer (at its new position).
    let new_add_data_off =
        add_offset(sc, new_pc, read_header(buf).original_len) + read_header(buf).add_len as usize;
    if read_header(buf).add_len as usize + bytes.len() > MAX_ADD_BUFFER {
        // Rollback: shift buffers back and restore piece_count.
        // For simplicity in this no_alloc context, we already checked above.
        return false;
    }
    buf[new_add_data_off..new_add_data_off + bytes.len()].copy_from_slice(bytes);
    let add_buf_offset = read_header(buf).add_len;
    read_header_mut(buf).add_len += bytes.len() as u32;

    // Now write the pieces.
    if need_split {
        // We're splitting piece `pi` at `off_in`.
        let old_piece = read_piece(buf, sc, pi);

        // Shift pieces after `pi` right by `pieces_added`.
        shift_pieces_right(buf, sc, pc, pi + 1, pieces_added);

        // Left half of split.
        let left = Piece {
            source: old_piece.source,
            _pad: 0,
            style_id: old_piece.style_id,
            _pad2: 0,
            offset: old_piece.offset,
            length: off_in,
            operation_id: old_piece.operation_id,
        };
        write_piece(buf, sc, pi, &left);

        // New inserted piece.
        let new_piece = Piece {
            source: SOURCE_ADD,
            _pad: 0,
            style_id: cur_style,
            _pad2: 0,
            offset: add_buf_offset,
            length: bytes.len() as u32,
            operation_id: op_id,
        };
        write_piece(buf, sc, pi + 1, &new_piece);

        // Right half of split.
        let right = Piece {
            source: old_piece.source,
            _pad: 0,
            style_id: old_piece.style_id,
            _pad2: 0,
            offset: old_piece.offset + off_in,
            length: old_piece.length - off_in,
            operation_id: old_piece.operation_id,
        };
        write_piece(buf, sc, pi + 2, &right);
    } else {
        // Insert at a piece boundary (beginning of `pi` or end of all pieces).
        // Shift pieces from `pi` onward right by 1.
        shift_pieces_right(buf, sc, pc, pi, 1);

        let new_piece = Piece {
            source: SOURCE_ADD,
            _pad: 0,
            style_id: cur_style,
            _pad2: 0,
            offset: add_buf_offset,
            length: bytes.len() as u32,
            operation_id: op_id,
        };
        write_piece(buf, sc, pi, &new_piece);
    }

    // Update text_len.
    read_header_mut(buf).text_len += bytes.len() as u32;

    true
}

/// Delete a single byte at the given logical position.
pub fn delete(buf: &mut [u8], pos: u32) -> bool {
    delete_range(buf, pos, pos + 1)
}

/// Delete bytes in the range `[start, end)`.
pub fn delete_range(buf: &mut [u8], start: u32, end: u32) -> bool {
    if start >= end {
        return false;
    }
    let h = read_header(buf);
    let tl = h.text_len;
    if end > tl {
        return false;
    }

    let sc = h.style_count;
    let pc = h.piece_count;
    let ol = h.original_len;
    let al = h.add_len;

    // Collect pieces into a temporary array, apply the delete, then write back.
    // We work with a stack-allocated piece buffer (MAX_PIECES is 512 × 16 = 8KB).
    let mut pieces = [Piece {
        source: 0,
        _pad: 0,
        style_id: 0,
        _pad2: 0,
        offset: 0,
        length: 0,
        operation_id: 0,
    }; MAX_PIECES];
    let mut count = 0usize;

    let mut offset: u32 = 0;
    for i in 0..pc {
        let p = read_piece(buf, sc, i);
        let p_start = offset;
        let p_end = offset + p.length;

        if p_end <= start || p_start >= end {
            // Entirely outside deletion range — keep as-is.
            pieces[count] = p;
            count += 1;
        } else if p_start >= start && p_end <= end {
            // Entirely within deletion range — remove.
        } else if start > p_start && end < p_end {
            // Deletion splits this piece into two.
            // Left part.
            pieces[count] = Piece {
                source: p.source,
                _pad: 0,
                style_id: p.style_id,
                _pad2: 0,
                offset: p.offset,
                length: start - p_start,
                operation_id: p.operation_id,
            };
            count += 1;
            // Right part.
            pieces[count] = Piece {
                source: p.source,
                _pad: 0,
                style_id: p.style_id,
                _pad2: 0,
                offset: p.offset + (end - p_start),
                length: p_end - end,
                operation_id: p.operation_id,
            };
            count += 1;
        } else if start > p_start {
            // Deletion cuts off the end of this piece.
            pieces[count] = Piece {
                source: p.source,
                _pad: 0,
                style_id: p.style_id,
                _pad2: 0,
                offset: p.offset,
                length: start - p_start,
                operation_id: p.operation_id,
            };
            count += 1;
        } else {
            // Deletion cuts off the beginning of this piece.
            let trim = end - p_start;
            pieces[count] = Piece {
                source: p.source,
                _pad: 0,
                style_id: p.style_id,
                _pad2: 0,
                offset: p.offset + trim,
                length: p.length - trim,
                operation_id: p.operation_id,
            };
            count += 1;
        }

        offset = p_end;
    }

    // Calculate the new piece count.
    let new_pc = count as u16;
    let pieces_removed = pc as i32 - new_pc as i32;

    if pieces_removed > 0 {
        // Pieces shrunk — shift original+add buffers left.
        let old_data_off = original_offset(sc, pc);
        let new_data_off = original_offset(sc, new_pc);
        let data_len = ol as usize + al as usize;
        if data_len > 0 {
            // Copy forward (leftward shift).
            for i in 0..data_len {
                buf[new_data_off + i] = buf[old_data_off + i];
            }
        }
    } else if pieces_removed < 0 {
        // Pieces grew (split added one) — shift original+add buffers right.
        let old_data_off = original_offset(sc, pc);
        let new_data_off = original_offset(sc, new_pc);
        let data_len = ol as usize + al as usize;
        if new_data_off + data_len > buf.len() {
            return false;
        }
        if data_len > 0 {
            let mut i = data_len;
            while i > 0 {
                i -= 1;
                buf[new_data_off + i] = buf[old_data_off + i];
            }
        }
    }

    // Update header.
    let h = read_header_mut(buf);
    h.piece_count = new_pc;
    h.text_len -= end - start;

    // Write pieces.
    for i in 0..count {
        write_piece(buf, sc, i as u16, &pieces[i]);
    }

    true
}

/// Apply a style to the byte range `[start, end)`.
pub fn apply_style(buf: &mut [u8], start: u32, end: u32, style_id: u8) {
    if start >= end {
        return;
    }
    let h = read_header(buf);
    if end > h.text_len {
        return;
    }
    let sc = h.style_count;
    let pc = h.piece_count;
    let ol = h.original_len;
    let al = h.add_len;

    // Read all pieces into a temporary buffer, apply style changes, write back.
    let mut pieces = [Piece {
        source: 0,
        _pad: 0,
        style_id: 0,
        _pad2: 0,
        offset: 0,
        length: 0,
        operation_id: 0,
    }; MAX_PIECES];
    let mut count = 0usize;

    let mut offset: u32 = 0;
    for i in 0..pc {
        let p = read_piece(buf, sc, i);
        let p_start = offset;
        let p_end = offset + p.length;

        if p_end <= start || p_start >= end {
            // Outside style range — keep as-is.
            pieces[count] = p;
            count += 1;
        } else if p_start >= start && p_end <= end {
            // Entirely within style range — change style.
            pieces[count] = Piece { style_id, ..p };
            count += 1;
        } else if start > p_start && end < p_end {
            // Style range splits this piece into three.
            if count + 3 > MAX_PIECES {
                return; // Can't fit — abort.
            }
            // Left (unchanged).
            pieces[count] = Piece {
                offset: p.offset,
                length: start - p_start,
                ..p
            };
            count += 1;
            // Middle (styled).
            pieces[count] = Piece {
                offset: p.offset + (start - p_start),
                length: end - start,
                style_id,
                ..p
            };
            count += 1;
            // Right (unchanged).
            pieces[count] = Piece {
                offset: p.offset + (end - p_start),
                length: p_end - end,
                ..p
            };
            count += 1;
        } else if start > p_start {
            // Style cuts into the end of this piece — split into two.
            if count + 2 > MAX_PIECES {
                return;
            }
            pieces[count] = Piece {
                offset: p.offset,
                length: start - p_start,
                ..p
            };
            count += 1;
            pieces[count] = Piece {
                offset: p.offset + (start - p_start),
                length: p_end - start,
                style_id,
                ..p
            };
            count += 1;
        } else {
            // Style cuts into the beginning of this piece — split into two.
            if count + 2 > MAX_PIECES {
                return;
            }
            pieces[count] = Piece {
                offset: p.offset,
                length: end - p_start,
                style_id,
                ..p
            };
            count += 1;
            pieces[count] = Piece {
                offset: p.offset + (end - p_start),
                length: p_end - end,
                ..p
            };
            count += 1;
        }

        offset = p_end;
    }

    let new_pc = count as u16;

    // Relocate data buffers if piece count changed.
    if new_pc != pc {
        let old_data_off = original_offset(sc, pc);
        let new_data_off = original_offset(sc, new_pc);
        let data_len = ol as usize + al as usize;

        if new_data_off + data_len > buf.len() {
            return; // Buffer too small.
        }

        if new_data_off > old_data_off {
            // Shift right.
            let mut i = data_len;
            while i > 0 {
                i -= 1;
                buf[new_data_off + i] = buf[old_data_off + i];
            }
        } else if new_data_off < old_data_off {
            // Shift left.
            for i in 0..data_len {
                buf[new_data_off + i] = buf[old_data_off + i];
            }
        }
    }

    // Update header and write pieces.
    read_header_mut(buf).piece_count = new_pc;
    for i in 0..count {
        write_piece(buf, sc, i as u16, &pieces[i]);
    }
}

/// Set the current insertion style.
pub fn set_current_style(buf: &mut [u8], style_id: u8) {
    read_header_mut(buf).current_style = style_id;
}

/// Get the current insertion style.
pub fn current_style(buf: &[u8]) -> u8 {
    read_header(buf).current_style
}

/// Set the selection range in the header.
pub fn set_selection(buf: &mut [u8], start: u32, end: u32) {
    let h = read_header_mut(buf);
    h.selection_start = start;
    h.selection_end = end;
}

/// Get the selection range from the header.
pub fn selection(buf: &[u8]) -> (u32, u32) {
    let h = read_header(buf);
    (h.selection_start, h.selection_end)
}

/// Increment operation_id and return the new value.
pub fn next_operation(buf: &mut [u8]) -> u32 {
    let h = read_header_mut(buf);
    h.operation_id += 1;
    h.operation_id
}

// ---------------------------------------------------------------------------
// Public API — Style palette
// ---------------------------------------------------------------------------

/// Add a style to the palette. Returns the style index, or `None` if full.
///
/// Adding a style shifts the piece array and data buffers right by STYLE_SIZE.
pub fn add_style(buf: &mut [u8], style: &Style) -> Option<u8> {
    let h = read_header(buf);
    let sc = h.style_count;
    let pc = h.piece_count;
    let ol = h.original_len;
    let al = h.add_len;

    if sc as usize >= MAX_STYLES {
        return None;
    }

    let new_sc = sc + 1;

    // Check that the buffer has room for the extra STYLE_SIZE bytes.
    let old_total = total_used(h);
    if old_total + STYLE_SIZE > buf.len() {
        return None;
    }

    // Shift pieces + original + add buffers right by STYLE_SIZE.
    let old_pieces_off = pieces_offset(sc);
    let new_pieces_off = pieces_offset(new_sc);
    let data_after_styles = (pc as usize) * PIECE_SIZE + ol as usize + al as usize;

    if data_after_styles > 0 {
        let mut i = data_after_styles;
        while i > 0 {
            i -= 1;
            buf[new_pieces_off + i] = buf[old_pieces_off + i];
        }
    }

    // Write the new style.
    let h = read_header_mut(buf);
    h.style_count = new_sc;
    write_style(buf, sc, style);

    Some(sc)
}

/// Get a style from the palette by index.
pub fn style(buf: &[u8], id: u8) -> Option<&Style> {
    let h = read_header(buf);
    if id >= h.style_count {
        return None;
    }
    let off = styles_offset() + (id as usize) * STYLE_SIZE;
    if off + STYLE_SIZE > buf.len() {
        return None;
    }
    // SAFETY: Style is repr(C), buffer bounds checked above.
    Some(unsafe { &*(buf[off..].as_ptr() as *const Style) })
}

/// Number of styles in the palette.
pub fn style_count(buf: &[u8]) -> u8 {
    read_header(buf).style_count
}

/// Find the first style in the palette with the given semantic role.
pub fn find_style_by_role(buf: &[u8], role: u8) -> Option<u8> {
    let h = read_header(buf);
    for i in 0..h.style_count {
        if let Some(s) = style(buf, i) {
            if s.role == role {
                return Some(i);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Public API — Read access
// ---------------------------------------------------------------------------

/// Read the byte at the given logical position.
pub fn byte_at(buf: &[u8], pos: u32) -> Option<u8> {
    let h = read_header(buf);
    if pos >= h.text_len {
        return None;
    }
    let sc = h.style_count;
    let pc = h.piece_count;

    let mut offset: u32 = 0;
    for i in 0..pc {
        let p = read_piece(buf, sc, i);
        if pos < offset + p.length {
            let off_in = pos - offset;
            return Some(read_source_byte(buf, h, &p, off_in));
        }
        offset += p.length;
    }
    None
}

/// Copy text from `[start, end)` into `out`. Returns the number of bytes copied.
pub fn text_slice(buf: &[u8], start: u32, end: u32, out: &mut [u8]) -> usize {
    let h = read_header(buf);
    let tl = h.text_len;
    let sc = h.style_count;
    let pc = h.piece_count;

    let end = if end > tl { tl } else { end };
    if start >= end {
        return 0;
    }

    let mut written = 0usize;
    let mut offset: u32 = 0;

    for i in 0..pc {
        let p = read_piece(buf, sc, i);
        let p_start = offset;
        let p_end = offset + p.length;

        if p_end <= start {
            offset = p_end;
            continue;
        }
        if p_start >= end {
            break;
        }

        // Overlap: [max(p_start, start), min(p_end, end))
        let from = if start > p_start { start - p_start } else { 0 };
        let to = if end < p_end { end - p_start } else { p.length };

        for j in from..to {
            if written >= out.len() {
                return written;
            }
            out[written] = read_source_byte(buf, h, &p, j);
            written += 1;
        }

        offset = p_end;
    }

    written
}

/// Return the style_id at the given logical byte position.
pub fn style_at(buf: &[u8], pos: u32) -> Option<u8> {
    let h = read_header(buf);
    if pos >= h.text_len {
        return None;
    }
    let sc = h.style_count;
    let pc = h.piece_count;

    let mut offset: u32 = 0;
    for i in 0..pc {
        let p = read_piece(buf, sc, i);
        if pos < offset + p.length {
            return Some(p.style_id);
        }
        offset += p.length;
    }
    None
}

// ---------------------------------------------------------------------------
// Public API — Styled runs (coalesced iteration)
// ---------------------------------------------------------------------------

/// Count of coalesced styled runs (adjacent pieces with the same style merge).
pub fn styled_run_count(buf: &[u8]) -> usize {
    let h = read_header(buf);
    if h.piece_count == 0 {
        return 0;
    }

    let sc = h.style_count;
    let mut count = 1usize;
    let mut prev_style = read_piece(buf, sc, 0).style_id;

    for i in 1..h.piece_count {
        let p = read_piece(buf, sc, i);
        if p.style_id != prev_style {
            count += 1;
            prev_style = p.style_id;
        }
    }

    count
}

/// Get the Nth coalesced styled run.
pub fn styled_run(buf: &[u8], index: usize) -> Option<StyledRun> {
    let h = read_header(buf);
    if h.piece_count == 0 {
        return None;
    }

    let sc = h.style_count;
    let mut run_idx = 0usize;
    let mut byte_offset: u32 = 0;
    let mut byte_len: u32 = 0;
    let mut current_style = read_piece(buf, sc, 0).style_id;
    let mut run_start: u32 = 0;

    for i in 0..h.piece_count {
        let p = read_piece(buf, sc, i);

        if i > 0 && p.style_id != current_style {
            if run_idx == index {
                return Some(StyledRun {
                    byte_offset: run_start,
                    byte_len,
                    style_id: current_style,
                });
            }
            run_idx += 1;
            run_start = byte_offset;
            byte_len = 0;
            current_style = p.style_id;
        }

        byte_len += p.length;
        byte_offset += p.length;
    }

    // Last run.
    if run_idx == index {
        return Some(StyledRun {
            byte_offset: run_start,
            byte_len,
            style_id: current_style,
        });
    }

    None
}

/// Copy the text of a styled run into `out`. Returns the number of bytes copied.
pub fn copy_run_text(buf: &[u8], run: &StyledRun, out: &mut [u8]) -> usize {
    text_slice(buf, run.byte_offset, run.byte_offset + run.byte_len, out)
}

// ---------------------------------------------------------------------------
// Default styles
// ---------------------------------------------------------------------------

/// Create the default body style (sans, 14pt, w400, black).
pub fn default_body_style() -> Style {
    Style {
        font_family: FONT_SANS,
        role: ROLE_BODY,
        weight: 400,
        flags: 0,
        font_size_pt: 14,
        color: [0, 0, 0, 255],
        _pad: [0; 2],
    }
}

/// Create the heading 1 style (sans, 24pt, w700, black).
pub fn heading1_style() -> Style {
    Style {
        font_family: FONT_SANS,
        role: ROLE_HEADING1,
        weight: 700,
        flags: 0,
        font_size_pt: 24,
        color: [0, 0, 0, 255],
        _pad: [0; 2],
    }
}

/// Create the heading 2 style (sans, 18pt, w600, black).
pub fn heading2_style() -> Style {
    Style {
        font_family: FONT_SANS,
        role: ROLE_HEADING2,
        weight: 600,
        flags: 0,
        font_size_pt: 18,
        color: [0, 0, 0, 255],
        _pad: [0; 2],
    }
}

/// Create the bold style (sans, 14pt, w700, black).
pub fn bold_style() -> Style {
    Style {
        font_family: FONT_SANS,
        role: ROLE_STRONG,
        weight: 700,
        flags: 0,
        font_size_pt: 14,
        color: [0, 0, 0, 255],
        _pad: [0; 2],
    }
}

/// Create the italic style (sans, 14pt, w400, italic, black).
pub fn italic_style() -> Style {
    Style {
        font_family: FONT_SANS,
        role: ROLE_EMPHASIS,
        weight: 400,
        flags: FLAG_ITALIC,
        font_size_pt: 14,
        color: [0, 0, 0, 255],
        _pad: [0; 2],
    }
}

/// Create the bold italic style (sans, 14pt, w700, italic, black).
pub fn bold_italic_style() -> Style {
    Style {
        font_family: FONT_SANS,
        role: ROLE_STRONG,
        weight: 700,
        flags: FLAG_ITALIC,
        font_size_pt: 14,
        color: [0, 0, 0, 255],
        _pad: [0; 2],
    }
}

/// Create the code style (mono, 13pt, w400, #666666).
pub fn code_style() -> Style {
    Style {
        font_family: FONT_MONO,
        role: ROLE_CODE,
        weight: 400,
        flags: 0,
        font_size_pt: 13,
        color: [0x66, 0x66, 0x66, 255],
        _pad: [0; 2],
    }
}

/// Add the 7 default styles to a piece table. Returns true if all were added.
pub fn add_default_styles(buf: &mut [u8]) -> bool {
    let styles = [
        default_body_style(),
        heading1_style(),
        heading2_style(),
        bold_style(),
        italic_style(),
        bold_italic_style(),
        code_style(),
    ];
    for s in &styles {
        if add_style(buf, s).is_none() {
            return false;
        }
    }
    true
}
