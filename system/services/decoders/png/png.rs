//! Full PNG decoder — no external dependencies, no_std, no alloc.
//!
//! Supports all PNG color types (0, 2, 3, 4, 6) at all valid bit depths
//! (1, 2, 4, 8, 16), including Adam7 interlacing, PLTE palettes, and
//! tRNS transparency. Decodes into a caller-provided BGRA8888 output buffer.
//!
//! Buffer requirement: caller must provide at least `png_decode_buf_size(data)`
//! bytes. For non-interlaced images this is approximately `w*h*4 + raw_size`.
//! For interlaced images the raw_size includes all 7 Adam7 passes.

/// PNG decoding errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngError {
    /// Not a valid PNG file (bad magic bytes).
    InvalidSignature,
    /// Unexpected end of data.
    Truncated,
    /// Width or height is zero.
    ZeroDimensions,
    /// Bit depth or color type combination not valid per PNG spec.
    UnsupportedFormat,
    /// Missing required IHDR chunk.
    MissingIhdr,
    /// Corrupt or invalid compressed data.
    InvalidData,
    /// Decompressed data doesn't match expected scanline size.
    DataSizeMismatch,
    /// Output buffer too small for decoded image.
    BufferTooSmall,
    /// Image dimensions would overflow.
    DimensionOverflow,
    /// Chunk CRC32 does not match computed value.
    CrcMismatch,
}

/// Decoded PNG image header.
#[derive(Debug, Clone, Copy)]
pub struct PngHeader {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: u8,
}

/// PNG magic signature (8 bytes).
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Adam7 interlace pass parameters: (x_start, y_start, x_step, y_step).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_be_u32(data: &[u8]) -> u32 {
    ((data[0] as u32) << 24) | ((data[1] as u32) << 16) | ((data[2] as u32) << 8) | (data[3] as u32)
}

fn read_be_u16(data: &[u8]) -> u16 {
    ((data[0] as u16) << 8) | (data[1] as u16)
}

/// Bits per raw pixel for a given color type and bit depth.
pub fn bits_per_pixel(color_type: u8, bit_depth: u8) -> usize {
    let channels: usize = match color_type {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => 1,
    };
    channels * bit_depth as usize
}

/// Raw scanline bytes (excluding filter byte) for a given width and format.
fn raw_row_bytes(width: usize, color_type: u8, bit_depth: u8) -> usize {
    (width * bits_per_pixel(color_type, bit_depth) + 7) / 8
}

/// Bytes per complete pixel for filtering (minimum 1, per PNG spec).
fn filter_bpp(color_type: u8, bit_depth: u8) -> usize {
    let bpp = bits_per_pixel(color_type, bit_depth) / 8;
    if bpp == 0 {
        1
    } else {
        bpp
    }
}

/// Validate color_type / bit_depth combination per PNG spec Table 11.1.
fn validate_format(color_type: u8, bit_depth: u8) -> Result<(), PngError> {
    let valid = match color_type {
        0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
        2 => matches!(bit_depth, 8 | 16),
        3 => matches!(bit_depth, 1 | 2 | 4 | 8),
        4 => matches!(bit_depth, 8 | 16),
        6 => matches!(bit_depth, 8 | 16),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(PngError::UnsupportedFormat)
    }
}

// ---------------------------------------------------------------------------
// CRC32 (IEEE 802.3, polynomial 0xEDB88320 reflected)
// ---------------------------------------------------------------------------

/// Precomputed CRC32 lookup table (256 entries, IEEE polynomial).
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

/// Compute CRC32 over a byte slice.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i < data.len() {
        let idx = ((crc ^ data[i] as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[idx];
        i += 1;
    }
    crc ^ 0xFFFF_FFFF
}

