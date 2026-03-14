// Minimal PNG decoder — no external dependencies, no_std.
//
// Supports 8-bit RGB (color type 2) and 8-bit RGBA (color type 6) images.
// Implements zlib/DEFLATE decompression and all 5 PNG filter types.
// Returns errors for invalid input — never panics.

/// PNG decoding errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngError {
    /// Not a valid PNG file (bad magic bytes).
    InvalidSignature,
    /// Unexpected end of data.
    Truncated,
    /// Width or height is zero.
    ZeroDimensions,
    /// Bit depth or color type not supported (only 8-bit RGB/RGBA).
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

/// Read a big-endian u32 from a byte slice.
fn png_read_be_u32(data: &[u8]) -> u32 {
    ((data[0] as u32) << 24) | ((data[1] as u32) << 16) | ((data[2] as u32) << 8) | (data[3] as u32)
}

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
    let chunk_len = png_read_be_u32(&data[8..12]) as usize;
    let chunk_type = &data[12..16];
    if chunk_type != b"IHDR" || chunk_len != 13 {
        return Err(PngError::MissingIhdr);
    }

    let ihdr = &data[16..29];
    let width = png_read_be_u32(&ihdr[0..4]);
    let height = png_read_be_u32(&ihdr[4..8]);
    let bit_depth = ihdr[8];
    let color_type = ihdr[9];

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

/// Decode a PNG image to BGRA8888 pixel data.
///
/// `data` is the complete PNG file. `output` must be at least
/// `width * height * 4 + height` bytes (extra `height` bytes for filter bytes
/// during decompression). Returns the header on success.
///
/// Only supports 8-bit RGB (color type 2) and 8-bit RGBA (color type 6).
pub fn png_decode(data: &[u8], output: &mut [u8]) -> Result<PngHeader, PngError> {
    let header = png_header(data)?;

    if header.bit_depth != 8 {
        return Err(PngError::UnsupportedFormat);
    }
    let channels: u32 = match header.color_type {
        2 => 3, // RGB
        6 => 4, // RGBA
        _ => return Err(PngError::UnsupportedFormat),
    };

    let pixels = header
        .width
        .checked_mul(header.height)
        .ok_or(PngError::DimensionOverflow)?;
    let out_size = pixels.checked_mul(4).ok_or(PngError::DimensionOverflow)?;

    // Scanline size: filter byte + width * channels
    let scanline_bytes = 1 + (header.width as usize) * (channels as usize);
    let total_raw = scanline_bytes * (header.height as usize);

    // We need the output buffer to hold at least total_raw bytes for
    // decompression, then we convert in-place to BGRA.
    let min_buf = if total_raw > out_size as usize {
        total_raw
    } else {
        out_size as usize
    };
    if output.len() < min_buf {
        return Err(PngError::BufferTooSmall);
    }

    // Collect IDAT chunk offsets
    let mut idat_offsets: [usize; 64] = [0; 64];
    let mut idat_lengths: [usize; 64] = [0; 64];
    let mut idat_count = 0;
    let mut pos = 8;

    while pos + 8 <= data.len() {
        let chunk_len = png_read_be_u32(&data[pos..pos + 4]) as usize;
        let chunk_type = &data[pos + 4..pos + 8];

        if pos + 8 + chunk_len + 4 > data.len() {
            if chunk_type == b"IDAT" {
                return Err(PngError::Truncated);
            }
            break;
        }

        if chunk_type == b"IDAT" {
            if idat_count < 64 {
                idat_offsets[idat_count] = pos + 8;
                idat_lengths[idat_count] = chunk_len;
                idat_count += 1;
            }
        } else if chunk_type == b"IEND" {
            break;
        }

        pos += 8 + chunk_len + 4;
    }

    if idat_count == 0 {
        return Err(PngError::Truncated);
    }

    // Inflate zlib stream from IDAT chunks into output buffer
    let decompressed_len = inflate_idat(
        data,
        &idat_offsets[..idat_count],
        &idat_lengths[..idat_count],
        &mut output[..total_raw],
    )?;
    if decompressed_len != total_raw {
        return Err(PngError::DataSizeMismatch);
    }

    // Unfilter scanlines in-place
    let bpp = channels as usize;
    unfilter_scanlines(output, header.width as usize, header.height as usize, bpp)?;

    // Convert raw pixels to BGRA8888 in-place.
    //
    // For RGBA (4 channels): raw_start(y) = y*(1+w*4)+1, out_start(y) = y*w*4.
    //   raw_start - out_start = y+1 > 0, so raw is always ahead of output.
    //   Forward processing (y=0..h-1, x=0..w-1) is safe — we never overwrite
    //   unread raw data.
    //
    // For RGB (3 channels): raw has 3 bytes per pixel, output has 4.
    //   raw_start(y) = y*(1+w*3)+1, out_start(y) = y*w*4.
    //   out_start can exceed raw_start for large y, so we must process
    //   backward (last row first, last pixel first within each row).
    let out_stride = header.width as usize * 4;

    if channels == 4 {
        // RGBA → BGRA: forward processing is safe
        for y in 0..header.height as usize {
            let raw_row_start = y * scanline_bytes + 1;
            let out_row_start = y * out_stride;
            for x in 0..header.width as usize {
                let ri = raw_row_start + x * 4;
                let oi = out_row_start + x * 4;
                let r = output[ri];
                let g = output[ri + 1];
                let b = output[ri + 2];
                let a = output[ri + 3];
                output[oi] = b;
                output[oi + 1] = g;
                output[oi + 2] = r;
                output[oi + 3] = a;
            }
        }
    } else {
        // RGB → BGRA: backward processing (output is wider than raw)
        for y in (0..header.height as usize).rev() {
            let raw_row_start = y * scanline_bytes + 1;
            let out_row_start = y * out_stride;
            for x in (0..header.width as usize).rev() {
                let ri = raw_row_start + x * 3;
                let oi = out_row_start + x * 4;
                let r = output[ri];
                let g = output[ri + 1];
                let b = output[ri + 2];
                output[oi] = b;
                output[oi + 1] = g;
                output[oi + 2] = r;
                output[oi + 3] = 255;
            }
        }
    }

    Ok(header)
}

