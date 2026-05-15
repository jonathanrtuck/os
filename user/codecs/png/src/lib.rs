//! Full PNG decoder — no external dependencies, no_std compatible, no alloc.
//!
//! Supports all PNG color types (0, 2, 3, 4, 6) at all valid bit depths
//! (1, 2, 4, 8, 16), including Adam7 interlacing, PLTE palettes, and
//! tRNS transparency. Decodes into a caller-provided BGRA8888 output buffer.
//!
//! Buffer requirement: caller must provide at least `png_decode_buf_size(data)`
//! bytes. For non-interlaced images this is approximately `w*h*4 + raw_size`.
//! For interlaced images the raw_size includes all 7 Adam7 passes.

#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngError {
    InvalidSignature,
    Truncated,
    ZeroDimensions,
    UnsupportedFormat,
    MissingIhdr,
    InvalidData,
    DataSizeMismatch,
    BufferTooSmall,
    DimensionOverflow,
    CrcMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngHeader {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: u8,
}

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn read_be_u32(data: &[u8]) -> u32 {
    ((data[0] as u32) << 24) | ((data[1] as u32) << 16) | ((data[2] as u32) << 8) | (data[3] as u32)
}

fn read_be_u16(data: &[u8]) -> u16 {
    ((data[0] as u16) << 8) | (data[1] as u16)
}

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

fn raw_row_bytes(width: usize, color_type: u8, bit_depth: u8) -> usize {
    (width * bits_per_pixel(color_type, bit_depth) + 7) / 8
}

fn filter_bpp(color_type: u8, bit_depth: u8) -> usize {
    let bpp = bits_per_pixel(color_type, bit_depth) / 8;

    if bpp == 0 { 1 } else { bpp }
}

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

// ── CRC32 (IEEE 802.3, polynomial 0xEDB88320 reflected) ──────────

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

// ── Header parsing ───────────────────────────────────────────────

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

// ── Chunk parsing ────────────────────────────────────────────────

struct PngChunks {
    first_idat_pos: usize,
    palette: [[u8; 3]; 256],
    palette_count: usize,
    trns_alpha: [u8; 256],
    trns_count: usize,
    trns_gray: u16,
    trns_gray_set: bool,
    trns_rgb: [u16; 3],
    trns_rgb_set: bool,
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

fn parse_chunks(data: &[u8], color_type: u8) -> Result<PngChunks, PngError> {
    let mut chunks = PngChunks::new();

    if data.len() < 29 {
        return Err(PngError::Truncated);
    }

    chunks.interlace = data[28];

    if chunks.interlace > 1 {
        return Err(PngError::InvalidData);
    }

    let mut pos = 33;

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
                _ => {}
            }
        } else if chunk_type == b"IEND" {
            break;
        }

        pos += 8 + chunk_len + 4;
    }

    if chunks.first_idat_pos == 0 {
        return Err(PngError::Truncated);
    }

    if color_type == 3 && chunks.palette_count == 0 {
        return Err(PngError::InvalidData);
    }

    Ok(chunks)
}