/// Compute CRC32 over chunk_type (4 bytes) + chunk_data.
/// This is the PNG-specified CRC scope: type field + data field, not length.
pub fn chunk_crc(chunk_type: &[u8], chunk_data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i < chunk_type.len() {
        let idx = ((crc ^ chunk_type[i] as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[idx];
        i += 1;
    }
    i = 0;
    while i < chunk_data.len() {
        let idx = ((crc ^ chunk_data[i] as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[idx];
        i += 1;
    }
    crc ^ 0xFFFF_FFFF
}

// ---------------------------------------------------------------------------
// Header parsing
// ---------------------------------------------------------------------------

/// Parse PNG header only (for querying dimensions without full decode).
pub fn png_header(data: &[u8]) -> Result<PngHeader, PngError> {
    if data.len() < 8 {
        return Err(PngError::Truncated);
    }
    if data[..8] != PNG_SIGNATURE {
        return Err(PngError::InvalidSignature);
    }
    if data.len() < 8 + 4 + 4 + 13 + 4 {
        return Err(PngError::Truncated);
    }
    let chunk_len = read_be_u32(&data[8..12]) as usize;
    let chunk_type = &data[12..16];
    if chunk_type != b"IHDR" || chunk_len != 13 {
        return Err(PngError::MissingIhdr);
    }
    // Validate IHDR CRC: covers chunk_type (4 bytes) + chunk_data (13 bytes).
    let ihdr_data = &data[16..16 + chunk_len];
    let stored_crc = read_be_u32(&data[16 + chunk_len..16 + chunk_len + 4]);
    let computed_crc = chunk_crc(chunk_type, ihdr_data);
    if stored_crc != computed_crc {
        return Err(PngError::CrcMismatch);
    }
    let width = read_be_u32(&ihdr_data[0..4]);
    let height = read_be_u32(&ihdr_data[4..8]);
    let bit_depth = ihdr_data[8];
    let color_type = ihdr_data[9];
    if width == 0 || height == 0 {
        return Err(PngError::ZeroDimensions);
    }
    Ok(PngHeader {
        width,
        height,
        bit_depth,
        color_type,
    })
}

// ---------------------------------------------------------------------------
// Chunk parsing
// ---------------------------------------------------------------------------

/// Parsed ancillary chunk data needed for decoding.
struct PngChunks {
    /// File offset of the first IDAT chunk's length field.
    /// The BitReader walks consecutive IDAT chunks from here.
    first_idat_pos: usize,
    /// PLTE palette entries (RGB, up to 256).
    palette: [[u8; 3]; 256],
    palette_count: usize,
    /// tRNS alpha values for indexed color (up to 256). Default 255 (opaque).
    trns_alpha: [u8; 256],
    trns_count: usize,
    /// tRNS key color for grayscale.
    trns_gray: u16,
    trns_gray_set: bool,
    /// tRNS key color for RGB.
    trns_rgb: [u16; 3],
    trns_rgb_set: bool,
    /// Interlace method from IHDR (0 = none, 1 = Adam7).
    interlace: u8,
}

impl PngChunks {
    fn new() -> Self {
        Self {
            first_idat_pos: 0,
            palette: [[0; 3]; 256],
            palette_count: 0,
            trns_alpha: [255; 256],
            trns_count: 0,
            trns_gray: 0,
            trns_gray_set: false,
            trns_rgb: [0; 3],
            trns_rgb_set: false,
            interlace: 0,
        }
    }
}

/// Walk all chunks and extract IDAT offsets, PLTE, tRNS, and interlace method.
fn parse_chunks(data: &[u8], color_type: u8) -> Result<PngChunks, PngError> {
    let mut chunks = PngChunks::new();

    // Interlace method is IHDR byte 12 (file offset 28).
    if data.len() < 29 {
        return Err(PngError::Truncated);
    }
    chunks.interlace = data[28];
    if chunks.interlace > 1 {
        return Err(PngError::InvalidData);
    }

    let mut pos = 33; // skip signature (8) + IHDR chunk (4 len + 4 type + 13 data + 4 CRC)
    while pos + 8 <= data.len() {
        let chunk_len = read_be_u32(&data[pos..pos + 4]) as usize;
        let chunk_type = &data[pos + 4..pos + 8];

        if pos + 8 + chunk_len + 4 > data.len() {
            if chunk_type == b"IDAT" {
                return Err(PngError::Truncated);
            }
            break;
        }

        let chunk_data = &data[pos + 8..pos + 8 + chunk_len];

        // Validate CRC32 for every chunk (PNG spec §5.4).
        let stored_crc = read_be_u32(&data[pos + 8 + chunk_len..pos + 8 + chunk_len + 4]);
        let computed_crc = chunk_crc(chunk_type, chunk_data);
        if stored_crc != computed_crc {
            return Err(PngError::CrcMismatch);
        }

        if chunk_type == b"IDAT" {
            if chunks.first_idat_pos == 0 {
                chunks.first_idat_pos = pos;
            }
        } else if chunk_type == b"PLTE" {
            if chunk_len % 3 != 0 || chunk_len > 768 {
                return Err(PngError::InvalidData);
            }
            chunks.palette_count = chunk_len / 3;
            let mut i = 0;
            while i < chunks.palette_count {
                chunks.palette[i] = [
                    chunk_data[i * 3],
                    chunk_data[i * 3 + 1],
                    chunk_data[i * 3 + 2],
                ];
                i += 1;
            }
        } else if chunk_type == b"tRNS" {
            match color_type {
                0 => {
                    if chunk_len >= 2 {
                        chunks.trns_gray = read_be_u16(chunk_data);
                        chunks.trns_gray_set = true;
                    }
                }
                2 => {
                    if chunk_len >= 6 {
                        chunks.trns_rgb = [
                            read_be_u16(&chunk_data[0..2]),
                            read_be_u16(&chunk_data[2..4]),
                            read_be_u16(&chunk_data[4..6]),
                        ];
                        chunks.trns_rgb_set = true;
                    }
                }
                3 => {
                    let count = chunk_len.min(256);
                    let mut i = 0;
                    while i < count {
                        chunks.trns_alpha[i] = chunk_data[i];
                        i += 1;
                    }
                    chunks.trns_count = count;
                }
                _ => {} // tRNS not applicable for types 4 and 6
            }
        } else if chunk_type == b"IEND" {
            break;
        }

        pos += 8 + chunk_len + 4;
    }

    if chunks.first_idat_pos == 0 {
        return Err(PngError::Truncated);
    }

    // Indexed images require a palette.
    if color_type == 3 && chunks.palette_count == 0 {
        return Err(PngError::InvalidData);
    }

    Ok(chunks)
}

// ---------------------------------------------------------------------------
// Full decode
// ---------------------------------------------------------------------------

/// Compute the minimum output buffer size for `png_decode`.
///
/// Returns the number of bytes the output buffer must have.
pub fn png_decode_buf_size(data: &[u8]) -> Result<usize, PngError> {
    let header = png_header(data)?;
    validate_format(header.color_type, header.bit_depth)?;

    let w = header.width as usize;
    let h = header.height as usize;
    let bgra_size = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(4))
        .ok_or(PngError::DimensionOverflow)?;

    let interlace = data.get(28).copied().unwrap_or(0);
    let total_raw = if interlace == 0 {
        let rrb = raw_row_bytes(w, header.color_type, header.bit_depth);
        (rrb + 1) * h
    } else {
        adam7_total_raw(w, h, header.color_type, header.bit_depth)
    };

    Ok(bgra_size + total_raw)
}

/// Total decompressed bytes for all Adam7 passes.
fn adam7_total_raw(w: usize, h: usize, color_type: u8, bit_depth: u8) -> usize {
    let mut total = 0;
    for &(xs, ys, xstep, ystep) in &ADAM7 {
        let pw = if xs < w {
            (w - xs + xstep - 1) / xstep
        } else {
            0
        };
        let ph = if ys < h {
            (h - ys + ystep - 1) / ystep
        } else {
            0
        };
        if pw > 0 && ph > 0 {
            let rrb = raw_row_bytes(pw, color_type, bit_depth);
            total += (rrb + 1) * ph;
        }
    }
    total
}

/// Decode a PNG image to BGRA8888 pixel data.
///
/// `data` is the complete PNG file. `output` must be at least
/// `png_decode_buf_size(data)` bytes. Returns the header on success.
/// The first `width * height * 4` bytes of `output` contain the BGRA pixels.
pub fn png_decode(data: &[u8], output: &mut [u8]) -> Result<PngHeader, PngError> {
    let header = png_header(data)?;
    validate_format(header.color_type, header.bit_depth)?;

    let w = header.width as usize;
    let h = header.height as usize;
    let bgra_size = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(4))
        .ok_or(PngError::DimensionOverflow)?;

    let chunks = parse_chunks(data, header.color_type)?;

    if chunks.interlace == 0 {
        decode_non_interlaced(
            output,
            w,
            h,
            header.color_type,
            header.bit_depth,
            bgra_size,
            data,
            &chunks,
        )?;
    } else {
        decode_interlaced(
            output,
            w,
            h,
            header.color_type,
            header.bit_depth,
            bgra_size,
            data,
            &chunks,
        )?;
    }

    Ok(header)
}

/// Decode a non-interlaced image.
///
/// Buffer layout: `[BGRA output: bgra_size] [raw decompressed: total_raw]`
/// We decompress into the raw area, unfilter in-place, then convert row by row
/// from the raw area into the BGRA area. No overlap.
fn decode_non_interlaced(
    output: &mut [u8],
    w: usize,
    h: usize,
    color_type: u8,
    bit_depth: u8,
    bgra_size: usize,
    data: &[u8],
    chunks: &PngChunks,
) -> Result<(), PngError> {
    let rrb = raw_row_bytes(w, color_type, bit_depth);
    let scanline = rrb + 1; // filter byte + raw row
    let total_raw = scanline * h;
    let min_buf = bgra_size + total_raw;

    if output.len() < min_buf {
        return Err(PngError::BufferTooSmall);
    }

    // Decompress into the area after BGRA output.
    let raw_start = bgra_size;
    let decompressed = inflate_idat(
        data,
        chunks.first_idat_pos,
        &mut output[raw_start..raw_start + total_raw],
    )?;
    if decompressed != total_raw {
        return Err(PngError::DataSizeMismatch);
    }

    // Unfilter scanlines in the raw area.
    let bpp = filter_bpp(color_type, bit_depth);
    unfilter_scanlines(&mut output[raw_start..], rrb, h, bpp)?;

    // Convert each row from raw → BGRA. No overlap (different regions).
    for y in 0..h {
        let raw_row = raw_start + y * scanline + 1; // +1 to skip filter byte
        let bgra_row = y * w * 4;
        // We need separate borrows: read from raw area, write to bgra area.
        // Since raw_start >= bgra_size and bgra_row < bgra_size, they don't overlap.
        // Use split_at_mut to satisfy the borrow checker.
        let (bgra_part, raw_part) = output.split_at_mut(raw_start);
        let raw_slice = &raw_part[y * scanline + 1..y * scanline + 1 + rrb];
        let bgra_slice = &mut bgra_part[bgra_row..bgra_row + w * 4];
        row_to_bgra(raw_slice, bgra_slice, w, color_type, bit_depth, chunks);
    }

    Ok(())
}

/// Decode an Adam7 interlaced image.
///
/// Same buffer layout as non-interlaced. All 7 passes are decompressed into
/// the scratch area, then each pass is unfiltered and scattered into the BGRA output.
fn decode_interlaced(
    output: &mut [u8],
    w: usize,
    h: usize,
    color_type: u8,
    bit_depth: u8,
    bgra_size: usize,
    data: &[u8],
    chunks: &PngChunks,
) -> Result<(), PngError> {
    let total_raw = adam7_total_raw(w, h, color_type, bit_depth);
    let min_buf = bgra_size + total_raw;

    if output.len() < min_buf {
        return Err(PngError::BufferTooSmall);
    }

    // Zero the BGRA area (pixels not covered by any pass stay transparent black).
    let mut i = 0;
    while i < bgra_size {
        output[i] = 0;
        i += 1;
    }

    // Decompress all passes into scratch area.
    let raw_start = bgra_size;
    let decompressed = inflate_idat(
        data,
        chunks.first_idat_pos,
        &mut output[raw_start..raw_start + total_raw],
    )?;
    if decompressed != total_raw {
        return Err(PngError::DataSizeMismatch);
    }

    // Process each pass: unfilter, then scatter pixels.
    let mut pass_offset = 0usize;
    for &(xs, ys, xstep, ystep) in &ADAM7 {
        let pw = if xs < w {
            (w - xs + xstep - 1) / xstep
        } else {
            0
        };
        let ph = if ys < h {
            (h - ys + ystep - 1) / ystep
        } else {
            0
        };
        if pw == 0 || ph == 0 {
            continue;
        }

        let rrb = raw_row_bytes(pw, color_type, bit_depth);
        let scanline = rrb + 1;
        let pass_raw = scanline * ph;
        let bpp = filter_bpp(color_type, bit_depth);

        // Unfilter this pass's scanlines in the raw area.
        let abs_start = raw_start + pass_offset;
        unfilter_scanlines(&mut output[abs_start..abs_start + pass_raw], rrb, ph, bpp)?;

        // Convert each pass row and scatter pixels to their final positions.
        // Use a stack buffer for one row of BGRA to avoid overlap concerns.
        // Max pass width for 32×32 is 32; for any image max is w.
        // pw * 4 bytes per row. For large images this could be big, but
        // we stack-allocate via a loop with per-pixel scatter instead.
        for py in 0..ph {
            let raw_row_start = abs_start + py * scanline + 1;
            let dest_y = ys + py * ystep;

            // For each pixel in the pass row, convert and write directly to output.
            scatter_row(
                output,
                raw_start,
                raw_row_start,
                rrb,
                pw,
                dest_y,
                xs,
                xstep,
                w,
                color_type,
                bit_depth,
                chunks,
            );
        }

        pass_offset += pass_raw;
    }

    Ok(())
}

/// Convert one pass row and scatter pixels directly into the BGRA output.
///
/// Reads raw pixel data from `output[raw_row_start..raw_row_start+rrb]` and
/// writes BGRA pixels to their interlaced positions in `output[0..bgra_area]`.
/// `raw_start` is the boundary between BGRA and raw areas.
fn scatter_row(
    output: &mut [u8],
    raw_start: usize,
    raw_row_start: usize,
    rrb: usize,
    pass_width: usize,
    dest_y: usize,
    x_start: usize,
    x_step: usize,
    image_width: usize,
    color_type: u8,
    bit_depth: u8,
    chunks: &PngChunks,
) {
    // Extract raw row bytes into a stack copy to avoid aliasing.
    // Max raw row for a 32-pixel-wide pass at 16-bit RGBA: 32*8 = 256 bytes.
    // For larger images this could be bigger, but we cap at a reasonable size.
    // If too large, we'd need a different approach, but for PNG images up to
    // ~8K pixels wide at 64bpp, 64KB on stack is fine.
    let mut raw_buf = [0u8; 8192];
    let copy_len = rrb.min(raw_buf.len());
    raw_buf[..copy_len].copy_from_slice(&output[raw_row_start..raw_row_start + copy_len]);

    // Convert and scatter each pixel.
    for px in 0..pass_width {
        let dest_x = x_start + px * x_step;
        let out_idx = (dest_y * image_width + dest_x) * 4;
        let mut bgra = [0u8; 4];
        pixel_to_bgra(
            &raw_buf[..rrb],
            px,
            color_type,
            bit_depth,
            chunks,
            &mut bgra,
        );
        output[out_idx] = bgra[0];
        output[out_idx + 1] = bgra[1];
        output[out_idx + 2] = bgra[2];
        output[out_idx + 3] = bgra[3];
    }
}

// ---------------------------------------------------------------------------
// Pixel conversion
// ---------------------------------------------------------------------------

/// Convert one raw pixel at position `x` in the unfiltered row to BGRA.
fn pixel_to_bgra(
    raw: &[u8],
    x: usize,
    color_type: u8,
    bit_depth: u8,
    chunks: &PngChunks,
    out: &mut [u8; 4],
) {
    match (color_type, bit_depth) {
        // Grayscale 8-bit
        (0, 8) => {
            let g = raw[x];
            let a = if chunks.trns_gray_set && g as u16 == chunks.trns_gray {
                0
            } else {
                255
            };
            *out = [g, g, g, a];
        }
        // Grayscale 16-bit
        (0, 16) => {
            let sample = read_be_u16(&raw[x * 2..]);
            let g = (sample >> 8) as u8;
            let a = if chunks.trns_gray_set && sample == chunks.trns_gray {
                0
            } else {
                255
            };
            *out = [g, g, g, a];
        }
        // Grayscale sub-byte (1, 2, 4-bit)
        (0, bd) => {
            let val = unpack_sub_byte(raw, x, bd);
            let g = scale_to_8bit(val, bd);
            let a = if chunks.trns_gray_set && val as u16 == chunks.trns_gray {
                0
            } else {
                255
            };
            *out = [g, g, g, a];
        }
        // RGB 8-bit
        (2, 8) => {
            let i = x * 3;
            let (r, g, b) = (raw[i], raw[i + 1], raw[i + 2]);
            let a = if chunks.trns_rgb_set
                && r as u16 == chunks.trns_rgb[0]
                && g as u16 == chunks.trns_rgb[1]
                && b as u16 == chunks.trns_rgb[2]
            {
                0
            } else {
                255
            };
            *out = [b, g, r, a];
        }
        // RGB 16-bit
        (2, 16) => {
            let i = x * 6;
            let r16 = read_be_u16(&raw[i..]);
            let g16 = read_be_u16(&raw[i + 2..]);
            let b16 = read_be_u16(&raw[i + 4..]);
            let a = if chunks.trns_rgb_set
                && r16 == chunks.trns_rgb[0]
                && g16 == chunks.trns_rgb[1]
                && b16 == chunks.trns_rgb[2]
            {
                0
            } else {
                255
            };
            *out = [(b16 >> 8) as u8, (g16 >> 8) as u8, (r16 >> 8) as u8, a];
        }
        // Indexed 8-bit
        (3, 8) => {
            let idx = raw[x] as usize;
            let [r, g, b] = if idx < chunks.palette_count {
                chunks.palette[idx]
            } else {
                [0, 0, 0]
            };
            let a = if idx < chunks.trns_count {
                chunks.trns_alpha[idx]
            } else {
                255
            };
            *out = [b, g, r, a];
        }
        // Indexed sub-byte (1, 2, 4-bit)
        (3, bd) => {
            let idx = unpack_sub_byte(raw, x, bd) as usize;
            let [r, g, b] = if idx < chunks.palette_count {
                chunks.palette[idx]
            } else {
                [0, 0, 0]
            };
            let a = if idx < chunks.trns_count {
                chunks.trns_alpha[idx]
            } else {
                255
            };
            *out = [b, g, r, a];
        }
        // Grayscale + Alpha 8-bit
        (4, 8) => {
            let g = raw[x * 2];
            let a = raw[x * 2 + 1];
            *out = [g, g, g, a];
        }
        // Grayscale + Alpha 16-bit
        (4, 16) => {
            let g = raw[x * 4]; // high byte of gray
            let a = raw[x * 4 + 2]; // high byte of alpha
            *out = [g, g, g, a];
        }
        // RGBA 8-bit
        (6, 8) => {
            let i = x * 4;
            *out = [raw[i + 2], raw[i + 1], raw[i], raw[i + 3]];
        }
        // RGBA 16-bit
        (6, 16) => {
            let i = x * 8;
            *out = [raw[i + 4], raw[i + 2], raw[i], raw[i + 6]];
        }
        _ => {
            *out = [0, 0, 0, 255];
        }
    }
}

/// Convert one unfiltered raw row to BGRA8888 pixels.
///
/// `raw` is the unfiltered row data (excluding the filter byte).
/// `bgra` is the output for this row (exactly `width * 4` bytes).
fn row_to_bgra(
    raw: &[u8],
    bgra: &mut [u8],
    width: usize,
    color_type: u8,
    bit_depth: u8,
    chunks: &PngChunks,
) {
    for x in 0..width {
        let mut px = [0u8; 4];
        pixel_to_bgra(raw, x, color_type, bit_depth, chunks, &mut px);
        let o = x * 4;
        bgra[o] = px[0];
        bgra[o + 1] = px[1];
        bgra[o + 2] = px[2];
        bgra[o + 3] = px[3];
    }
}

/// Unpack a sub-byte pixel value (1, 2, or 4-bit) at position `x` in a packed row.
/// Bits are packed MSB-first within each byte (PNG spec §7.2).
fn unpack_sub_byte(raw: &[u8], x: usize, bit_depth: u8) -> u8 {
    let bd = bit_depth as usize;
    let pixels_per_byte = 8 / bd;
    let byte_idx = x / pixels_per_byte;
    let bit_offset = (pixels_per_byte - 1 - x % pixels_per_byte) * bd;
    let mask = (1u8 << bd) - 1;
    (raw[byte_idx] >> bit_offset) & mask
}

/// Scale a sub-byte sample value to 0-255.
/// 1-bit: 0→0, 1→255. 2-bit: 0→0, 1→85, 2→170, 3→255. 4-bit: 0→0, ..., 15→255.
fn scale_to_8bit(val: u8, bit_depth: u8) -> u8 {
    match bit_depth {
        1 => val * 255,
        2 => val * 85,
        4 => val * 17,
        8 => val,
        _ => val,
    }
}

// ---------------------------------------------------------------------------
// Scanline unfiltering
// ---------------------------------------------------------------------------

/// Unfilter PNG scanlines in-place.
///
/// `row_len` is the number of raw bytes per row (excluding the filter byte).
/// `bpp` is bytes per complete pixel (minimum 1) used by Sub/Average/Paeth filters.
fn unfilter_scanlines(
    data: &mut [u8],
    row_len: usize,
    height: usize,
    bpp: usize,
) -> Result<(), PngError> {
    let scanline = 1 + row_len;

    for y in 0..height {
        let row_start = y * scanline;
        let filter_type = data[row_start];
        let ps = row_start + 1; // pixel data start

        if ps + row_len > data.len() {
            return Err(PngError::Truncated);
        }

        match filter_type {
            0 => {} // None
            1 => {
                // Sub: Raw(x) += Raw(x - bpp)
                let mut i = bpp;
                while i < row_len {
                    data[ps + i] = data[ps + i].wrapping_add(data[ps + i - bpp]);
                    i += 1;
                }
            }
            2 => {
                // Up: Raw(x) += Prior(x)
                if y > 0 {
                    let prev_ps = (y - 1) * scanline + 1;
                    let mut i = 0;
                    while i < row_len {
                        data[ps + i] = data[ps + i].wrapping_add(data[prev_ps + i]);
                        i += 1;
                    }
                }
            }
            3 => {
                // Average: Raw(x) += floor((Raw(x-bpp) + Prior(x)) / 2)
                let prev_ps = if y > 0 { (y - 1) * scanline + 1 } else { 0 };
                let mut i = 0;
                while i < row_len {
                    let a = if i >= bpp {
                        data[ps + i - bpp] as u16
                    } else {
                        0
                    };
                    let b = if y > 0 { data[prev_ps + i] as u16 } else { 0 };
                    data[ps + i] = data[ps + i].wrapping_add(((a + b) / 2) as u8);
                    i += 1;
                }
            }
            4 => {
                // Paeth
                let prev_ps = if y > 0 { (y - 1) * scanline + 1 } else { 0 };
                let mut i = 0;
                while i < row_len {
                    let a = if i >= bpp {
                        data[ps + i - bpp] as i32
                    } else {
                        0
                    };
                    let b = if y > 0 { data[prev_ps + i] as i32 } else { 0 };
                    let c = if y > 0 && i >= bpp {
                        data[prev_ps + i - bpp] as i32
                    } else {
                        0
                    };
                    data[ps + i] = data[ps + i].wrapping_add(paeth_pred(a, b, c) as u8);
                    i += 1;
                }
            }
            _ => return Err(PngError::InvalidData),
        }
    }

    Ok(())
}

/// Paeth predictor function (PNG spec).
fn paeth_pred(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ---------------------------------------------------------------------------
// zlib / DEFLATE decompression — compact, stack-friendly implementation
// ---------------------------------------------------------------------------

/// Inflate zlib-compressed data from concatenated IDAT chunks.
///
/// `first_idat_pos` is the file offset of the first IDAT chunk's length field.
/// The BitReader walks consecutive IDAT chunks from there — no fixed limit on
/// the number of chunks.
fn inflate_idat(
    file_data: &[u8],
    first_idat_pos: usize,
    output: &mut [u8],
) -> Result<usize, PngError> {
    let mut reader = BitReader::new(file_data, first_idat_pos)?;

    // Parse zlib header (2 bytes)
    let cmf = reader.next_byte().ok_or(PngError::Truncated)?;
    let flg = reader.next_byte().ok_or(PngError::Truncated)?;

    if cmf & 0x0F != 8 {
        return Err(PngError::InvalidData);
    }
    if (flg >> 5) & 1 != 0 {
        return Err(PngError::InvalidData);
    }
    if ((cmf as u16) * 256 + flg as u16) % 31 != 0 {
        return Err(PngError::InvalidData);
    }

    inflate_stream(&mut reader, output)
}

/// Bit reader that walks consecutive IDAT chunks on-the-fly.
///
/// Instead of pre-collecting chunk offsets into a fixed-size array, this
/// reader holds a position into the file data and advances to the next
/// IDAT chunk when the current one is exhausted. Handles any number of
/// IDAT chunks with O(1) memory.
struct BitReader<'a> {
    data: &'a [u8],
    /// File offset of the current chunk's data start (chunk_pos + 8).
    chunk_data_start: usize,
    /// Length of the current IDAT chunk's data.
    chunk_data_len: usize,
    /// File offset of the current chunk's length field.
    chunk_pos: usize,
    /// Byte index within the current chunk's data.
    byte_idx: usize,
    /// True when no more IDAT chunks remain.
    exhausted: bool,
    bits: u32,
    nbits: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], first_idat_pos: usize) -> Result<Self, PngError> {
        if first_idat_pos + 8 > data.len() {
            return Err(PngError::Truncated);
        }
        let chunk_len = read_be_u32(&data[first_idat_pos..first_idat_pos + 4]) as usize;
        Ok(Self {
            data,
            chunk_pos: first_idat_pos,
            chunk_data_start: first_idat_pos + 8,
            chunk_data_len: chunk_len,
            byte_idx: 0,
            exhausted: false,
            bits: 0,
            nbits: 0,
        })
    }

    /// Advance to the next chunk in the file. If it's IDAT, start reading
    /// from it. Otherwise, mark the reader as exhausted.
    fn advance_chunk(&mut self) {
        // Current chunk ends at: chunk_data_start + chunk_data_len + 4 (CRC).
        let next_pos = self.chunk_data_start + self.chunk_data_len + 4;
        if next_pos + 8 > self.data.len() {
            self.exhausted = true;
            return;
        }
        let chunk_type = &self.data[next_pos + 4..next_pos + 8];
        if chunk_type != b"IDAT" {
            self.exhausted = true;
            return;
        }
        let chunk_len = read_be_u32(&self.data[next_pos..next_pos + 4]) as usize;
        if next_pos + 8 + chunk_len + 4 > self.data.len() {
            self.exhausted = true;
            return;
        }
        self.chunk_pos = next_pos;
        self.chunk_data_start = next_pos + 8;
        self.chunk_data_len = chunk_len;
        self.byte_idx = 0;
    }

    fn next_byte(&mut self) -> Option<u8> {
        loop {
            if self.exhausted {
                return None;
            }
            if self.byte_idx < self.chunk_data_len {
                let b = self.data[self.chunk_data_start + self.byte_idx];
                self.byte_idx += 1;
                return Some(b);
            }
            self.advance_chunk();
        }
    }

    fn read_bits(&mut self, n: u8) -> Result<u32, PngError> {
        while self.nbits < n {
            let b = self.next_byte().ok_or(PngError::Truncated)?;
            self.bits |= (b as u32) << self.nbits;
            self.nbits += 8;
        }
        let val = self.bits & ((1u32 << n) - 1);
        self.bits >>= n;
        self.nbits -= n;
        Ok(val)
    }

    fn align(&mut self) {
        self.bits = 0;
        self.nbits = 0;
    }

    fn read_u16_le(&mut self) -> Result<u16, PngError> {
        self.align();
        let lo = self.next_byte().ok_or(PngError::Truncated)?;
        let hi = self.next_byte().ok_or(PngError::Truncated)?;
        Ok((hi as u16) << 8 | lo as u16)
    }

    /// Decode one Huffman symbol using code lengths.
    /// Simple bit-by-bit decode — slow but uses zero extra memory.
    fn decode_huffman(&mut self, table: &HuffTable) -> Result<u16, PngError> {
        let mut code: u32 = 0;
        for len in 1..=15u8 {
            code = (code << 1) | self.read_bits(1)?;
            let start = table.offsets[len as usize] as usize;
            let count = table.counts[len as usize] as usize;
            let first_code = table.first_code[len as usize];
            if code >= first_code && (code - first_code) < count as u32 {
                let idx = start + (code - first_code) as usize;
                return Ok(table.symbols[idx]);
            }
        }
        Err(PngError::InvalidData)
    }
}