/// Unfilter PNG scanlines in-place.
fn unfilter_scanlines(
    data: &mut [u8],
    width: usize,
    height: usize,
    bpp: usize,
) -> Result<(), PngError> {
    let scanline_bytes = 1 + width * bpp;

    for y in 0..height {
        let row_start = y * scanline_bytes;
        let filter_type = data[row_start];
        let ps = row_start + 1; // pixel start
        let row_len = width * bpp;

        if ps + row_len > data.len() {
            return Err(PngError::Truncated);
        }

        match filter_type {
            0 => {} // None
            1 => {
                // Sub: Raw(x) += Raw(x - bpp)
                for i in bpp..row_len {
                    data[ps + i] = data[ps + i].wrapping_add(data[ps + i - bpp]);
                }
            }
            2 => {
                // Up: Raw(x) += Prior(x)
                if y > 0 {
                    let prev_ps = (y - 1) * scanline_bytes + 1;
                    for i in 0..row_len {
                        data[ps + i] = data[ps + i].wrapping_add(data[prev_ps + i]);
                    }
                }
            }
            3 => {
                // Average: Raw(x) += floor((Raw(x-bpp) + Prior(x)) / 2)
                let prev_ps = if y > 0 {
                    (y - 1) * scanline_bytes + 1
                } else {
                    0 // won't be used when y == 0
                };
                for i in 0..row_len {
                    let a = if i >= bpp {
                        data[ps + i - bpp] as u16
                    } else {
                        0
                    };
                    let b = if y > 0 { data[prev_ps + i] as u16 } else { 0 };
                    data[ps + i] = data[ps + i].wrapping_add(((a + b) / 2) as u8);
                }
            }
            4 => {
                // Paeth
                let prev_ps = if y > 0 {
                    (y - 1) * scanline_bytes + 1
                } else {
                    0
                };
                for i in 0..row_len {
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
fn inflate_idat(
    file_data: &[u8],
    offsets: &[usize],
    lengths: &[usize],
    output: &mut [u8],
) -> Result<usize, PngError> {
    let mut reader = BitReader::new(file_data, offsets, lengths);

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

/// Bit reader over concatenated IDAT chunks.
struct BitReader<'a> {
    data: &'a [u8],
    offsets: &'a [usize],
    lengths: &'a [usize],
    chunk_idx: usize,
    byte_idx: usize,
    bits: u32,
    nbits: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], offsets: &'a [usize], lengths: &'a [usize]) -> Self {
        Self {
            data,
            offsets,
            lengths,
            chunk_idx: 0,
            byte_idx: 0,
            bits: 0,
            nbits: 0,
        }
    }

    fn next_byte(&mut self) -> Option<u8> {
        while self.chunk_idx < self.offsets.len() {
            if self.byte_idx < self.lengths[self.chunk_idx] {
                let b = self.data[self.offsets[self.chunk_idx] + self.byte_idx];
                self.byte_idx += 1;
                return Some(b);
            }
            self.chunk_idx += 1;
            self.byte_idx = 0;
        }
        None
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
    /// This is a simple bit-by-bit decode — slow but uses zero extra memory.
    /// For each possible code length (1..=15), we check if the accumulated
    /// bits match any code at that length.
    fn decode_huffman(&mut self, table: &HuffTable) -> Result<u16, PngError> {
        let mut code: u32 = 0;
        for len in 1..=15u8 {
            code = (code << 1) | self.read_bits(1)?;
            // Check if this code matches any symbol at this length
            let start = table.offsets[len as usize] as usize;
            let count = table.counts[len as usize] as usize;
            // The canonical code for the first symbol at this length
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
    /// Sorted symbols array (max 320 entries: 288 lit/len + 32 dist).
    symbols: [u16; 320],
    /// Number of codes at each length (index 0 unused, 1..=15).
    counts: [u16; 16],
    /// Starting index in `symbols` for each length.
    offsets: [u16; 16],
    /// First canonical code at each length.
    first_code: [u32; 16],
}

impl HuffTable {
    /// Build from an array of code lengths. `code_lengths[i]` = bit length
    /// for symbol `i`. Length 0 means unused.
    fn build(code_lengths: &[u8], num_symbols: usize) -> Result<Self, PngError> {
        let mut table = HuffTable {
            symbols: [0u16; 320],
            counts: [0u16; 16],
            offsets: [0u16; 16],
            first_code: [0u32; 16],
        };

        // Count codes per length
        for i in 0..num_symbols {
            let len = code_lengths[i] as usize;
            if len > 15 {
                return Err(PngError::InvalidData);
            }
            if len > 0 {
                table.counts[len] += 1;
            }
        }

        // Compute offsets (prefix sum)
        let mut offset = 0u16;
        for i in 1..=15 {
            table.offsets[i] = offset;
            offset += table.counts[i];
        }

        // Compute first canonical code at each length
        let mut code = 0u32;
        for i in 1..=15 {
            code = (code + table.counts[i - 1] as u32) << 1;
            table.first_code[i] = code;
        }

        // Fill symbols sorted by (length, code)
        // We assign codes in symbol order for each length, matching canonical order.
        let mut pos = [0u16; 16]; // current insertion position per length
        for i in 0..16 {
            pos[i] = table.offsets[i];
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