// ── Full decode ──────────────────────────────────────────────────

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
    let scanline = rrb + 1;
    let total_raw = scanline * h;
    let min_buf = bgra_size + total_raw;

    if output.len() < min_buf {
        return Err(PngError::BufferTooSmall);
    }

    let raw_start = bgra_size;
    let decompressed = inflate_idat(
        data,
        chunks.first_idat_pos,
        &mut output[raw_start..raw_start + total_raw],
    )?;

    if decompressed != total_raw {
        return Err(PngError::DataSizeMismatch);
    }

    let bpp = filter_bpp(color_type, bit_depth);

    unfilter_scanlines(&mut output[raw_start..], rrb, h, bpp)?;

    for y in 0..h {
        let bgra_row = y * w * 4;
        let (bgra_part, raw_part) = output.split_at_mut(raw_start);
        let raw_slice = &raw_part[y * scanline + 1..y * scanline + 1 + rrb];
        let bgra_slice = &mut bgra_part[bgra_row..bgra_row + w * 4];
        row_to_bgra(raw_slice, bgra_slice, w, color_type, bit_depth, chunks);
    }

    Ok(())
}

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

    let mut i = 0;

    while i < bgra_size {
        output[i] = 0;
        i += 1;
    }

    let raw_start = bgra_size;
    let decompressed = inflate_idat(
        data,
        chunks.first_idat_pos,
        &mut output[raw_start..raw_start + total_raw],
    )?;

    if decompressed != total_raw {
        return Err(PngError::DataSizeMismatch);
    }

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
        let abs_start = raw_start + pass_offset;

        unfilter_scanlines(&mut output[abs_start..abs_start + pass_raw], rrb, ph, bpp)?;

        for py in 0..ph {
            let raw_row_start = abs_start + py * scanline + 1;
            let dest_y = ys + py * ystep;

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

fn scatter_row(
    output: &mut [u8],
    _raw_start: usize,
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
    let mut raw_buf = [0u8; 8192];
    let copy_len = rrb.min(raw_buf.len());

    raw_buf[..copy_len].copy_from_slice(&output[raw_row_start..raw_row_start + copy_len]);

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

// ── Pixel conversion ────────────────────────────────────────────

fn pixel_to_bgra(
    raw: &[u8],
    x: usize,
    color_type: u8,
    bit_depth: u8,
    chunks: &PngChunks,
    out: &mut [u8; 4],
) {
    match (color_type, bit_depth) {
        (0, 8) => {
            let g = raw[x];
            let a = if chunks.trns_gray_set && g as u16 == chunks.trns_gray {
                0
            } else {
                255
            };

            *out = [g, g, g, a];
        }
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
        (4, 8) => {
            let g = raw[x * 2];
            let a = raw[x * 2 + 1];

            *out = [g, g, g, a];
        }
        (4, 16) => {
            let g = raw[x * 4];
            let a = raw[x * 4 + 2];

            *out = [g, g, g, a];
        }
        (6, 8) => {
            let i = x * 4;

            *out = [raw[i + 2], raw[i + 1], raw[i], raw[i + 3]];
        }
        (6, 16) => {
            let i = x * 8;

            *out = [raw[i + 4], raw[i + 2], raw[i], raw[i + 6]];
        }
        _ => {
            *out = [0, 0, 0, 255];
        }
    }
}

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

fn unpack_sub_byte(raw: &[u8], x: usize, bit_depth: u8) -> u8 {
    let bd = bit_depth as usize;
    let pixels_per_byte = 8 / bd;
    let byte_idx = x / pixels_per_byte;
    let bit_offset = (pixels_per_byte - 1 - x % pixels_per_byte) * bd;
    let mask = (1u8 << bd) - 1;

    (raw[byte_idx] >> bit_offset) & mask
}

fn scale_to_8bit(val: u8, bit_depth: u8) -> u8 {
    match bit_depth {
        1 => val * 255,
        2 => val * 85,
        4 => val * 17,
        8 => val,
        _ => val,
    }
}

// ── Scanline unfiltering ─────────────────────────────────────────

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
        let ps = row_start + 1;

        if ps + row_len > data.len() {
            return Err(PngError::Truncated);
        }

        match filter_type {
            0 => {}
            1 => {
                let mut i = bpp;

                while i < row_len {
                    data[ps + i] = data[ps + i].wrapping_add(data[ps + i - bpp]);
                    i += 1;
                }
            }
            2 => {
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

// ── zlib / DEFLATE decompression ─────────────────────────────────

fn inflate_idat(
    file_data: &[u8],
    first_idat_pos: usize,
    output: &mut [u8],
) -> Result<usize, PngError> {
    let mut reader = BitReader::new(file_data, first_idat_pos)?;
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

struct BitReader<'a> {
    data: &'a [u8],
    chunk_data_start: usize,
    chunk_data_len: usize,
    chunk_pos: usize,
    byte_idx: usize,
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

    fn advance_chunk(&mut self) {
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

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];

const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];

const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn inflate_stream(reader: &mut BitReader, output: &mut [u8]) -> Result<usize, PngError> {
    let mut out_pos = 0;

    loop {
        let bfinal = reader.read_bits(1)?;
        let btype = reader.read_bits(2)?;

        match btype {
            0 => {
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

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate alloc;
    extern crate std;

    use alloc::{format, vec, vec::Vec};

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);

        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"))
    }

    fn decode_fixture(name: &str) -> (PngHeader, Vec<u8>) {
        let data = fixture(name);
        let buf_size = png_decode_buf_size(&data).unwrap();
        let mut output = vec![0u8; buf_size];
        let header = png_decode(&data, &mut output).unwrap();
        let pixel_count = header.width as usize * header.height as usize * 4;

        output.truncate(pixel_count);

        (header, output)
    }

    // ── Header parsing ───────────────────────────────────────────

    #[test]
    fn header_basn0g08() {
        let data = fixture("basn0g08.png");
        let h = png_header(&data).unwrap();

        assert_eq!(h.width, 32);
        assert_eq!(h.height, 32);
        assert_eq!(h.bit_depth, 8);
        assert_eq!(h.color_type, 0);
    }

    #[test]
    fn header_basn2c08() {
        let data = fixture("basn2c08.png");
        let h = png_header(&data).unwrap();

        assert_eq!(h.width, 32);
        assert_eq!(h.height, 32);
        assert_eq!(h.bit_depth, 8);
        assert_eq!(h.color_type, 2);
    }

    #[test]
    fn header_basn6a08() {
        let data = fixture("basn6a08.png");
        let h = png_header(&data).unwrap();

        assert_eq!(h.width, 32);
        assert_eq!(h.height, 32);
        assert_eq!(h.bit_depth, 8);
        assert_eq!(h.color_type, 6);
    }

    #[test]
    fn header_invalid_signature() {
        let data = [0u8; 100];

        assert_eq!(png_header(&data), Err(PngError::InvalidSignature));
    }

    #[test]
    fn header_truncated() {
        assert_eq!(
            png_header(&[0x89, 0x50, 0x4E, 0x47]),
            Err(PngError::Truncated)
        );
    }

    // ── CRC32 ────────────────────────────────────────────────────

    #[test]
    fn crc32_empty() {
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    // ── Format validation ────────────────────────────────────────

    #[test]
    fn validate_all_valid_formats() {
        let valid = [
            (0, 1),
            (0, 2),
            (0, 4),
            (0, 8),
            (0, 16),
            (2, 8),
            (2, 16),
            (3, 1),
            (3, 2),
            (3, 4),
            (3, 8),
            (4, 8),
            (4, 16),
            (6, 8),
            (6, 16),
        ];

        for (ct, bd) in valid {
            assert!(
                validate_format(ct, bd).is_ok(),
                "({ct}, {bd}) should be valid"
            );
        }
    }

    #[test]
    fn validate_invalid_formats() {
        assert!(validate_format(0, 3).is_err());
        assert!(validate_format(2, 4).is_err());
        assert!(validate_format(3, 16).is_err());
        assert!(validate_format(5, 8).is_err());
    }

    // ── Grayscale (color type 0) ─────────────────────────────────

    #[test]
    fn decode_gray_1bit() {
        let (h, pixels) = decode_fixture("basn0g01.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.color_type, 0);
        assert_eq!(h.bit_depth, 1);
        assert!(!pixels.is_empty());
        assert!(pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn decode_gray_2bit() {
        let (h, _) = decode_fixture("basn0g02.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.bit_depth, 2);
    }

    #[test]
    fn decode_gray_4bit() {
        let (h, _) = decode_fixture("basn0g04.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.bit_depth, 4);
    }

    #[test]
    fn decode_gray_8bit() {
        let (h, pixels) = decode_fixture("basn0g08.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(pixels.len(), 32 * 32 * 4);
        // Gray pixels: B=G=R, A=255
        assert_eq!(pixels[0], pixels[1]);
        assert_eq!(pixels[1], pixels[2]);
        assert_eq!(pixels[3], 255);
    }

    #[test]
    fn decode_gray_16bit() {
        let (h, pixels) = decode_fixture("basn0g16.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert!(!pixels.is_empty());
    }

    // ── RGB (color type 2) ───────────────────────────────────────

    #[test]
    fn decode_rgb_8bit() {
        let (h, pixels) = decode_fixture("basn2c08.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.color_type, 2);
        assert_eq!(pixels.len(), 32 * 32 * 4);
        assert_eq!(pixels[3], 255); // alpha is opaque
    }

    #[test]
    fn decode_rgb_16bit() {
        let (h, pixels) = decode_fixture("basn2c16.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert!(!pixels.is_empty());
    }

    // ── Indexed (color type 3) ───────────────────────────────────

    #[test]
    fn decode_indexed_1bit() {
        let (h, _) = decode_fixture("basn3p01.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.color_type, 3);
    }

    #[test]
    fn decode_indexed_2bit() {
        let (h, _) = decode_fixture("basn3p02.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_indexed_4bit() {
        let (h, _) = decode_fixture("basn3p04.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_indexed_8bit() {
        let (h, pixels) = decode_fixture("basn3p08.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(pixels.len(), 32 * 32 * 4);
    }

    // ── Gray + Alpha (color type 4) ──────────────────────────────

    #[test]
    fn decode_gray_alpha_8bit() {
        let (h, pixels) = decode_fixture("basn4a08.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.color_type, 4);

        // Some pixels should have varying alpha
        let has_varying_alpha = pixels.chunks(4).any(|px| px[3] != 255);

        assert!(has_varying_alpha);
    }

    #[test]
    fn decode_gray_alpha_16bit() {
        let (h, _) = decode_fixture("basn4a16.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    // ── RGBA (color type 6) ──────────────────────────────────────

    #[test]
    fn decode_rgba_8bit() {
        let (h, pixels) = decode_fixture("basn6a08.png");

        assert_eq!((h.width, h.height), (32, 32));
        assert_eq!(h.color_type, 6);
        assert_eq!(pixels.len(), 32 * 32 * 4);
    }

    #[test]
    fn decode_rgba_16bit() {
        let (h, _) = decode_fixture("basn6a16.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    // ── Adam7 interlacing ────────────────────────────────────────

    #[test]
    fn decode_interlaced_gray_1bit() {
        let (h, _) = decode_fixture("basi0g01.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_interlaced_gray_8bit() {
        let (h, _) = decode_fixture("basi0g08.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_interlaced_rgb_8bit() {
        let (h, _) = decode_fixture("basi2c08.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_interlaced_indexed_8bit() {
        let (h, _) = decode_fixture("basi3p08.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_interlaced_gray_alpha_8bit() {
        let (h, _) = decode_fixture("basi4a08.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_interlaced_rgba_8bit() {
        let (h, _) = decode_fixture("basi6a08.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn interlaced_matches_non_interlaced() {
        let (_, ni_pixels) = decode_fixture("basn0g08.png");
        let (_, i_pixels) = decode_fixture("basi0g08.png");

        assert_eq!(
            ni_pixels, i_pixels,
            "interlaced and non-interlaced should produce identical output"
        );
    }

    #[test]
    fn interlaced_rgb_matches() {
        let (_, ni) = decode_fixture("basn2c08.png");
        let (_, i) = decode_fixture("basi2c08.png");

        assert_eq!(ni, i);
    }

    #[test]
    fn interlaced_indexed_matches() {
        let (_, ni) = decode_fixture("basn3p08.png");
        let (_, i) = decode_fixture("basi3p08.png");

        assert_eq!(ni, i);
    }

    #[test]
    fn interlaced_gray_alpha_matches() {
        let (_, ni) = decode_fixture("basn4a08.png");
        let (_, i) = decode_fixture("basi4a08.png");

        assert_eq!(ni, i);
    }

    #[test]
    fn interlaced_rgba_matches() {
        let (_, ni) = decode_fixture("basn6a08.png");
        let (_, i) = decode_fixture("basi6a08.png");

        assert_eq!(ni, i);
    }

    // ── Filters ──────────────────────────────────────────────────

    #[test]
    fn filter_none() {
        let (_, _) = decode_fixture("f00n0g08.png");
    }

    #[test]
    fn filter_sub() {
        let (_, _) = decode_fixture("f01n0g08.png");
    }

    #[test]
    fn filter_up() {
        let (_, _) = decode_fixture("f02n0g08.png");
    }

    #[test]
    fn filter_average() {
        let (_, _) = decode_fixture("f03n0g08.png");
    }

    #[test]
    fn filter_paeth() {
        let (_, _) = decode_fixture("f04n0g08.png");
    }

    #[test]
    fn filter_all_rgb() {
        let (_, _) = decode_fixture("f00n2c08.png");
        let (_, _) = decode_fixture("f01n2c08.png");
        let (_, _) = decode_fixture("f02n2c08.png");
        let (_, _) = decode_fixture("f03n2c08.png");
        let (_, _) = decode_fixture("f04n2c08.png");
    }

    // ── Odd sizes ────────────────────────────────────────────────

    #[test]
    fn decode_1x1() {
        let (h, _) = decode_fixture("s01n3p01.png");

        assert_eq!((h.width, h.height), (1, 1));
    }

    #[test]
    fn decode_2x2() {
        let (h, _) = decode_fixture("s02n3p01.png");

        assert_eq!((h.width, h.height), (2, 2));
    }

    #[test]
    fn decode_3x3() {
        let (h, _) = decode_fixture("s03n3p01.png");

        assert_eq!((h.width, h.height), (3, 3));
    }

    #[test]
    fn decode_4x4() {
        let (h, _) = decode_fixture("s04n3p01.png");

        assert_eq!((h.width, h.height), (4, 4));
    }

    #[test]
    fn decode_32x32() {
        let (h, _) = decode_fixture("s32n3p04.png");

        assert_eq!((h.width, h.height), (32, 32));
    }

    #[test]
    fn decode_odd_sizes_interlaced() {
        let (h, _) = decode_fixture("s01i3p01.png");

        assert_eq!((h.width, h.height), (1, 1));

        let (h, _) = decode_fixture("s02i3p01.png");

        assert_eq!((h.width, h.height), (2, 2));

        let (h, _) = decode_fixture("s03i3p01.png");

        assert_eq!((h.width, h.height), (3, 3));

        let (h, _) = decode_fixture("s04i3p01.png");

        assert_eq!((h.width, h.height), (4, 4));
    }

    #[test]
    fn decode_odd_sizes_5_to_9() {
        for size in 5..=9 {
            let name = format!("s{:02}n3p02.png", size);
            let (h, _) = decode_fixture(&name);

            assert_eq!((h.width, h.height), (size, size));
        }
    }

    #[test]
    fn decode_odd_sizes_32_to_40() {
        for size in 32..=40 {
            let name = format!("s{:02}n3p04.png", size);
            let (h, _) = decode_fixture(&name);

            assert_eq!((h.width, h.height), (size, size));
        }
    }

    // ── Background (bKGD) ────────────────────────────────────────

    #[test]
    fn decode_background_images() {
        decode_fixture("bgai4a08.png");
        decode_fixture("bgai4a16.png");
        decode_fixture("bgan6a08.png");
        decode_fixture("bgan6a16.png");
        decode_fixture("bgbn4a08.png");
        decode_fixture("bggn4a16.png");
        decode_fixture("bgwn6a08.png");
        decode_fixture("bgyn6a16.png");
    }

    // ── Transparency (tRNS) ──────────────────────────────────────

    #[test]
    fn decode_trns_gray() {
        let (_, pixels) = decode_fixture("tbwn0g16.png");
        let has_transparent = pixels.chunks(4).any(|px| px[3] == 0);

        assert!(
            has_transparent,
            "tRNS gray should produce transparent pixels"
        );
    }

    #[test]
    fn decode_trns_rgb() {
        let (_, pixels) = decode_fixture("tbrn2c08.png");
        let has_transparent = pixels.chunks(4).any(|px| px[3] == 0);

        assert!(
            has_transparent,
            "tRNS RGB should produce transparent pixels"
        );
    }

    #[test]
    fn decode_trns_indexed() {
        let (_, pixels) = decode_fixture("tbbn3p08.png");
        let has_transparent = pixels.chunks(4).any(|px| px[3] < 255);

        assert!(
            has_transparent,
            "tRNS indexed should produce transparent pixels"
        );
    }

    // ── Compression levels ───────────────────────────────────────

    #[test]
    fn decode_compression_levels() {
        let (_, z0) = decode_fixture("z00n2c08.png");
        let (_, z3) = decode_fixture("z03n2c08.png");
        let (_, z6) = decode_fixture("z06n2c08.png");
        let (_, z9) = decode_fixture("z09n2c08.png");

        assert_eq!(
            z0, z3,
            "compression level 0 and 3 should decode identically"
        );
        assert_eq!(z3, z6);
        assert_eq!(z6, z9);
    }

    // ── Gamma (decoded the same, just different gAMA chunks) ─────

    #[test]
    fn decode_gamma_images() {
        decode_fixture("g03n0g16.png");
        decode_fixture("g04n0g16.png");
        decode_fixture("g05n0g16.png");
        decode_fixture("g07n0g16.png");
        decode_fixture("g10n0g16.png");
        decode_fixture("g25n0g16.png");
        decode_fixture("g03n2c08.png");
        decode_fixture("g04n2c08.png");
        decode_fixture("g05n2c08.png");
        decode_fixture("g07n2c08.png");
        decode_fixture("g10n2c08.png");
        decode_fixture("g25n2c08.png");
        decode_fixture("g03n3p04.png");
        decode_fixture("g04n3p04.png");
        decode_fixture("g05n3p04.png");
        decode_fixture("g07n3p04.png");
        decode_fixture("g10n3p04.png");
        decode_fixture("g25n3p04.png");
    }

    // ── Corrupt files (x* prefix = should fail) ──────────────────

    #[test]
    fn reject_corrupt_files() {
        let corrupt = [
            "xc1n0g08.png",
            "xc9n2c08.png",
            "xcrn0g04.png",
            "xcsn0g01.png",
            "xd0n2c08.png",
            "xd3n2c08.png",
            "xd9n2c08.png",
            "xdtn0g01.png",
            "xhdn0g08.png",
            "xlfn0g04.png",
            "xs1n0g01.png",
            "xs2n0g01.png",
            "xs4n0g01.png",
            "xs7n0g01.png",
        ];

        for name in corrupt {
            let data = fixture(name);
            let buf_size = png_decode_buf_size(&data);

            if let Ok(size) = buf_size {
                let mut output = vec![0u8; size];

                assert!(
                    png_decode(&data, &mut output).is_err(),
                    "{name} should fail to decode"
                );
            }
            // If buf_size itself errors, that's also a valid rejection
        }
    }

    // ── Buffer too small ─────────────────────────────────────────

    #[test]
    fn buffer_too_small() {
        let data = fixture("basn0g08.png");
        let mut output = [0u8; 10];

        assert_eq!(
            png_decode(&data, &mut output),
            Err(PngError::BufferTooSmall)
        );
    }

    // ── Paeth predictor ──────────────────────────────────────────

    #[test]
    fn paeth_predictor() {
        assert_eq!(paeth_pred(10, 20, 15), 15); // p=15, pa=5, pb=5, pc=0 → c wins
        assert_eq!(paeth_pred(10, 20, 10), 20); // p=20, pa=10, pb=0, pc=10 → pb<=pc → b
        assert_eq!(paeth_pred(10, 20, 30), 10); // p=0, pa=10, pb=20, pc=30 → pa≤pb, pa≤pc → a
    }

    // ── Sub-byte unpacking ───────────────────────────────────────

    #[test]
    fn unpack_1bit() {
        let data = [0b1010_0110u8];

        assert_eq!(unpack_sub_byte(&data, 0, 1), 1);
        assert_eq!(unpack_sub_byte(&data, 1, 1), 0);
        assert_eq!(unpack_sub_byte(&data, 2, 1), 1);
        assert_eq!(unpack_sub_byte(&data, 3, 1), 0);
        assert_eq!(unpack_sub_byte(&data, 4, 1), 0);
        assert_eq!(unpack_sub_byte(&data, 5, 1), 1);
        assert_eq!(unpack_sub_byte(&data, 6, 1), 1);
        assert_eq!(unpack_sub_byte(&data, 7, 1), 0);
    }

    #[test]
    fn unpack_2bit() {
        let data = [0b11_10_01_00u8];

        assert_eq!(unpack_sub_byte(&data, 0, 2), 3);
        assert_eq!(unpack_sub_byte(&data, 1, 2), 2);
        assert_eq!(unpack_sub_byte(&data, 2, 2), 1);
        assert_eq!(unpack_sub_byte(&data, 3, 2), 0);
    }

    #[test]
    fn unpack_4bit() {
        let data = [0xABu8];

        assert_eq!(unpack_sub_byte(&data, 0, 4), 0xA);
        assert_eq!(unpack_sub_byte(&data, 1, 4), 0xB);
    }

    // ── Scale to 8-bit ───────────────────────────────────────────

    #[test]
    fn scale_1bit_to_8bit() {
        assert_eq!(scale_to_8bit(0, 1), 0);
        assert_eq!(scale_to_8bit(1, 1), 255);
    }

    #[test]
    fn scale_2bit_to_8bit() {
        assert_eq!(scale_to_8bit(0, 2), 0);
        assert_eq!(scale_to_8bit(1, 2), 85);
        assert_eq!(scale_to_8bit(2, 2), 170);
        assert_eq!(scale_to_8bit(3, 2), 255);
    }

    #[test]
    fn scale_4bit_to_8bit() {
        assert_eq!(scale_to_8bit(0, 4), 0);
        assert_eq!(scale_to_8bit(15, 4), 255);
    }

    // ── Bits per pixel ───────────────────────────────────────────

    #[test]
    fn bpp_values() {
        assert_eq!(bits_per_pixel(0, 1), 1);
        assert_eq!(bits_per_pixel(0, 8), 8);
        assert_eq!(bits_per_pixel(0, 16), 16);
        assert_eq!(bits_per_pixel(2, 8), 24);
        assert_eq!(bits_per_pixel(2, 16), 48);
        assert_eq!(bits_per_pixel(3, 1), 1);
        assert_eq!(bits_per_pixel(3, 8), 8);
        assert_eq!(bits_per_pixel(4, 8), 16);
        assert_eq!(bits_per_pixel(4, 16), 32);
        assert_eq!(bits_per_pixel(6, 8), 32);
        assert_eq!(bits_per_pixel(6, 16), 64);
    }

    // ── Bulk: every non-corrupt fixture decodes without panic ────

    #[test]
    fn decode_all_valid_fixtures() {
        let valid = [
            "basn0g01.png",
            "basn0g02.png",
            "basn0g04.png",
            "basn0g08.png",
            "basn0g16.png",
            "basn2c08.png",
            "basn2c16.png",
            "basn3p01.png",
            "basn3p02.png",
            "basn3p04.png",
            "basn3p08.png",
            "basn4a08.png",
            "basn4a16.png",
            "basn6a08.png",
            "basn6a16.png",
            "basi0g01.png",
            "basi0g02.png",
            "basi0g04.png",
            "basi0g08.png",
            "basi0g16.png",
            "basi2c08.png",
            "basi2c16.png",
            "basi3p01.png",
            "basi3p02.png",
            "basi3p04.png",
            "basi3p08.png",
            "basi4a08.png",
            "basi4a16.png",
            "basi6a08.png",
            "basi6a16.png",
            "f00n0g08.png",
            "f00n2c08.png",
            "f01n0g08.png",
            "f01n2c08.png",
            "f02n0g08.png",
            "f02n2c08.png",
            "f03n0g08.png",
            "f03n2c08.png",
            "f04n0g08.png",
            "f04n2c08.png",
            "s01n3p01.png",
            "s02n3p01.png",
            "s03n3p01.png",
            "s04n3p01.png",
            "s05n3p02.png",
            "s06n3p02.png",
            "s07n3p02.png",
            "s08n3p02.png",
            "s09n3p02.png",
            "s32n3p04.png",
            "s33n3p04.png",
            "s34n3p04.png",
            "s35n3p04.png",
            "s36n3p04.png",
            "s37n3p04.png",
            "s38n3p04.png",
            "s39n3p04.png",
            "s40n3p04.png",
            "s01i3p01.png",
            "s02i3p01.png",
            "s03i3p01.png",
            "s04i3p01.png",
            "s05i3p02.png",
            "s06i3p02.png",
            "s07i3p02.png",
            "s08i3p02.png",
            "s09i3p02.png",
            "s32i3p04.png",
            "s33i3p04.png",
            "s34i3p04.png",
            "s35i3p04.png",
            "s36i3p04.png",
            "s37i3p04.png",
            "s38i3p04.png",
            "s39i3p04.png",
            "s40i3p04.png",
            "z00n2c08.png",
            "z03n2c08.png",
            "z06n2c08.png",
            "z09n2c08.png",
            "bgai4a08.png",
            "bgai4a16.png",
            "bgan6a08.png",
            "bgan6a16.png",
            "bgbn4a08.png",
            "bggn4a16.png",
            "bgwn6a08.png",
            "bgyn6a16.png",
            "tbbn0g04.png",
            "tbbn2c16.png",
            "tbbn3p08.png",
            "tbgn2c16.png",
            "tbgn3p08.png",
            "tbrn2c08.png",
            "tbwn0g16.png",
            "tbwn3p08.png",
            "tbyn3p08.png",
            "tp0n0g08.png",
            "tp0n2c08.png",
            "tp0n3p08.png",
            "tp1n3p08.png",
            "tm3n3p02.png",
            "g03n0g16.png",
            "g04n0g16.png",
            "g05n0g16.png",
            "g07n0g16.png",
            "g10n0g16.png",
            "g25n0g16.png",
            "g03n2c08.png",
            "g04n2c08.png",
            "g05n2c08.png",
            "g07n2c08.png",
            "g10n2c08.png",
            "g25n2c08.png",
            "g03n3p04.png",
            "g04n3p04.png",
            "g05n3p04.png",
            "g07n3p04.png",
            "g10n3p04.png",
            "g25n3p04.png",
            "ch1n3p04.png",
            "ch2n3p08.png",
            "cm0n0g04.png",
            "cm7n0g04.png",
            "cm9n0g04.png",
            "ct0n0g04.png",
            "ct1n0g04.png",
            "cten0g04.png",
            "ctfn0g04.png",
            "ctgn0g04.png",
            "cthn0g04.png",
            "ctjn0g04.png",
            "ctzn0g04.png",
            "pp0n2c16.png",
            "pp0n6a08.png",
            "ps1n0g08.png",
            "ps1n2c16.png",
            "ps2n0g08.png",
            "ps2n2c16.png",
            "ccwn2c08.png",
            "ccwn3p08.png",
            "cdfn2c08.png",
            "cdhn2c08.png",
            "cdsn2c08.png",
            "cdun2c08.png",
            "cs3n2c16.png",
            "cs3n3p08.png",
            "cs5n2c08.png",
            "cs5n3p08.png",
            "cs8n2c08.png",
            "cs8n3p08.png",
            "oi1n0g16.png",
            "oi1n2c16.png",
            "oi2n0g16.png",
            "oi2n2c16.png",
            "oi4n0g16.png",
            "oi4n2c16.png",
            "oi9n0g16.png",
            "oi9n2c16.png",
            "exif2c08.png",
            "f99n0g04.png",
        ];

        for name in valid {
            let data = fixture(name);
            let buf_size = match png_decode_buf_size(&data) {
                Ok(s) => s,
                Err(e) => panic!("{name}: buf_size failed: {e:?}"),
            };
            let mut output = vec![0u8; buf_size];

            if let Err(e) = png_decode(&data, &mut output) {
                panic!("{name}: decode failed: {e:?}");
            }
        }
    }
}