/// Compact Huffman table using canonical codes.
/// Total size: ~660 bytes for max 320 symbols (well within stack budget).
struct HuffTable {
    symbols: [u16; 320],
    counts: [u16; 16],
    offsets: [u16; 16],
    first_code: [u32; 16],
}

impl HuffTable {
    fn build(code_lengths: &[u8], num_symbols: usize) -> Result<Self, PngError> {
        let mut table = HuffTable {
            symbols: [0u16; 320],
            counts: [0u16; 16],
            offsets: [0u16; 16],
            first_code: [0u32; 16],
        };

        for i in 0..num_symbols {
            let len = code_lengths[i] as usize;
            if len > 15 {
                return Err(PngError::InvalidData);
            }
            if len > 0 {
                table.counts[len] += 1;
            }
        }

        let mut offset = 0u16;
        for i in 1..=15 {
            table.offsets[i] = offset;
            offset += table.counts[i];
        }

        let mut code = 0u32;
        for i in 1..=15 {
            code = (code + table.counts[i - 1] as u32) << 1;
            table.first_code[i] = code;
        }

        let mut pos = [0u16; 16];
        let mut i = 0;
        while i < 16 {
            pos[i] = table.offsets[i];
            i += 1;
        }
        for sym in 0..num_symbols {
            let len = code_lengths[sym] as usize;
            if len > 0 {
                let idx = pos[len] as usize;
                if idx < 320 {
                    table.symbols[idx] = sym as u16;
                }
                pos[len] += 1;
            }
        }

        Ok(table)
    }
}

