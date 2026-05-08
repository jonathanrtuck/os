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
//! - All limits derived from `buf.len()` — the buffer is the single source of truth.
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

/// Shift the data region (original + add buffers) when piece count changes.
/// `old_pc` is the piece count before the change, `new_pc` after.
fn relocate_data(buf: &mut [u8], sc: u8, old_pc: u16, new_pc: u16, ol: u32, al: u32) -> bool {
    let old_data_off = original_offset(sc, old_pc);
    let new_data_off = original_offset(sc, new_pc);
    let data_len = ol as usize + al as usize;

    if data_len == 0 || old_data_off == new_data_off {
        return true;
    }

    if new_data_off + data_len > buf.len() {
        return false;
    }

    if new_data_off > old_data_off {
        // Shift right — copy backward.
        let mut i = data_len;
        while i > 0 {
            i -= 1;
            buf[new_data_off + i] = buf[old_data_off + i];
        }
    } else {
        // Shift left — copy forward.
        for i in 0..data_len {
            buf[new_data_off + i] = buf[old_data_off + i];
        }
    }
    true
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
            if pc > 0 { Some(pc - 1) } else { None }
        } else if off_in > 0 {
            let p = read_piece(buf, sc, pi);
            if off_in == p.length { Some(pi) } else { None }
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

    // Check piece count fits in u16.
    if final_piece_count > u16::MAX as usize {
        return false;
    }

    let pieces_added = final_piece_count as u16 - pc;

    // Check buffer capacity for shifted data + new bytes.
    let h = read_header(buf);
    let new_add_off = add_offset(sc, pc + pieces_added, h.original_len);

    // Need room for existing add data + new bytes.
    if new_add_off + h.add_len as usize + bytes.len() > buf.len() {
        return false;
    }

    // Shift original + add buffers right to make room for new pieces.
    let old_orig_off = original_offset(sc, pc);
    let data_len = h.original_len as usize + h.add_len as usize;
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
///
/// Operates in-place on the piece array — no scratch buffer needed.
/// Uses a two-cursor approach: reads from `ri`, writes to `wi`.
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

    // In-place delete with read/write cursors.
    //
    // A delete can split one piece into two (hole punch), which grows the
    // piece count by 1. But in that case no pieces are fully removed.
    //
    // Detect if any single piece contains both `start` and `end` internally.
    let might_split = {
        let mut offset: u32 = 0;
        let mut found = false;
        for i in 0..pc {
            let p = read_piece(buf, sc, i);
            let p_start = offset;
            let p_end = offset + p.length;
            if start > p_start && end < p_end {
                found = true;
                break;
            }
            offset = p_end;
        }
        found
    };

    if might_split {
        // Need to grow piece array by 1 — check buffer capacity.
        let new_data_off = original_offset(sc, pc + 1);
        if new_data_off + ol as usize + al as usize > buf.len() {
            return false;
        }
        // Shift data right by one piece slot to make room.
        relocate_data(buf, sc, pc, pc + 1, ol, al);
        read_header_mut(buf).piece_count = pc + 1;
    }

    // Now do the in-place piece rewrite with read/write cursors.
    // Iterate over the ORIGINAL piece count — the pre-expansion only added
    // empty slots, not logical pieces.
    let cur_pc = read_header(buf).piece_count; // may be pc+1 after expansion
    let mut wi: u16 = 0; // write index
    let mut offset: u32 = 0;

    for ri in 0..pc {
        let p = read_piece(buf, sc, ri);
        let p_start = offset;
        let p_end = offset + p.length;
        offset = p_end;

        if p_end <= start || p_start >= end {
            // Entirely outside deletion range — keep as-is.
            if wi != ri {
                write_piece(buf, sc, wi, &p);
            }
            wi += 1;
        } else if p_start >= start && p_end <= end {
            // Entirely within deletion range — skip (remove).
        } else if start > p_start && end < p_end {
            // Deletion punches a hole — split into two.
            let left = Piece {
                length: start - p_start,
                ..p
            };
            write_piece(buf, sc, wi, &left);
            wi += 1;
            let right = Piece {
                offset: p.offset + (end - p_start),
                length: p_end - end,
                ..p
            };
            write_piece(buf, sc, wi, &right);
            wi += 1;
        } else if start > p_start {
            // Deletion cuts off the end.
            let trimmed = Piece {
                length: start - p_start,
                ..p
            };
            write_piece(buf, sc, wi, &trimmed);
            wi += 1;
        } else {
            // Deletion cuts off the beginning.
            let trim = end - p_start;
            let trimmed = Piece {
                offset: p.offset + trim,
                length: p.length - trim,
                ..p
            };
            write_piece(buf, sc, wi, &trimmed);
            wi += 1;
        }
    }

    let new_pc = wi;

    // Relocate data buffers to match new piece count.
    if new_pc != cur_pc {
        relocate_data(buf, sc, cur_pc, new_pc, ol, al);
    }

    // Update header.
    let h = read_header_mut(buf);
    h.piece_count = new_pc;
    h.text_len -= end - start;

    true
}