/// Length base values for codes 257-285.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits for length codes 257-285.
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Distance base values for codes 0-29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits for distance codes 0-29.
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Code length alphabet order for dynamic Huffman.
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Inflate a DEFLATE stream.
fn inflate_stream(reader: &mut BitReader, output: &mut [u8]) -> Result<usize, PngError> {
    let mut out_pos = 0;

    loop {
        let bfinal = reader.read_bits(1)?;
        let btype = reader.read_bits(2)?;

        match btype {
            0 => {
                // Uncompressed block
                let len = reader.read_u16_le()?;
                let nlen = reader.read_u16_le()?;
                if len != !nlen {
                    return Err(PngError::InvalidData);
                }
                for _ in 0..len {
                    if out_pos >= output.len() {
                        return Err(PngError::DataSizeMismatch);
                    }
                    output[out_pos] = reader.next_byte().ok_or(PngError::Truncated)?;
                    out_pos += 1;
                }
            }
            1 => {
                // Fixed Huffman codes
                let mut lit_lens = [0u8; 288];
                let mut i = 0;
                while i <= 143 {
                    lit_lens[i] = 8;
                    i += 1;
                }
                while i <= 255 {
                    lit_lens[i] = 9;
                    i += 1;
                }
                while i <= 279 {
                    lit_lens[i] = 7;
                    i += 1;
                }
                while i <= 287 {
                    lit_lens[i] = 8;
                    i += 1;
                }

                let dist_lens = [5u8; 32];

                let lit_table = HuffTable::build(&lit_lens, 288)?;
                let dist_table = HuffTable::build(&dist_lens, 32)?;
                out_pos = decode_codes(reader, output, out_pos, &lit_table, &dist_table)?;
            }
            2 => {
                // Dynamic Huffman codes
                let hlit = reader.read_bits(5)? as usize + 257;
                let hdist = reader.read_bits(5)? as usize + 1;
                let hclen = reader.read_bits(4)? as usize + 4;

                let mut cl_lens = [0u8; 19];
                for i in 0..hclen {
                    cl_lens[CL_ORDER[i]] = reader.read_bits(3)? as u8;
                }
                let cl_table = HuffTable::build(&cl_lens, 19)?;

                let total = hlit + hdist;
                let mut combined = [0u8; 320];
                let mut idx = 0;

                while idx < total {
                    let sym = reader.decode_huffman(&cl_table)?;
                    match sym {
                        0..=15 => {
                            combined[idx] = sym as u8;
                            idx += 1;
                        }
                        16 => {
                            if idx == 0 {
                                return Err(PngError::InvalidData);
                            }
                            let rep = reader.read_bits(2)? as usize + 3;
                            let prev = combined[idx - 1];
                            for _ in 0..rep {
                                if idx >= total {
                                    return Err(PngError::InvalidData);
                                }
                                combined[idx] = prev;
                                idx += 1;
                            }
                        }
                        17 => {
                            let rep = reader.read_bits(3)? as usize + 3;
                            for _ in 0..rep {
                                if idx >= total {
                                    return Err(PngError::InvalidData);
                                }
                                combined[idx] = 0;
                                idx += 1;
                            }
                        }
                        18 => {
                            let rep = reader.read_bits(7)? as usize + 11;
                            for _ in 0..rep {
                                if idx >= total {
                                    return Err(PngError::InvalidData);
                                }
                                combined[idx] = 0;
                                idx += 1;
                            }
                        }
                        _ => return Err(PngError::InvalidData),
                    }
                }

                let lit_table = HuffTable::build(&combined[..hlit], hlit)?;
                let dist_table = HuffTable::build(&combined[hlit..hlit + hdist], hdist)?;
                out_pos = decode_codes(reader, output, out_pos, &lit_table, &dist_table)?;
            }
            _ => return Err(PngError::InvalidData),
        }

        if bfinal != 0 {
            break;
        }
    }

    Ok(out_pos)
}

/// Decode literal/length + distance codes until end-of-block (symbol 256).
fn decode_codes(
    reader: &mut BitReader,
    output: &mut [u8],
    mut out_pos: usize,
    lit_table: &HuffTable,
    dist_table: &HuffTable,
) -> Result<usize, PngError> {
    loop {
        let sym = reader.decode_huffman(lit_table)?;

        if sym < 256 {
            if out_pos >= output.len() {
                return Err(PngError::DataSizeMismatch);
            }
            output[out_pos] = sym as u8;
            out_pos += 1;
        } else if sym == 256 {
            break;
        } else {
            let li = (sym - 257) as usize;
            if li >= 29 {
                return Err(PngError::InvalidData);
            }
            let length = LEN_BASE[li] as usize + reader.read_bits(LEN_EXTRA[li])? as usize;

            let di = reader.decode_huffman(dist_table)? as usize;
            if di >= 30 {
                return Err(PngError::InvalidData);
            }
            let distance = DIST_BASE[di] as usize + reader.read_bits(DIST_EXTRA[di])? as usize;

            if distance > out_pos {
                return Err(PngError::InvalidData);
            }

            for _ in 0..length {
                if out_pos >= output.len() {
                    return Err(PngError::DataSizeMismatch);
                }
                output[out_pos] = output[out_pos - distance];
                out_pos += 1;
            }
        }
    }

    Ok(out_pos)
}