/// Apply a style to the byte range `[start, end)`.
///
/// Operates in-place on the piece array. Splits are handled by first
/// expanding the piece array, then processing right-to-left.
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

    // Count how many extra piece slots we need.
    let mut extra_pieces: u16 = 0;
    {
        let mut offset: u32 = 0;
        for i in 0..pc {
            let p = read_piece(buf, sc, i);
            let p_start = offset;
            let p_end = offset + p.length;
            offset = p_end;

            if p_end <= start || p_start >= end {
                // Outside — no split.
            } else if p_start >= start && p_end <= end {
                // Fully inside — no split, just restyle.
            } else if start > p_start && end < p_end {
                // Both boundaries inside this piece — 1→3 split.
                extra_pieces += 2;
            } else {
                // One boundary inside — 1→2 split.
                extra_pieces += 1;
            }
        }
    }

    if extra_pieces > 0 {
        // Check buffer capacity for expanded piece array.
        let new_data_off = original_offset(sc, pc + extra_pieces);
        if new_data_off + ol as usize + al as usize > buf.len() {
            return; // Buffer too small.
        }
        // Shift data right to accommodate extra pieces.
        relocate_data(buf, sc, pc, pc + extra_pieces, ol, al);
    }

    // Process pieces right-to-left so that rightward placement doesn't
    // clobber unprocessed pieces.
    let mut slots_remaining = extra_pieces;

    let mut i = pc;
    while i > 0 {
        i -= 1;
        let p = read_piece(buf, sc, i);

        // Compute starting offset of piece i by summing pieces 0..i.
        let mut p_start: u32 = 0;
        for j in 0..i {
            p_start += read_piece(buf, sc, j).length;
        }
        let p_end = p_start + p.length;

        // Where this piece lands in the new array.
        let dest = i + slots_remaining;

        if p_end <= start || p_start >= end {
            // Outside range — copy to destination.
            write_piece(buf, sc, dest, &p);
        } else if p_start >= start && p_end <= end {
            // Fully inside — restyle in place.
            write_piece(buf, sc, dest, &Piece { style_id, ..p });
        } else if start > p_start && end < p_end {
            // 1→3 split.
            slots_remaining -= 2;
            let base = i + slots_remaining;
            write_piece(
                buf,
                sc,
                base,
                &Piece {
                    length: start - p_start,
                    ..p
                },
            );
            write_piece(
                buf,
                sc,
                base + 1,
                &Piece {
                    offset: p.offset + (start - p_start),
                    length: end - start,
                    style_id,
                    ..p
                },
            );
            write_piece(
                buf,
                sc,
                base + 2,
                &Piece {
                    offset: p.offset + (end - p_start),
                    length: p_end - end,
                    ..p
                },
            );
        } else if start > p_start {
            // Style cuts into end — 1→2 split.
            slots_remaining -= 1;
            let base = i + slots_remaining;
            write_piece(
                buf,
                sc,
                base,
                &Piece {
                    length: start - p_start,
                    ..p
                },
            );
            write_piece(
                buf,
                sc,
                base + 1,
                &Piece {
                    offset: p.offset + (start - p_start),
                    length: p_end - start,
                    style_id,
                    ..p
                },
            );
        } else {
            // Style cuts into beginning — 1→2 split.
            slots_remaining -= 1;
            let base = i + slots_remaining;
            write_piece(
                buf,
                sc,
                base,
                &Piece {
                    length: end - p_start,
                    style_id,
                    ..p
                },
            );
            write_piece(
                buf,
                sc,
                base + 1,
                &Piece {
                    offset: p.offset + (end - p_start),
                    length: p_end - end,
                    ..p
                },
            );
        }
    }

    // Update piece count.
    read_header_mut(buf).piece_count = pc + extra_pieces;
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
/// The style_count field is u8, so the maximum is 255.
pub fn add_style(buf: &mut [u8], style: &Style) -> Option<u8> {
    let h = read_header(buf);
    let sc = h.style_count;
    let pc = h.piece_count;
    let ol = h.original_len;
    let al = h.add_len;

    // u8 ceiling.
    if sc == 255 {
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
// Public API — Compaction
// ---------------------------------------------------------------------------

/// Compact the piece table by merging adjacent pieces that reference
/// contiguous ranges in the same source buffer with the same style.
///
/// Returns the number of pieces removed. After compaction, the buffer
/// has more free space for new inserts and splits.
pub fn compact(buf: &mut [u8]) -> u16 {
    let h = read_header(buf);
    let sc = h.style_count;
    let pc = h.piece_count;

    if pc <= 1 {
        return 0;
    }

    let ol = h.original_len;
    let al = h.add_len;

    // Forward scan with read/write cursors.
    let mut wi: u16 = 0;
    let mut current = read_piece(buf, sc, 0);

    for ri in 1..pc {
        let next = read_piece(buf, sc, ri);
        if next.source == current.source
            && next.style_id == current.style_id
            && next.offset == current.offset + current.length
        {
            // Merge: extend current piece to cover next.
            current.length += next.length;
        } else {
            // Flush current, advance.
            write_piece(buf, sc, wi, &current);
            wi += 1;
            current = next;
        }
    }
    // Flush last piece.
    write_piece(buf, sc, wi, &current);
    wi += 1;

    let new_pc = wi;
    let removed = pc - new_pc;

    if removed > 0 {
        // Shift data buffers left to reclaim space from removed pieces.
        relocate_data(buf, sc, pc, new_pc, ol, al);
        read_header_mut(buf).piece_count = new_pc;
    }

    removed
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
extern crate alloc;

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::{string::String, vec, vec::Vec};

    use super::*;

    /// Helper: initialize an empty piece table in a freshly allocated buffer.
    fn make_empty(size: usize) -> Vec<u8> {
        let mut buf = vec![0u8; size];
        assert!(init(&mut buf, size));
        buf
    }

    /// Helper: initialize a piece table with text and the default body style.
    fn make_with_text(size: usize, text: &[u8]) -> Vec<u8> {
        let style = default_body_style();
        let mut buf = vec![0u8; size];
        let cap = buf.len();
        assert!(init_with_text(&mut buf, cap, text, &style));
        buf
    }

    /// Helper: extract the full logical text from a piece table into a Vec.
    fn extract_text(buf: &[u8]) -> Vec<u8> {
        let len = text_len(buf) as usize;
        let mut out = vec![0u8; len];
        let copied = text_slice(buf, 0, len as u32, &mut out);
        out.truncate(copied);
        out
    }

    /// Helper: extract the full logical text as a String (assumes ASCII/UTF-8).
    fn extract_string(buf: &[u8]) -> String {
        String::from_utf8(extract_text(buf)).expect("non-UTF-8 text in piece table")
    }

    // -----------------------------------------------------------------------
    // 1. Empty table creation
    // -----------------------------------------------------------------------

    #[test]
    fn init_creates_valid_empty_table() {
        let buf = make_empty(1024);
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 0);
        assert_eq!(cursor_pos(&buf), 0);
        assert_eq!(header(&buf).piece_count, 0);
        assert_eq!(header(&buf).style_count, 0);
    }

    #[test]
    fn init_fails_on_undersized_buffer() {
        let mut buf = vec![0u8; HEADER_SIZE - 1];
        assert!(!init(&mut buf, HEADER_SIZE - 1));
    }

    #[test]
    fn init_minimum_buffer_succeeds() {
        let buf = make_empty(HEADER_SIZE);
        assert!(validate(&buf));
    }

    #[test]
    fn init_with_text_creates_single_piece() {
        let buf = make_with_text(1024, b"Hello, world!");
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 13);
        assert_eq!(header(&buf).piece_count, 1);
        assert_eq!(header(&buf).style_count, 1);
        assert_eq!(extract_string(&buf), "Hello, world!");
    }

    #[test]
    fn init_with_text_fails_on_undersized_buffer() {
        let text = b"Hello";
        let style = default_body_style();
        let needed = HEADER_SIZE + STYLE_SIZE + PIECE_SIZE + text.len();
        let mut buf = vec![0u8; needed - 1];
        let cap = buf.len();
        assert!(!init_with_text(&mut buf, cap, text, &style));
    }

    #[test]
    fn empty_table_extract_yields_empty() {
        let buf = make_empty(1024);
        assert_eq!(extract_text(&buf), Vec::<u8>::new());
    }

    // -----------------------------------------------------------------------
    // 2. Insert operations
    // -----------------------------------------------------------------------

    #[test]
    fn insert_single_byte_into_empty_table() {
        let mut buf = make_empty(1024);
        assert!(insert(&mut buf, 0, b'A'));
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 1);
        assert_eq!(byte_at(&buf, 0), Some(b'A'));
    }

    #[test]
    fn insert_bytes_at_beginning() {
        let mut buf = make_with_text(4096, b"world");
        assert!(insert_bytes(&mut buf, 0, b"Hello "));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello world");
    }

    #[test]
    fn insert_bytes_at_end() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(insert_bytes(&mut buf, 5, b" world"));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello world");
    }

    #[test]
    fn insert_bytes_in_middle() {
        let mut buf = make_with_text(4096, b"Heo");
        assert!(insert_bytes(&mut buf, 2, b"ll"));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello");
    }

    #[test]
    fn insert_empty_bytes_is_noop() {
        let mut buf = make_with_text(4096, b"Hello");
        let pc_before = header(&buf).piece_count;
        assert!(insert_bytes(&mut buf, 2, b""));
        assert_eq!(header(&buf).piece_count, pc_before);
        assert_eq!(extract_string(&buf), "Hello");
    }

    #[test]
    fn insert_past_end_fails() {
        let mut buf = make_empty(1024);
        assert!(!insert(&mut buf, 1, b'A'));
    }

    #[test]
    fn sequential_inserts_coalesce() {
        let mut buf = make_empty(4096);
        // Insert 'H', 'e', 'l', 'l', 'o' one at a time at the end.
        for (i, &ch) in b"Hello".iter().enumerate() {
            assert!(insert(&mut buf, i as u32, ch));
        }
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello");
        // Sequential appends with the same style should coalesce into 1 piece.
        assert_eq!(header(&buf).piece_count, 1);
    }

    #[test]
    fn insert_into_empty_then_read_each_byte() {
        let mut buf = make_empty(4096);
        assert!(insert_bytes(&mut buf, 0, b"ABCDE"));
        for (i, &ch) in b"ABCDE".iter().enumerate() {
            assert_eq!(byte_at(&buf, i as u32), Some(ch));
        }
    }

    // -----------------------------------------------------------------------
    // 3. Delete operations
    // -----------------------------------------------------------------------

    #[test]
    fn delete_single_byte_from_beginning() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(delete(&mut buf, 0));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "ello");
    }

    #[test]
    fn delete_single_byte_from_end() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(delete(&mut buf, 4));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hell");
    }

    #[test]
    fn delete_single_byte_from_middle() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(delete(&mut buf, 2));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Helo");
    }

    #[test]
    fn delete_range_removes_substring() {
        let mut buf = make_with_text(4096, b"Hello, world!");
        // Delete ", world" (positions 5..12).
        assert!(delete_range(&mut buf, 5, 12));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello!");
    }

    #[test]
    fn delete_entire_content() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(delete_range(&mut buf, 0, 5));
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 0);
        assert_eq!(extract_text(&buf), Vec::<u8>::new());
    }

    #[test]
    fn delete_range_past_end_fails() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(!delete_range(&mut buf, 0, 6));
    }

    #[test]
    fn delete_empty_range_fails() {
        let mut buf = make_with_text(4096, b"Hello");
        // start >= end should return false.
        assert!(!delete_range(&mut buf, 3, 3));
        assert!(!delete_range(&mut buf, 4, 3));
    }

    #[test]
    fn delete_from_empty_table_fails() {
        let mut buf = make_empty(1024);
        assert!(!delete(&mut buf, 0));
    }

    // -----------------------------------------------------------------------
    // 4. Sequential edits
    // -----------------------------------------------------------------------

    #[test]
    fn insert_then_delete_restores_original() {
        let mut buf = make_with_text(4096, b"Hello");
        // Insert "XX" at position 2.
        assert!(insert_bytes(&mut buf, 2, b"XX"));
        assert_eq!(extract_string(&buf), "HeXXllo");
        // Delete those two characters back.
        assert!(delete_range(&mut buf, 2, 4));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello");
    }

    #[test]
    fn multiple_inserts_at_different_positions() {
        let mut buf = make_empty(4096);
        assert!(insert_bytes(&mut buf, 0, b"AC"));
        assert!(insert_bytes(&mut buf, 1, b"B"));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "ABC");
    }

    #[test]
    fn interleaved_insert_delete() {
        let mut buf = make_with_text(4096, b"abcdef");

        // Delete "cd" (positions 2..4).
        assert!(delete_range(&mut buf, 2, 4));
        assert_eq!(extract_string(&buf), "abef");

        // Insert "XY" at position 2.
        assert!(insert_bytes(&mut buf, 2, b"XY"));
        assert_eq!(extract_string(&buf), "abXYef");

        // Delete first character.
        assert!(delete(&mut buf, 0));
        assert_eq!(extract_string(&buf), "bXYef");

        // Append "Z".
        let len = text_len(&buf);
        assert!(insert(&mut buf, len, b'Z'));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "bXYefZ");
    }

    #[test]
    fn build_string_char_by_char_at_front() {
        let mut buf = make_empty(4096);
        // Insert characters at position 0 each time, building "edcba".
        for &ch in b"abcde" {
            assert!(insert(&mut buf, 0, ch));
        }
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "edcba");
    }

    // -----------------------------------------------------------------------
    // 5. Iteration / content extraction
    // -----------------------------------------------------------------------

    #[test]
    fn byte_at_returns_none_past_end() {
        let buf = make_with_text(4096, b"Hi");
        assert_eq!(byte_at(&buf, 0), Some(b'H'));
        assert_eq!(byte_at(&buf, 1), Some(b'i'));
        assert_eq!(byte_at(&buf, 2), None);
        assert_eq!(byte_at(&buf, u32::MAX), None);
    }

    #[test]
    fn text_slice_partial_range() {
        let buf = make_with_text(4096, b"Hello, world!");
        let mut out = [0u8; 5];
        let n = text_slice(&buf, 7, 12, &mut out);
        assert_eq!(n, 5);
        assert_eq!(&out[..n], b"world");
    }

    #[test]
    fn text_slice_clamps_to_text_len() {
        let buf = make_with_text(4096, b"Hi");
        let mut out = [0u8; 64];
        // Request beyond text_len; should clamp.
        let n = text_slice(&buf, 0, 100, &mut out);
        assert_eq!(n, 2);
        assert_eq!(&out[..n], b"Hi");
    }

    #[test]
    fn text_slice_start_past_end_returns_zero() {
        let buf = make_with_text(4096, b"Hi");
        let mut out = [0u8; 64];
        assert_eq!(text_slice(&buf, 5, 10, &mut out), 0);
    }

    #[test]
    fn text_slice_limited_by_output_buffer() {
        let buf = make_with_text(4096, b"Hello, world!");
        let mut out = [0u8; 3];
        let n = text_slice(&buf, 0, 13, &mut out);
        assert_eq!(n, 3);
        assert_eq!(&out[..n], b"Hel");
    }

    #[test]
    fn styled_run_count_empty_table() {
        let buf = make_empty(1024);
        assert_eq!(styled_run_count(&buf), 0);
    }

    #[test]
    fn styled_run_count_single_piece() {
        let buf = make_with_text(4096, b"Hello");
        assert_eq!(styled_run_count(&buf), 1);
        let run = styled_run(&buf, 0).unwrap();
        assert_eq!(run.byte_offset, 0);
        assert_eq!(run.byte_len, 5);
        assert_eq!(run.style_id, 0);
    }

    #[test]
    fn styled_run_out_of_bounds_returns_none() {
        let buf = make_with_text(4096, b"Hello");
        assert!(styled_run(&buf, 1).is_none());
        assert!(styled_run(&buf, 100).is_none());
    }

    #[test]
    fn copy_run_text_extracts_correctly() {
        let buf = make_with_text(4096, b"Hello");
        let run = styled_run(&buf, 0).unwrap();
        let mut out = [0u8; 16];
        let n = copy_run_text(&buf, &run, &mut out);
        assert_eq!(&out[..n], b"Hello");
    }

    // -----------------------------------------------------------------------
    // 6. Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn validate_rejects_bad_magic() {
        let mut buf = make_empty(1024);
        // Corrupt the magic number.
        buf[0] = 0xFF;
        assert!(!validate(&buf));
    }

    #[test]
    fn validate_rejects_truncated_buffer() {
        assert!(!validate(&[0u8; 4]));
        assert!(!validate(&[]));
    }

    #[test]
    fn cursor_position_roundtrip() {
        let mut buf = make_empty(1024);
        set_cursor_pos(&mut buf, 42);
        assert_eq!(cursor_pos(&buf), 42);
    }

    #[test]
    fn selection_roundtrip() {
        let mut buf = make_empty(1024);
        set_selection(&mut buf, 10, 20);
        assert_eq!(selection(&buf), (10, 20));
    }

    #[test]
    fn operation_id_increments() {
        let mut buf = make_empty(1024);
        assert_eq!(header(&buf).operation_id, 0);
        assert_eq!(next_operation(&mut buf), 1);
        assert_eq!(next_operation(&mut buf), 2);
        assert_eq!(header(&buf).operation_id, 2);
    }

    #[test]
    fn large_insert_then_verify_every_byte() {
        let mut buf = make_empty(8192);
        let text: Vec<u8> = (0u8..=255).cycle().take(500).collect();
        assert!(insert_bytes(&mut buf, 0, &text));
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 500);
        for (i, &expected) in text.iter().enumerate() {
            assert_eq!(byte_at(&buf, i as u32), Some(expected), "mismatch at {}", i);
        }
    }

    #[test]
    fn delete_hole_punch_splits_piece() {
        let mut buf = make_with_text(4096, b"ABCDE");
        // Delete "CD" (positions 2..4) — punches a hole in the single piece.
        assert!(delete_range(&mut buf, 2, 4));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "ABE");
        // Should have split into 2 pieces.
        assert_eq!(header(&buf).piece_count, 2);
    }

    // -----------------------------------------------------------------------
    // 7. Style operations
    // -----------------------------------------------------------------------

    #[test]
    fn add_style_returns_sequential_indices() {
        let mut buf = make_empty(4096);
        let idx0 = add_style(&mut buf, &default_body_style());
        let idx1 = add_style(&mut buf, &bold_style());
        let idx2 = add_style(&mut buf, &italic_style());
        assert_eq!(idx0, Some(0));
        assert_eq!(idx1, Some(1));
        assert_eq!(idx2, Some(2));
        assert_eq!(style_count(&buf), 3);
    }

    #[test]
    fn style_retrieval_by_index() {
        let mut buf = make_empty(4096);
        let body = default_body_style();
        add_style(&mut buf, &body);
        let retrieved = style(&buf, 0).unwrap();
        assert_eq!(retrieved.role, body.role);
        assert_eq!(retrieved.weight, body.weight);
        assert_eq!(retrieved.font_size_pt, body.font_size_pt);
    }

    #[test]
    fn style_out_of_range_returns_none() {
        let buf = make_empty(4096);
        assert!(style(&buf, 0).is_none());
        assert!(style(&buf, 255).is_none());
    }

    #[test]
    fn find_style_by_role_finds_correct_style() {
        let mut buf = make_empty(4096);
        add_style(&mut buf, &default_body_style());
        add_style(&mut buf, &bold_style());
        add_style(&mut buf, &code_style());
        assert_eq!(find_style_by_role(&buf, ROLE_BODY), Some(0));
        assert_eq!(find_style_by_role(&buf, ROLE_STRONG), Some(1));
        assert_eq!(find_style_by_role(&buf, ROLE_CODE), Some(2));
        assert_eq!(find_style_by_role(&buf, ROLE_HEADING1), None);
    }

    #[test]
    fn add_default_styles_adds_seven() {
        let mut buf = make_empty(4096);
        assert!(add_default_styles(&mut buf));
        assert_eq!(style_count(&buf), 7);
    }

    #[test]
    fn apply_style_changes_run_style() {
        let mut buf = make_with_text(4096, b"Hello, world!");
        add_style(&mut buf, &bold_style());
        let bold_idx = style_count(&buf) - 1;

        // Apply bold to "world" (positions 7..12).
        apply_style(&mut buf, 7, 12, bold_idx);
        assert!(validate(&buf));

        // Text content should be unchanged.
        assert_eq!(extract_string(&buf), "Hello, world!");

        // Styles should differ across the range.
        assert_eq!(style_at(&buf, 0), Some(0)); // original style
        assert_eq!(style_at(&buf, 7), Some(bold_idx)); // bold
        assert_eq!(style_at(&buf, 11), Some(bold_idx)); // bold
        assert_eq!(style_at(&buf, 12), Some(0)); // original style
    }

    #[test]
    fn apply_style_empty_range_is_noop() {
        let mut buf = make_with_text(4096, b"Hello");
        let pc_before = header(&buf).piece_count;
        apply_style(&mut buf, 3, 3, 0);
        assert_eq!(header(&buf).piece_count, pc_before);
    }

    #[test]
    fn set_and_get_current_style() {
        let mut buf = make_empty(1024);
        assert_eq!(current_style(&buf), 0);
        set_current_style(&mut buf, 5);
        assert_eq!(current_style(&buf), 5);
    }

    // -----------------------------------------------------------------------
    // 8. Compaction
    // -----------------------------------------------------------------------

    #[test]
    fn compact_merges_adjacent_same_source_pieces() {
        let mut buf = make_with_text(4096, b"Hello");
        // Insert in the middle to create multiple pieces, then delete the insert
        // so we have two adjacent pieces from the same source.
        assert!(insert_bytes(&mut buf, 2, b"XX"));
        assert!(delete_range(&mut buf, 2, 4));
        // Now we have two original-buffer pieces (He + llo) that are contiguous.
        let pc_before = header(&buf).piece_count;
        assert!(pc_before >= 2, "expected >= 2 pieces, got {}", pc_before);

        let removed = compact(&mut buf);
        assert!(removed > 0);
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello");
    }

    #[test]
    fn compact_on_single_piece_is_noop() {
        let mut buf = make_with_text(4096, b"Hello");
        assert_eq!(compact(&mut buf), 0);
        assert_eq!(header(&buf).piece_count, 1);
    }

    #[test]
    fn compact_empty_table_is_noop() {
        let mut buf = make_empty(1024);
        assert_eq!(compact(&mut buf), 0);
    }

    // -----------------------------------------------------------------------
    // 9. Stress / combined operations
    // -----------------------------------------------------------------------

    #[test]
    fn repeated_insert_delete_cycles_stay_valid() {
        let mut buf = make_empty(8192);
        for round in 0..20u8 {
            let text = [b'A' + (round % 26)];
            assert!(insert_bytes(&mut buf, 0, &text));
        }
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 20);

        // Delete all characters one by one from the front.
        for _ in 0..20 {
            assert!(delete(&mut buf, 0));
        }
        assert!(validate(&buf));
        assert_eq!(text_len(&buf), 0);
    }

    #[test]
    fn insert_at_every_position_in_growing_string() {
        let mut buf = make_empty(8192);
        // Build "0123456789" by inserting each digit at its correct position.
        for i in 0..10u8 {
            assert!(insert(&mut buf, i as u32, b'0' + i));
        }
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "0123456789");
    }

    #[test]
    fn style_at_returns_none_on_empty() {
        let buf = make_empty(1024);
        assert_eq!(style_at(&buf, 0), None);
    }

    #[test]
    fn text_slice_across_multiple_pieces() {
        let mut buf = make_with_text(4096, b"AAABBB");
        // Insert "CCC" in the middle to create multiple pieces.
        assert!(insert_bytes(&mut buf, 3, b"CCC"));
        assert_eq!(extract_string(&buf), "AAACCCBBB");

        // Slice across piece boundaries.
        let mut out = [0u8; 5];
        let n = text_slice(&buf, 2, 7, &mut out);
        assert_eq!(n, 5);
        assert_eq!(&out[..n], b"ACCCB");
    }

    #[test]
    fn styled_runs_after_apply_style() {
        let mut buf = make_with_text(4096, b"AABBCC");
        add_style(&mut buf, &bold_style());

        // Apply bold to the middle "BB" (positions 2..4).
        apply_style(&mut buf, 2, 4, 1);
        assert!(validate(&buf));

        // Should have 3 styled runs: AA(style=0), BB(style=1), CC(style=0).
        assert_eq!(styled_run_count(&buf), 3);

        let r0 = styled_run(&buf, 0).unwrap();
        assert_eq!(r0.byte_offset, 0);
        assert_eq!(r0.byte_len, 2);
        assert_eq!(r0.style_id, 0);

        let r1 = styled_run(&buf, 1).unwrap();
        assert_eq!(r1.byte_offset, 2);
        assert_eq!(r1.byte_len, 2);
        assert_eq!(r1.style_id, 1);

        let r2 = styled_run(&buf, 2).unwrap();
        assert_eq!(r2.byte_offset, 4);
        assert_eq!(r2.byte_len, 2);
        assert_eq!(r2.style_id, 0);
    }

    #[test]
    fn validate_detects_corrupted_text_len() {
        let mut buf = make_with_text(4096, b"Hello");
        assert!(validate(&buf));
        // Corrupt text_len to mismatch piece sum.
        header_mut(&mut buf).text_len = 99;
        assert!(!validate(&buf));
    }

    #[test]
    fn insert_after_style_addition_preserves_data() {
        let mut buf = make_empty(4096);
        add_style(&mut buf, &default_body_style());
        add_style(&mut buf, &bold_style());
        // Insert text -- style addition shifted pieces/data, verify insert works.
        assert!(insert_bytes(&mut buf, 0, b"Hello"));
        assert!(validate(&buf));
        assert_eq!(extract_string(&buf), "Hello");
    }
}
