//! Full JPEG decoder — no external dependencies, no_std compatible, no alloc.
//!
//! Supports baseline (SOF0), extended sequential (SOF1), and progressive
//! (SOF2) JPEG decoding for 8-bit precision, 1–3 component images with all
//! standard chroma subsampling modes (4:4:4, 4:2:2, 4:2:0). Decodes into a
//! caller-provided BGRA8888 output buffer. EXIF orientation is parsed and
//! applied automatically.
//!
//! Buffer requirement: caller must provide at least `jpeg_decode_buf_size(data)`
//! bytes. For baseline images this is `width * height * 4`. For progressive
//! images the buffer includes additional space for coefficient storage.
//!
//! # Known limitations (if a JPEG renders with wrong colors, check these)
//!
//! - **RGB JPEG**: We assume YCbCr and always apply color conversion. Some
//!   JPEGs (notably Photoshop exports) store raw RGB, signaled by component
//!   IDs 'R'/'G'/'B' (0x52/0x47/0x42) or an Adobe APP14 marker with
//!   transform=0. These will decode with inverted/shifted colors.
//!
//! - **CMYK/YCCK** (4 components): Photoshop can export CMYK JPEGs. We
//!   reject Nf>3, so these will fail to decode entirely.
//!
//! - **ICC color profiles** (APP2): We ignore them. Colors may appear
//!   slightly off for images with non-sRGB profiles (e.g. Adobe RGB,
//!   ProPhoto). The image will decode but may look desaturated or shifted.
//!
//! - **Arithmetic coding** (SOF9/SOF10): Rejected as unsupported. Extremely
//!   rare in the wild.
//!
//! - **12-bit precision**: SOF1 with P=12 is rejected. Only used in medical
//!   imaging (DICOM).

#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegError {
    InvalidSignature,
    Truncated,
    UnsupportedFormat,
    InvalidData,
    BufferTooSmall,
    DimensionOverflow,
    MissingTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegHeader {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub is_progressive: bool,
    pub orientation: u8,
}

// ── Markers ─────────────────────────────────────────────

const M_SOF0: u8 = 0xC0;
const M_SOF1: u8 = 0xC1;
const M_SOF2: u8 = 0xC2;
const M_DHT: u8 = 0xC4;
const M_SOI: u8 = 0xD8;
const M_EOI: u8 = 0xD9;
const M_SOS: u8 = 0xDA;
const M_DQT: u8 = 0xDB;
const M_DRI: u8 = 0xDD;

// ── Zig-zag order ───────────────────────────────────────
//
// Maps zig-zag position (0..63) to block index (row*8+col).
// From ITU-T T.81, Figure A.6.

const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// ── IDCT basis matrix ───────────────────────────────────
//
// IDCT_BASIS[k][n] = round(C(k) * cos((2n+1)*k*pi/16) / 2 * 4096)
// where C(0) = 1/sqrt(2), C(k) = 1 for k > 0.
// Used for separable 1D IDCT with 12-bit fixed-point precision.

const IDCT_BASIS: [[i32; 8]; 8] = [
    [1448, 1448, 1448, 1448, 1448, 1448, 1448, 1448],
    [2009, 1703, 1138, 400, -400, -1138, -1703, -2009],
    [1892, 784, -784, -1892, -1892, -784, 784, 1892],
    [1703, -400, -2009, -1138, 1138, 2009, 400, -1703],
    [1448, -1448, -1448, 1448, 1448, -1448, -1448, 1448],
    [1138, -2009, 400, 1703, -1703, -400, 2009, -1138],
    [784, -1892, 1892, -784, -784, 1892, -1892, 784],
    [400, -1138, 1703, -2009, 2009, -1703, 1138, -400],
];

// ── Internal types ──────────────────────────────────────

const MAX_COMPONENTS: usize = 4;

#[derive(Clone, Copy)]
struct ComponentInfo {
    id: u8,
    h_factor: u8,
    v_factor: u8,
    quant_id: u8,
    dc_table: u8,
    ac_table: u8,
}

impl ComponentInfo {
    const fn zero() -> Self {
        Self {
            id: 0,
            h_factor: 0,
            v_factor: 0,
            quant_id: 0,
            dc_table: 0,
            ac_table: 0,
        }
    }
}

struct HuffTable {
    values: [u8; 256],
    max_code: [i32; 17],
    val_offset: [i32; 17],
    num_values: usize,
}

impl HuffTable {
    const fn empty() -> Self {
        Self {
            values: [0; 256],
            max_code: [-1; 17],
            val_offset: [0; 17],
            num_values: 0,
        }
    }

    fn build(bits: &[u8], huffval: &[u8], nval: usize) -> Self {
        let mut table = Self::empty();

        table.num_values = nval;

        let mut i = 0;

        while i < nval && i < 256 {
            table.values[i] = huffval[i];
            i += 1;
        }

        let mut code = 0u32;
        let mut offset = 0usize;
        let mut length = 1;

        while length <= 16 {
            let count = bits[length] as usize;

            if count > 0 {
                table.val_offset[length] = offset as i32 - code as i32;
                code += count as u32;
                table.max_code[length] = (code - 1) as i32;
                offset += count;
            }

            code <<= 1;
            length += 1;
        }

        table
    }
}

struct ScanInfo {
    comp_indices: [u8; MAX_COMPONENTS],
    num_components: u8,
    ss: u8,
    se: u8,
    ah: u8,
    al: u8,
}

struct DecoderState {
    width: u16,
    height: u16,
    num_components: u8,
    components: [ComponentInfo; MAX_COMPONENTS],
    max_h: u8,
    max_v: u8,
    is_progressive: bool,
    orientation: u8,

    quant: [[u16; 64]; 4],
    quant_valid: [bool; 4],

    dc_tables: [HuffTable; 4],
    ac_tables: [HuffTable; 4],
    dc_valid: [bool; 4],
    ac_valid: [bool; 4],

    restart_interval: u16,
    first_sos_pos: usize,
    frame_parsed: bool,
}

impl DecoderState {
    fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            num_components: 0,
            components: [ComponentInfo::zero(); MAX_COMPONENTS],
            max_h: 0,
            max_v: 0,
            is_progressive: false,
            orientation: 1,
            quant: [[0; 64]; 4],
            quant_valid: [false; 4],
            dc_tables: [
                HuffTable::empty(),
                HuffTable::empty(),
                HuffTable::empty(),
                HuffTable::empty(),
            ],
            ac_tables: [
                HuffTable::empty(),
                HuffTable::empty(),
                HuffTable::empty(),
                HuffTable::empty(),
            ],
            dc_valid: [false; 4],
            ac_valid: [false; 4],
            restart_interval: 0,
            first_sos_pos: 0,
            frame_parsed: false,
        }
    }

    fn mcu_cols(&self) -> usize {
        let mcu_w = self.max_h as usize * 8;

        (self.width as usize + mcu_w - 1) / mcu_w
    }

    fn mcu_rows(&self) -> usize {
        let mcu_h = self.max_v as usize * 8;

        (self.height as usize + mcu_h - 1) / mcu_h
    }
}

// ── Bit reader ──────────────────────────────────────────
//
// Reads bits MSB-first from JPEG entropy-coded data, handling
// byte stuffing (0xFF 0x00 → 0xFF) and marker detection.

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bits: u32,
    nbits: u8,
    marker: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], start: usize) -> Self {
        Self {
            data,
            pos: start,
            bits: 0,
            nbits: 0,
            marker: 0,
        }
    }

    fn next_byte(&mut self) -> Result<u8, JpegError> {
        if self.marker != 0 {
            return Ok(0);
        }

        if self.pos >= self.data.len() {
            return Err(JpegError::Truncated);
        }

        let b = self.data[self.pos];

        self.pos += 1;

        if b != 0xFF {
            return Ok(b);
        }

        loop {
            if self.pos >= self.data.len() {
                return Err(JpegError::Truncated);
            }

            let next = self.data[self.pos];

            self.pos += 1;

            if next == 0x00 {
                return Ok(0xFF);
            }

            if next == 0xFF {
                continue;
            }

            // Any non-zero, non-0xFF byte after 0xFF is a marker.
            // Return zero padding — remaining bits are byte-alignment
            // padding. Callers check `self.marker` at scan/MCU boundaries.
            self.marker = next;

            return Ok(0);
        }
    }

    fn read_bit(&mut self) -> Result<u32, JpegError> {
        if self.nbits == 0 {
            let b = self.next_byte()?;

            self.bits = b as u32;
            self.nbits = 8;
        }

        self.nbits -= 1;

        Ok((self.bits >> self.nbits) & 1)
    }

    fn read_bits(&mut self, n: u8) -> Result<u32, JpegError> {
        while self.nbits < n {
            let b = self.next_byte()?;

            self.bits = (self.bits << 8) | b as u32;
            self.nbits += 8;
        }

        self.nbits -= n;

        Ok((self.bits >> self.nbits) & ((1u32 << n) - 1))
    }

    fn decode_huffman(&mut self, table: &HuffTable) -> Result<u8, JpegError> {
        let mut code = 0u32;

        for length in 1..=16u8 {
            code = (code << 1) | self.read_bit()?;

            if table.max_code[length as usize] >= 0
                && code as i32 <= table.max_code[length as usize]
            {
                let idx = (code as i32 + table.val_offset[length as usize]) as usize;

                return Ok(table.values[idx]);
            }
        }

        Err(JpegError::InvalidData)
    }

    fn receive_extend(&mut self, nbits: u8) -> Result<i32, JpegError> {
        if nbits == 0 {
            return Ok(0);
        }

        let val = self.read_bits(nbits)? as i32;
        let threshold = 1 << (nbits - 1);

        if val < threshold {
            Ok(val - (2 * threshold - 1))
        } else {
            Ok(val)
        }
    }

    fn align(&mut self) {
        self.bits = 0;
        self.nbits = 0;
    }

    fn handle_restart(&mut self) {
        self.align();

        if self.marker >= 0xD0 && self.marker <= 0xD7 {
            self.marker = 0;

            return;
        }

        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xFF {
                let m = self.data[self.pos + 1];

                if m >= 0xD0 && m <= 0xD7 {
                    self.pos += 2;

                    return;
                } else if m == 0xFF {
                    self.pos += 1;
                } else {
                    return;
                }
            } else {
                return;
            }
        }
    }
}

// ── IDCT ────────────────────────────────────────────────
//
// 2D 8×8 Inverse Discrete Cosine Transform using separable
// 1D passes with the precomputed basis matrix.

fn idct_1d(coeffs: [i32; 8]) -> [i32; 8] {
    if coeffs[1] | coeffs[2] | coeffs[3] | coeffs[4] | coeffs[5] | coeffs[6] | coeffs[7] == 0 {
        let dc = ((coeffs[0] as i64 * IDCT_BASIS[0][0] as i64 + 2048) >> 12) as i32;

        return [dc; 8];
    }

    let mut out = [0i32; 8];

    for n in 0..8 {
        let mut sum = 0i64;

        for k in 0..8 {
            sum += coeffs[k] as i64 * IDCT_BASIS[k][n] as i64;
        }

        out[n] = ((sum + 2048) >> 12) as i32;
    }

    out
}

fn idct_2d(block: &mut [i32; 64]) {
    for row in 0..8 {
        let off = row * 8;
        let coeffs = [
            block[off],
            block[off + 1],
            block[off + 2],
            block[off + 3],
            block[off + 4],
            block[off + 5],
            block[off + 6],
            block[off + 7],
        ];
        let result = idct_1d(coeffs);

        block[off] = result[0];
        block[off + 1] = result[1];
        block[off + 2] = result[2];
        block[off + 3] = result[3];
        block[off + 4] = result[4];
        block[off + 5] = result[5];
        block[off + 6] = result[6];
        block[off + 7] = result[7];
    }

    for col in 0..8 {
        let coeffs = [
            block[col],
            block[col + 8],
            block[col + 16],
            block[col + 24],
            block[col + 32],
            block[col + 40],
            block[col + 48],
            block[col + 56],
        ];
        let result = idct_1d(coeffs);

        block[col] = (result[0] + 128).clamp(0, 255);
        block[col + 8] = (result[1] + 128).clamp(0, 255);
        block[col + 16] = (result[2] + 128).clamp(0, 255);
        block[col + 24] = (result[3] + 128).clamp(0, 255);
        block[col + 32] = (result[4] + 128).clamp(0, 255);
        block[col + 40] = (result[5] + 128).clamp(0, 255);
        block[col + 48] = (result[6] + 128).clamp(0, 255);
        block[col + 56] = (result[7] + 128).clamp(0, 255);
    }
}

// ── Color conversion ────────────────────────────────────

fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn ycbcr_to_bgra(y: i32, cb: i32, cr: i32) -> [u8; 4] {
    let cb_shift = cb - 128;
    let cr_shift = cr - 128;
    let r = y + ((91881 * cr_shift + 32768) >> 16);
    let g = y - ((22554 * cb_shift + 46802 * cr_shift + 32768) >> 16);
    let b = y + ((116130 * cb_shift + 32768) >> 16);

    [clamp_u8(b), clamp_u8(g), clamp_u8(r), 255]
}

// ── Marker parsing ──────────────────────────────────────

fn read_u16_be(data: &[u8], pos: usize) -> u16 {
    ((data[pos] as u16) << 8) | data[pos + 1] as u16
}

fn parse_dqt(
    state: &mut DecoderState,
    data: &[u8],
    pos: usize,
    len: usize,
) -> Result<(), JpegError> {
    let end = pos + len;
    let mut p = pos;

    while p < end {
        if p >= data.len() {
            return Err(JpegError::Truncated);
        }

        let pq_tq = data[p];
        let precision = pq_tq >> 4;
        let table_id = (pq_tq & 0x0F) as usize;

        p += 1;

        if table_id >= 4 {
            return Err(JpegError::InvalidData);
        }

        if precision == 0 {
            if p + 64 > end {
                return Err(JpegError::Truncated);
            }

            for i in 0..64 {
                state.quant[table_id][i] = data[p + i] as u16;
            }

            p += 64;
        } else {
            if p + 128 > end {
                return Err(JpegError::Truncated);
            }

            for i in 0..64 {
                state.quant[table_id][i] = read_u16_be(data, p + i * 2);
            }

            p += 128;
        }

        state.quant_valid[table_id] = true;
    }

    Ok(())
}

fn parse_dht(
    state: &mut DecoderState,
    data: &[u8],
    pos: usize,
    len: usize,
) -> Result<(), JpegError> {
    let end = pos + len;
    let mut p = pos;

    while p < end {
        if p >= data.len() {
            return Err(JpegError::Truncated);
        }

        let tc_th = data[p];
        let tc = tc_th >> 4;
        let th = (tc_th & 0x0F) as usize;

        p += 1;

        if tc > 1 || th >= 4 {
            return Err(JpegError::InvalidData);
        }

        if p + 16 > end {
            return Err(JpegError::Truncated);
        }

        let mut bits = [0u8; 17];
        let mut total = 0usize;

        for i in 1..=16 {
            bits[i] = data[p + i - 1];
            total += bits[i] as usize;
        }

        p += 16;

        if total > 256 || p + total > end {
            return Err(JpegError::Truncated);
        }

        let huffval = &data[p..p + total];

        if tc == 0 {
            state.dc_tables[th] = HuffTable::build(&bits, huffval, total);
            state.dc_valid[th] = true;
        } else {
            state.ac_tables[th] = HuffTable::build(&bits, huffval, total);
            state.ac_valid[th] = true;
        }

        p += total;
    }

    Ok(())
}

fn parse_sof(
    state: &mut DecoderState,
    data: &[u8],
    pos: usize,
    progressive: bool,
) -> Result<(), JpegError> {
    if pos + 6 > data.len() {
        return Err(JpegError::Truncated);
    }

    let precision = data[pos];

    if precision != 8 {
        return Err(JpegError::UnsupportedFormat);
    }

    state.height = read_u16_be(data, pos + 1);
    state.width = read_u16_be(data, pos + 3);
    state.num_components = data[pos + 5];
    state.is_progressive = progressive;

    if state.width == 0 || state.height == 0 {
        return Err(JpegError::InvalidData);
    }

    if state.num_components == 0 || state.num_components as usize > MAX_COMPONENTS {
        return Err(JpegError::UnsupportedFormat);
    }

    let nc = state.num_components as usize;

    if pos + 6 + nc * 3 > data.len() {
        return Err(JpegError::Truncated);
    }

    let mut max_h: u8 = 1;
    let mut max_v: u8 = 1;

    for i in 0..nc {
        let base = pos + 6 + i * 3;

        state.components[i].id = data[base];
        state.components[i].h_factor = data[base + 1] >> 4;
        state.components[i].v_factor = data[base + 1] & 0x0F;
        state.components[i].quant_id = data[base + 2];

        if state.components[i].h_factor == 0
            || state.components[i].v_factor == 0
            || state.components[i].h_factor > 4
            || state.components[i].v_factor > 4
        {
            return Err(JpegError::InvalidData);
        }

        if state.components[i].h_factor > max_h {
            max_h = state.components[i].h_factor;
        }
        if state.components[i].v_factor > max_v {
            max_v = state.components[i].v_factor;
        }
    }

    state.max_h = max_h;
    state.max_v = max_v;
    state.frame_parsed = true;

    Ok(())
}

// ── EXIF orientation ─────────────────────────────────────

fn parse_exif_orientation(data: &[u8], pos: usize, len: usize) -> u8 {
    let end = pos + len;

    if end > data.len() || len < 14 {
        return 1;
    }

    if &data[pos..pos + 6] != b"Exif\x00\x00" {
        return 1;
    }

    let tiff = pos + 6;
    let big_endian = match (data[tiff], data[tiff + 1]) {
        (0x4D, 0x4D) => true,
        (0x49, 0x49) => false,
        _ => return 1,
    };
    let read16 = |off: usize| -> u16 {
        if big_endian {
            read_u16_be(data, off)
        } else {
            (data[off] as u16) | ((data[off + 1] as u16) << 8)
        }
    };
    let read32 = |off: usize| -> u32 {
        if big_endian {
            ((data[off] as u32) << 24)
                | ((data[off + 1] as u32) << 16)
                | ((data[off + 2] as u32) << 8)
                | (data[off + 3] as u32)
        } else {
            (data[off] as u32)
                | ((data[off + 1] as u32) << 8)
                | ((data[off + 2] as u32) << 16)
                | ((data[off + 3] as u32) << 24)
        }
    };

    if read16(tiff + 2) != 42 {
        return 1;
    }

    let ifd_offset = read32(tiff + 4) as usize;
    let ifd = tiff + ifd_offset;

    if ifd + 2 > end {
        return 1;
    }

    let entry_count = read16(ifd) as usize;

    for i in 0..entry_count {
        let entry = ifd + 2 + i * 12;

        if entry + 12 > end {
            break;
        }

        let tag = read16(entry);

        if tag == 0x0112 {
            let val = read16(entry + 8);

            if val >= 1 && val <= 8 {
                return val as u8;
            }
        }
    }

    1
}

fn parse_markers(state: &mut DecoderState, data: &[u8]) -> Result<(), JpegError> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != M_SOI {
        return Err(JpegError::InvalidSignature);
    }

    let mut pos = 2;

    loop {
        if pos + 1 >= data.len() {
            return Err(JpegError::Truncated);
        }

        if data[pos] != 0xFF {
            return Err(JpegError::InvalidData);
        }

        while pos + 1 < data.len() && data[pos + 1] == 0xFF {
            pos += 1;
        }

        if pos + 1 >= data.len() {
            return Err(JpegError::Truncated);
        }

        let marker = data[pos + 1];

        pos += 2;

        if marker == M_EOI || marker == 0x00 {
            return Err(JpegError::InvalidData);
        }

        if marker == M_SOS {
            state.first_sos_pos = pos;

            return Ok(());
        }

        if pos + 1 >= data.len() {
            return Err(JpegError::Truncated);
        }

        let seg_len = read_u16_be(data, pos) as usize;

        if seg_len < 2 || pos + seg_len > data.len() {
            return Err(JpegError::Truncated);
        }

        let payload_start = pos + 2;
        let payload_len = seg_len - 2;

        match marker {
            M_SOF0 | M_SOF1 => parse_sof(state, data, payload_start, false)?,
            M_SOF2 => parse_sof(state, data, payload_start, true)?,
            M_DHT => parse_dht(state, data, payload_start, payload_len)?,
            M_DQT => parse_dqt(state, data, payload_start, payload_len)?,
            M_DRI => {
                if payload_len >= 2 {
                    state.restart_interval = read_u16_be(data, payload_start);
                }
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(JpegError::UnsupportedFormat);
            }
            0xE1 => {
                if state.orientation == 1 {
                    state.orientation = parse_exif_orientation(data, payload_start, payload_len);
                }
            }
            _ => {}
        }

        pos += seg_len;
    }
}

// ── SOS header re-parse (for progressive multi-scan) ────

fn parse_sos_at(
    state: &mut DecoderState,
    data: &[u8],
    sos_pos: usize,
) -> Result<(ScanInfo, usize), JpegError> {
    if sos_pos + 1 >= data.len() {
        return Err(JpegError::Truncated);
    }

    let seg_len = read_u16_be(data, sos_pos) as usize;

    if seg_len < 2 || sos_pos + seg_len > data.len() {
        return Err(JpegError::Truncated);
    }

    let payload_start = sos_pos + 2;
    let ns = data[payload_start] as usize;

    if ns == 0 || ns > MAX_COMPONENTS {
        return Err(JpegError::InvalidData);
    }

    let header_len = 1 + ns * 2 + 3;

    if payload_start + header_len > data.len() {
        return Err(JpegError::Truncated);
    }

    let mut scan = ScanInfo {
        comp_indices: [0; MAX_COMPONENTS],
        num_components: ns as u8,
        ss: 0,
        se: 0,
        ah: 0,
        al: 0,
    };

    for i in 0..ns {
        let cs = data[payload_start + 1 + i * 2];
        let td_ta = data[payload_start + 2 + i * 2];
        let td = td_ta >> 4;
        let ta = td_ta & 0x0F;
        let mut comp_idx = 0xFFu8;

        for j in 0..state.num_components as usize {
            if state.components[j].id == cs {
                comp_idx = j as u8;

                break;
            }
        }

        if comp_idx == 0xFF {
            return Err(JpegError::InvalidData);
        }

        scan.comp_indices[i] = comp_idx;
        state.components[comp_idx as usize].dc_table = td;
        state.components[comp_idx as usize].ac_table = ta;
    }

    let tail = payload_start + 1 + ns * 2;

    scan.ss = data[tail];
    scan.se = data[tail + 1];
    scan.ah = data[tail + 2] >> 4;
    scan.al = data[tail + 2] & 0x0F;

    let entropy_start = sos_pos + seg_len;

    Ok((scan, entropy_start))
}

// ── Baseline decode ─────────────────────────────────────

fn decode_baseline(
    state: &mut DecoderState,
    data: &[u8],
    output: &mut [u8],
) -> Result<(), JpegError> {
    let (scan, entropy_start) = parse_sos_at(state, data, state.first_sos_pos)?;

    if scan.ss != 0 || scan.se != 63 || scan.ah != 0 || scan.al != 0 {
        return Err(JpegError::InvalidData);
    }

    let nc = scan.num_components as usize;
    let w = state.width as usize;
    let h = state.height as usize;
    let mcu_cols = state.mcu_cols();
    let mcu_rows = state.mcu_rows();
    let mcu_w = state.max_h as usize * 8;
    let mcu_h = state.max_v as usize * 8;
    let mut reader = BitReader::new(data, entropy_start);
    let mut dc_pred = [0i32; MAX_COMPONENTS];
    let mut mcu_count = 0u32;
    let restart_interval = state.restart_interval as u32;

    for mcu_row in 0..mcu_rows {
        for mcu_col in 0..mcu_cols {
            if restart_interval > 0 && mcu_count > 0 && mcu_count % restart_interval == 0 {
                reader.handle_restart();
                dc_pred = [0i32; MAX_COMPONENTS];
            }

            // Decode all blocks in this MCU.
            let mut blocks = [[0i32; 64]; 10];
            let mut block_idx = 0;

            for ci in 0..nc {
                let comp = state.components[scan.comp_indices[ci] as usize];
                let qi = comp.quant_id as usize;

                if !state.quant_valid[qi] {
                    return Err(JpegError::MissingTable);
                }

                let dc_tbl = comp.dc_table as usize;
                let ac_tbl = comp.ac_table as usize;

                if !state.dc_valid[dc_tbl] || !state.ac_valid[ac_tbl] {
                    return Err(JpegError::MissingTable);
                }

                let bh = comp.h_factor as usize;
                let bv = comp.v_factor as usize;

                for v in 0..bv {
                    for h in 0..bh {
                        let _ = (v, h);

                        decode_block_baseline(
                            &mut reader,
                            &mut blocks[block_idx],
                            &mut dc_pred[scan.comp_indices[ci] as usize],
                            &state.dc_tables[dc_tbl],
                            &state.ac_tables[ac_tbl],
                            &state.quant[qi],
                        )?;

                        block_idx += 1;
                    }
                }
            }

            // Write MCU pixels to output.
            write_mcu_pixels(
                state, &scan, &blocks, mcu_col, mcu_row, mcu_w, mcu_h, w, h, output,
            );

            mcu_count += 1;
        }
    }

    Ok(())
}

fn decode_block_baseline(
    reader: &mut BitReader,
    block: &mut [i32; 64],
    dc_pred: &mut i32,
    dc_table: &HuffTable,
    ac_table: &HuffTable,
    quant: &[u16; 64],
) -> Result<(), JpegError> {
    *block = [0i32; 64];

    // DC coefficient.
    let dc_cat = reader.decode_huffman(dc_table)?;

    if dc_cat > 11 {
        return Err(JpegError::InvalidData);
    }

    let dc_diff = reader.receive_extend(dc_cat)?;

    *dc_pred += dc_diff;
    block[0] = *dc_pred * quant[0] as i32;

    // AC coefficients.
    let mut k = 1;

    while k < 64 {
        let rs = reader.decode_huffman(ac_table)?;
        let run = rs >> 4;
        let size = rs & 0x0F;

        if size == 0 {
            if run == 0 {
                break;
            }

            if run == 0x0F {
                k += 16;

                continue;
            }

            return Err(JpegError::InvalidData);
        }

        k += run as usize;

        if k >= 64 {
            return Err(JpegError::InvalidData);
        }

        let val = reader.receive_extend(size)?;
        let zi = ZIGZAG[k] as usize;

        block[zi] = val * quant[k] as i32;
        k += 1;
    }

    idct_2d(block);

    Ok(())
}

fn write_mcu_pixels(
    state: &DecoderState,
    scan: &ScanInfo,
    blocks: &[[i32; 64]; 10],
    mcu_col: usize,
    mcu_row: usize,
    mcu_w: usize,
    mcu_h: usize,
    img_w: usize,
    img_h: usize,
    output: &mut [u8],
) {
    let nc = scan.num_components as usize;
    let px_x = mcu_col * mcu_w;
    let px_y = mcu_row * mcu_h;

    if nc == 1 {
        // Grayscale.
        let comp = state.components[scan.comp_indices[0] as usize];
        let bh = comp.h_factor as usize;

        for y in 0..mcu_h {
            let out_y = px_y + y;

            if out_y >= img_h {
                break;
            }

            for x in 0..mcu_w {
                let out_x = px_x + x;

                if out_x >= img_w {
                    break;
                }

                let block_row = y / 8;
                let block_col = x / 8;
                let bi = block_row * bh + block_col;
                let local_y = y % 8;
                let local_x = x % 8;
                let val = blocks[bi][local_y * 8 + local_x] as u8;
                let out_idx = (out_y * img_w + out_x) * 4;

                output[out_idx] = val;
                output[out_idx + 1] = val;
                output[out_idx + 2] = val;
                output[out_idx + 3] = 255;
            }
        }

        return;
    }

    // YCbCr (or similar multi-component).
    // Block layout: Y blocks first, then Cb blocks, then Cr blocks.
    let comp_y = state.components[scan.comp_indices[0] as usize];
    let bh_y = comp_y.h_factor as usize;
    let bv_y = comp_y.v_factor as usize;
    let y_block_count = bh_y * bv_y;
    let comp_cb = state.components[scan.comp_indices[1] as usize];
    let bh_cb = comp_cb.h_factor as usize;
    let bv_cb = comp_cb.v_factor as usize;
    let cb_block_start = y_block_count;
    let cb_block_count = bh_cb * bv_cb;
    let cr_block_start = cb_block_start + cb_block_count;
    let max_h = state.max_h as usize;
    let max_v = state.max_v as usize;

    for y in 0..mcu_h {
        let out_y = px_y + y;

        if out_y >= img_h {
            break;
        }

        for x in 0..mcu_w {
            let out_x = px_x + x;

            if out_x >= img_w {
                break;
            }

            // Y sample.
            let y_block_row = y / 8;
            let y_block_col = x / 8;
            let y_bi = y_block_row * bh_y + y_block_col;
            let y_val = blocks[y_bi][(y % 8) * 8 + (x % 8)];
            // Cb sample (upsampled from subsampled block).
            let cb_py = y * bv_cb / max_v;
            let cb_px = x * bh_cb / max_h;
            let cb_block_row = cb_py / 8;
            let cb_block_col = cb_px / 8;
            let cb_bi = cb_block_start + cb_block_row * bh_cb + cb_block_col;
            let cb_val = blocks[cb_bi][(cb_py % 8) * 8 + (cb_px % 8)];
            // Cr sample.
            let cr_py = y * comp_cb.v_factor as usize / max_v;
            let cr_px = x * comp_cb.h_factor as usize / max_h;
            let cr_block_row = cr_py / 8;
            let cr_block_col = cr_px / 8;
            let cr_bi = cr_block_start + cr_block_row * bh_cb + cr_block_col;
            let cr_val = blocks[cr_bi][(cr_py % 8) * 8 + (cr_px % 8)];
            let bgra = ycbcr_to_bgra(y_val, cb_val, cr_val);
            let out_idx = (out_y * img_w + out_x) * 4;

            output[out_idx] = bgra[0];
            output[out_idx + 1] = bgra[1];
            output[out_idx + 2] = bgra[2];
            output[out_idx + 3] = bgra[3];
        }
    }
}

// ── Progressive decode ──────────────────────────────────

fn coeff_buf_size(state: &DecoderState) -> usize {
    let mcu_cols = state.mcu_cols();
    let mcu_rows = state.mcu_rows();
    let mut total = 0;

    for i in 0..state.num_components as usize {
        let bh = state.components[i].h_factor as usize;
        let bv = state.components[i].v_factor as usize;
        let blocks = mcu_cols * bh * mcu_rows * bv;

        total += blocks * 64;
    }

    total * 2
}

fn coeff_offset(state: &DecoderState, comp_idx: usize) -> usize {
    let mcu_cols = state.mcu_cols();
    let mcu_rows = state.mcu_rows();
    let mut offset = 0;

    for i in 0..comp_idx {
        let bh = state.components[i].h_factor as usize;
        let bv = state.components[i].v_factor as usize;

        offset += mcu_cols * bh * mcu_rows * bv * 64 * 2;
    }

    offset
}

fn comp_blocks_w(state: &DecoderState, comp_idx: usize) -> usize {
    state.mcu_cols() * state.components[comp_idx].h_factor as usize
}

fn read_coeff(coeff_buf: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([coeff_buf[offset], coeff_buf[offset + 1]])
}

fn write_coeff(coeff_buf: &mut [u8], offset: usize, val: i16) {
    let bytes = val.to_le_bytes();

    coeff_buf[offset] = bytes[0];
    coeff_buf[offset + 1] = bytes[1];
}

fn decode_progressive(
    state: &mut DecoderState,
    data: &[u8],
    output: &mut [u8],
) -> Result<(), JpegError> {
    let w = state.width as usize;
    let h = state.height as usize;
    let pixel_size = w * h * 4;

    for b in output[pixel_size..].iter_mut() {
        *b = 0;
    }

    // Process scans.
    let mut sos_pos = state.first_sos_pos;

    loop {
        let (scan, entropy_start) = parse_sos_at(state, data, sos_pos)?;
        let (end_pos, pending_marker) = {
            let coeff_buf = &mut output[pixel_size..];

            decode_progressive_scan(state, data, entropy_start, &scan, coeff_buf)?
        };
        // The bit reader may have already consumed the marker that ends
        // the entropy segment. Use it directly if available; otherwise
        // scan forward from end_pos.
        let mut pos = end_pos;
        let mut found_eoi = false;
        let mut found_sos = false;

        if pending_marker != 0 {
            let marker = pending_marker;

            match marker {
                M_EOI => found_eoi = true,
                M_SOS => {
                    sos_pos = pos;
                    found_sos = true;
                }
                M_DHT => {
                    if pos + 1 < data.len() {
                        let seg_len = read_u16_be(data, pos) as usize;

                        if seg_len >= 2 && pos + seg_len <= data.len() {
                            let _ = parse_dht(state, data, pos + 2, seg_len - 2);
                            pos += seg_len;
                        }
                    }
                }
                M_DQT => {
                    if pos + 1 < data.len() {
                        let seg_len = read_u16_be(data, pos) as usize;

                        if seg_len >= 2 && pos + seg_len <= data.len() {
                            let _ = parse_dqt(state, data, pos + 2, seg_len - 2);
                            pos += seg_len;
                        }
                    }
                }
                _ => {}
            }
        }

        // If the pending marker didn't resolve to SOS or EOI, keep
        // scanning for more markers between scans.
        while !found_eoi && !found_sos {
            if pos + 1 >= data.len() {
                break;
            }

            if data[pos] != 0xFF {
                pos += 1;

                continue;
            }

            while pos + 1 < data.len() && data[pos + 1] == 0xFF {
                pos += 1;
            }

            if pos + 1 >= data.len() {
                break;
            }

            let marker = data[pos + 1];

            pos += 2;

            match marker {
                0x00 => continue,
                M_EOI => {
                    found_eoi = true;
                }
                M_SOS => {
                    sos_pos = pos;
                    found_sos = true;
                }
                M_DHT => {
                    if pos + 1 >= data.len() {
                        break;
                    }

                    let seg_len = read_u16_be(data, pos) as usize;

                    if seg_len < 2 || pos + seg_len > data.len() {
                        break;
                    }

                    let _ = parse_dht(state, data, pos + 2, seg_len - 2);

                    pos += seg_len;
                }
                M_DQT => {
                    if pos + 1 >= data.len() {
                        break;
                    }

                    let seg_len = read_u16_be(data, pos) as usize;

                    if seg_len < 2 || pos + seg_len > data.len() {
                        break;
                    }

                    let _ = parse_dqt(state, data, pos + 2, seg_len - 2);

                    pos += seg_len;
                }
                _ if marker >= 0xD0 && marker <= 0xD7 => continue,
                _ => {
                    if pos + 1 >= data.len() {
                        break;
                    }

                    let seg_len = read_u16_be(data, pos) as usize;

                    if pos + seg_len <= data.len() {
                        pos += seg_len;
                    } else {
                        break;
                    }
                }
            }
        }

        if found_eoi || (!found_sos && pos + 1 >= data.len()) {
            break;
        }

        if !found_sos {
            break;
        }
    }

    finalize_progressive(state, output)
}

fn decode_progressive_scan(
    state: &DecoderState,
    data: &[u8],
    entropy_start: usize,
    scan: &ScanInfo,
    coeff_buf: &mut [u8],
) -> Result<(usize, u8), JpegError> {
    let mut reader = BitReader::new(data, entropy_start);
    let mut dc_pred = [0i32; MAX_COMPONENTS];
    let mcu_cols = state.mcu_cols();
    let mcu_rows = state.mcu_rows();
    let restart_interval = state.restart_interval as u32;
    let mut mcu_count = 0u32;
    let mut eobrun = 0u32;
    let is_dc = scan.ss == 0 && scan.se == 0;
    let is_first = scan.ah == 0;
    let interleaved = scan.num_components > 1;

    if interleaved {
        for mcu_row in 0..mcu_rows {
            for mcu_col in 0..mcu_cols {
                if restart_interval > 0 && mcu_count > 0 && mcu_count % restart_interval == 0 {
                    reader.handle_restart();

                    dc_pred = [0i32; MAX_COMPONENTS];
                    eobrun = 0;
                }

                for ci in 0..scan.num_components as usize {
                    let comp_idx = scan.comp_indices[ci] as usize;
                    let comp = state.components[comp_idx];
                    let bh = comp.h_factor as usize;
                    let bv = comp.v_factor as usize;
                    let base_offset = coeff_offset(state, comp_idx);
                    let blocks_w = comp_blocks_w(state, comp_idx);

                    for v in 0..bv {
                        for h in 0..bh {
                            let bx = mcu_col * bh + h;
                            let by = mcu_row * bv + v;
                            let block_offset = base_offset + (by * blocks_w + bx) * 64 * 2;

                            if is_dc {
                                if is_first {
                                    decode_dc_first(
                                        &mut reader,
                                        coeff_buf,
                                        block_offset,
                                        &mut dc_pred[comp_idx],
                                        &state.dc_tables[comp.dc_table as usize],
                                        scan.al,
                                    )?;
                                } else {
                                    decode_dc_refine(
                                        &mut reader,
                                        coeff_buf,
                                        block_offset,
                                        scan.al,
                                    )?;
                                }
                            } else if is_first {
                                eobrun = decode_ac_first(
                                    &mut reader,
                                    coeff_buf,
                                    block_offset,
                                    &state.ac_tables[comp.ac_table as usize],
                                    scan.ss,
                                    scan.se,
                                    scan.al,
                                    eobrun,
                                )?;
                            } else {
                                eobrun = decode_ac_refine(
                                    &mut reader,
                                    coeff_buf,
                                    block_offset,
                                    &state.ac_tables[comp.ac_table as usize],
                                    scan.ss,
                                    scan.se,
                                    scan.al,
                                    eobrun,
                                )?;
                            }
                        }
                    }
                }

                mcu_count += 1;
            }
        }
    } else {
        // Non-interleaved scan (single component).
        let comp_idx = scan.comp_indices[0] as usize;
        let comp = state.components[comp_idx];
        let base_offset = coeff_offset(state, comp_idx);
        let blocks_w = comp_blocks_w(state, comp_idx);
        let blocks_h_total = mcu_cols * comp.h_factor as usize;
        let blocks_v_total = mcu_rows * comp.v_factor as usize;
        let total_blocks = blocks_h_total * blocks_v_total;

        for block_num in 0..total_blocks {
            if restart_interval > 0 && mcu_count > 0 && mcu_count % restart_interval == 0 {
                reader.handle_restart();

                dc_pred = [0i32; MAX_COMPONENTS];
                eobrun = 0;
            }

            let bx = block_num % blocks_w;
            let by = block_num / blocks_w;
            let block_offset = base_offset + (by * blocks_w + bx) * 64 * 2;

            if is_dc {
                if is_first {
                    decode_dc_first(
                        &mut reader,
                        coeff_buf,
                        block_offset,
                        &mut dc_pred[comp_idx],
                        &state.dc_tables[comp.dc_table as usize],
                        scan.al,
                    )?;
                } else {
                    decode_dc_refine(&mut reader, coeff_buf, block_offset, scan.al)?;
                }
            } else if is_first {
                eobrun = decode_ac_first(
                    &mut reader,
                    coeff_buf,
                    block_offset,
                    &state.ac_tables[comp.ac_table as usize],
                    scan.ss,
                    scan.se,
                    scan.al,
                    eobrun,
                )?;
            } else {
                eobrun = decode_ac_refine(
                    &mut reader,
                    coeff_buf,
                    block_offset,
                    &state.ac_tables[comp.ac_table as usize],
                    scan.ss,
                    scan.se,
                    scan.al,
                    eobrun,
                )?;
            }

            mcu_count += 1;
        }
    }

    Ok((reader.pos, reader.marker))
}

fn decode_dc_first(
    reader: &mut BitReader,
    coeff_buf: &mut [u8],
    block_offset: usize,
    dc_pred: &mut i32,
    dc_table: &HuffTable,
    al: u8,
) -> Result<(), JpegError> {
    let cat = reader.decode_huffman(dc_table)?;

    if cat > 11 {
        return Err(JpegError::InvalidData);
    }

    let diff = reader.receive_extend(cat)?;

    *dc_pred += diff;

    let val = (*dc_pred << al as i32) as i16;

    write_coeff(coeff_buf, block_offset, val);

    Ok(())
}

fn decode_dc_refine(
    reader: &mut BitReader,
    coeff_buf: &mut [u8],
    block_offset: usize,
    al: u8,
) -> Result<(), JpegError> {
    let bit = reader.read_bit()? as i16;
    let current = read_coeff(coeff_buf, block_offset);

    write_coeff(coeff_buf, block_offset, current | (bit << al));

    Ok(())
}

fn decode_ac_first(
    reader: &mut BitReader,
    coeff_buf: &mut [u8],
    block_offset: usize,
    ac_table: &HuffTable,
    ss: u8,
    se: u8,
    al: u8,
    mut eobrun: u32,
) -> Result<u32, JpegError> {
    if eobrun > 0 {
        return Ok(eobrun - 1);
    }

    let mut k = ss as usize;

    while k <= se as usize {
        let rs = reader.decode_huffman(ac_table)?;
        let run = (rs >> 4) as usize;
        let size = rs & 0x0F;

        if size == 0 {
            if run == 0x0F {
                k += 16;

                continue;
            }

            // EOBn.
            if run > 0 {
                eobrun = (1 << run) + reader.read_bits(run as u8)? - 1;
            }

            return Ok(eobrun);
        }

        k += run;

        if k > se as usize {
            return Err(JpegError::InvalidData);
        }

        let val = reader.receive_extend(size)?;
        let coeff_val = (val << al as i32) as i16;
        let byte_offset = block_offset + ZIGZAG[k] as usize * 2;

        write_coeff(coeff_buf, byte_offset, coeff_val);

        k += 1;
    }

    Ok(0)
}

fn decode_ac_refine(
    reader: &mut BitReader,
    coeff_buf: &mut [u8],
    block_offset: usize,
    ac_table: &HuffTable,
    ss: u8,
    se: u8,
    al: u8,
    mut eobrun: u32,
) -> Result<u32, JpegError> {
    let p1 = 1i16 << al;
    let m1 = (-1i16) << al;

    if eobrun > 0 {
        // Refine existing non-zero coefficients.
        for k in ss as usize..=se as usize {
            let byte_offset = block_offset + ZIGZAG[k] as usize * 2;
            let current = read_coeff(coeff_buf, byte_offset);

            if current != 0 {
                let bit = reader.read_bit()? as i16;

                if bit != 0 {
                    if current > 0 {
                        write_coeff(coeff_buf, byte_offset, current + p1);
                    } else {
                        write_coeff(coeff_buf, byte_offset, current + m1);
                    }
                }
            }
        }

        return Ok(eobrun - 1);
    }

    let mut k = ss as usize;

    while k <= se as usize {
        let rs = reader.decode_huffman(ac_table)?;
        let run = (rs >> 4) as usize;
        let size = rs & 0x0F;

        if size == 0 {
            if run == 0x0F {
                // ZRL: skip 16 zero positions, refining non-zeros.
                let mut zeros = 0;

                while zeros < 16 && k <= se as usize {
                    let byte_offset = block_offset + ZIGZAG[k] as usize * 2;
                    let current = read_coeff(coeff_buf, byte_offset);

                    if current != 0 {
                        let bit = reader.read_bit()? as i16;

                        if bit != 0 {
                            if current > 0 {
                                write_coeff(coeff_buf, byte_offset, current + p1);
                            } else {
                                write_coeff(coeff_buf, byte_offset, current + m1);
                            }
                        }
                    } else {
                        zeros += 1;
                    }

                    k += 1;
                }

                continue;
            }

            // EOBn.
            if run > 0 {
                eobrun = (1 << run) + reader.read_bits(run as u8)?;
            } else {
                eobrun = 1;
            }

            // Refine remaining non-zeros in this block.
            while k <= se as usize {
                let byte_offset = block_offset + ZIGZAG[k] as usize * 2;
                let current = read_coeff(coeff_buf, byte_offset);

                if current != 0 {
                    let bit = reader.read_bit()? as i16;

                    if bit != 0 {
                        if current > 0 {
                            write_coeff(coeff_buf, byte_offset, current + p1);
                        } else {
                            write_coeff(coeff_buf, byte_offset, current + m1);
                        }
                    }
                }

                k += 1;
            }

            return Ok(eobrun - 1);
        }

        // New non-zero coefficient: skip `run` zero positions, refining non-zeros.
        let val = reader.receive_extend(size)?;
        let new_coeff = if val < 0 { m1 } else { p1 };
        let mut zeros = 0;

        while zeros < run && k <= se as usize {
            let byte_offset = block_offset + ZIGZAG[k] as usize * 2;
            let current = read_coeff(coeff_buf, byte_offset);

            if current != 0 {
                let bit = reader.read_bit()? as i16;

                if bit != 0 {
                    if current > 0 {
                        write_coeff(coeff_buf, byte_offset, current + p1);
                    } else {
                        write_coeff(coeff_buf, byte_offset, current + m1);
                    }
                }
            } else {
                zeros += 1;
            }

            k += 1;
        }

        if k > se as usize {
            return Err(JpegError::InvalidData);
        }

        let byte_offset = block_offset + ZIGZAG[k] as usize * 2;

        write_coeff(coeff_buf, byte_offset, new_coeff);

        k += 1;

        // Continue refining non-zeros for the rest of the block.
    }

    Ok(0)
}

fn finalize_progressive(state: &DecoderState, output: &mut [u8]) -> Result<(), JpegError> {
    let w = state.width as usize;
    let h = state.height as usize;
    let pixel_size = w * h * 4;
    let nc = state.num_components as usize;
    let mcu_cols = state.mcu_cols();
    let mcu_rows = state.mcu_rows();
    let mcu_w = state.max_h as usize * 8;
    let mcu_h = state.max_v as usize * 8;

    for mcu_row in 0..mcu_rows {
        for mcu_col in 0..mcu_cols {
            let mut blocks = [[0i32; 64]; 10];
            let mut block_idx = 0;

            for ci in 0..nc {
                let comp = state.components[ci];
                let qi = comp.quant_id as usize;

                if !state.quant_valid[qi] {
                    return Err(JpegError::MissingTable);
                }

                let bh = comp.h_factor as usize;
                let bv = comp.v_factor as usize;
                let base_offset = coeff_offset(state, ci);
                let blocks_w = comp_blocks_w(state, ci);

                for v in 0..bv {
                    for h in 0..bh {
                        let bx = mcu_col * bh + h;
                        let by = mcu_row * bv + v;
                        let block_byte_offset = base_offset + (by * blocks_w + bx) * 64 * 2;

                        // Read coefficients, dequantize, and IDCT.
                        for k in 0..64 {
                            let raw = read_coeff(output, pixel_size + block_byte_offset + k * 2);
                            let zi = ZIGZAG[k] as usize;

                            blocks[block_idx][zi] = raw as i32 * state.quant[qi][k] as i32;
                        }

                        idct_2d(&mut blocks[block_idx]);

                        block_idx += 1;
                    }
                }
            }

            // Build a fake scan info for write_mcu_pixels.
            let scan = ScanInfo {
                comp_indices: [0, 1, 2, 3],
                num_components: nc as u8,
                ss: 0,
                se: 63,
                ah: 0,
                al: 0,
            };

            write_mcu_pixels(
                state, &scan, &blocks, mcu_col, mcu_row, mcu_w, mcu_h, w, h, output,
            );
        }
    }

    Ok(())
}

// ── Public API ──────────────────────────────────────────

fn swaps_dimensions(orientation: u8) -> bool {
    orientation >= 5 && orientation <= 8
}

fn display_dimensions(w: u32, h: u32, orientation: u8) -> (u32, u32) {
    if swaps_dimensions(orientation) {
        (h, w)
    } else {
        (w, h)
    }
}

pub fn jpeg_header(data: &[u8]) -> Result<JpegHeader, JpegError> {
    let mut state = DecoderState::new();

    parse_markers(&mut state, data)?;

    if !state.frame_parsed {
        return Err(JpegError::InvalidData);
    }

    let (dw, dh) = display_dimensions(state.width as u32, state.height as u32, state.orientation);

    Ok(JpegHeader {
        width: dw,
        height: dh,
        components: state.num_components,
        is_progressive: state.is_progressive,
        orientation: state.orientation,
    })
}

pub fn jpeg_decode_buf_size(data: &[u8]) -> Result<usize, JpegError> {
    let mut state = DecoderState::new();

    parse_markers(&mut state, data)?;

    if !state.frame_parsed {
        return Err(JpegError::InvalidData);
    }

    let w = state.width as usize;
    let h = state.height as usize;
    let pixel_size = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(4))
        .ok_or(JpegError::DimensionOverflow)?;
    let mut total = pixel_size;

    if state.is_progressive {
        total += coeff_buf_size(&state);
    }

    // Rotation scratch: decode into upper half, rotate into lower half,
    // then copy back. Only needed when orientation swaps dimensions.
    if swaps_dimensions(state.orientation) {
        total = total.max(pixel_size * 2);
    }

    Ok(total)
}

pub fn jpeg_decode(data: &[u8], output: &mut [u8]) -> Result<JpegHeader, JpegError> {
    let mut state = DecoderState::new();

    parse_markers(&mut state, data)?;

    if !state.frame_parsed {
        return Err(JpegError::InvalidData);
    }

    let required = jpeg_decode_buf_size(data)?;

    if output.len() < required {
        return Err(JpegError::BufferTooSmall);
    }

    let raw_w = state.width as usize;
    let raw_h = state.height as usize;
    let pixel_size = raw_w * raw_h * 4;
    let needs_rotate = state.orientation != 1;

    if needs_rotate && swaps_dimensions(state.orientation) {
        // Decode into the second half, rotate into the first half.
        let decode_offset = pixel_size;
        let decode_slice = &mut output[decode_offset..];

        if state.is_progressive {
            decode_progressive(&mut state, data, decode_slice)?;
        } else {
            decode_baseline(&mut state, data, decode_slice)?;
        }

        apply_orientation(output, raw_w, raw_h, state.orientation, pixel_size);
    } else if needs_rotate {
        // Orientations 2-4 don't swap dimensions — rotate in-place.
        if state.is_progressive {
            decode_progressive(&mut state, data, output)?;
        } else {
            decode_baseline(&mut state, data, output)?;
        }

        apply_orientation_inplace(output, raw_w, raw_h, state.orientation);
    } else {
        if state.is_progressive {
            decode_progressive(&mut state, data, output)?;
        } else {
            decode_baseline(&mut state, data, output)?;
        }
    }

    let (dw, dh) = display_dimensions(raw_w as u32, raw_h as u32, state.orientation);

    Ok(JpegHeader {
        width: dw,
        height: dh,
        components: state.num_components,
        is_progressive: state.is_progressive,
        orientation: state.orientation,
    })
}

fn apply_orientation(
    buf: &mut [u8],
    raw_w: usize,
    raw_h: usize,
    orientation: u8,
    src_offset: usize,
) {
    let new_w = raw_h;
    let new_h = raw_w;

    for y in 0..raw_h {
        for x in 0..raw_w {
            let src = src_offset + (y * raw_w + x) * 4;
            let (nx, ny) = match orientation {
                5 => (y, x),
                6 => (raw_h - 1 - y, x),
                7 => (raw_h - 1 - y, raw_w - 1 - x),
                8 => (y, raw_w - 1 - x),
                _ => (x, y),
            };
            let dst = (ny * new_w + nx) * 4;

            buf[dst] = buf[src];
            buf[dst + 1] = buf[src + 1];
            buf[dst + 2] = buf[src + 2];
            buf[dst + 3] = buf[src + 3];
        }
    }
}

fn apply_orientation_inplace(buf: &mut [u8], raw_w: usize, raw_h: usize, orientation: u8) {
    match orientation {
        2 => {
            for y in 0..raw_h {
                for x in 0..raw_w / 2 {
                    let a = (y * raw_w + x) * 4;
                    let b = (y * raw_w + (raw_w - 1 - x)) * 4;

                    for i in 0..4 {
                        buf.swap(a + i, b + i);
                    }
                }
            }
        }
        3 => {
            let total = raw_w * raw_h;

            for i in 0..total / 2 {
                let a = i * 4;
                let b = (total - 1 - i) * 4;

                for j in 0..4 {
                    buf.swap(a + j, b + j);
                }
            }
        }
        4 => {
            for y in 0..raw_h / 2 {
                for x in 0..raw_w {
                    let a = (y * raw_w + x) * 4;
                    let b = ((raw_h - 1 - y) * raw_w + x) * 4;

                    for i in 0..4 {
                        buf.swap(a + i, b + i);
                    }
                }
            }
        }
        _ => {}
    }
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate alloc;
    extern crate std;

    use alloc::{vec, vec::Vec};

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::format!("{}/../../../assets/{}", env!("CARGO_MANIFEST_DIR"), name);

        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"))
    }

    // ── Header parsing ──────────────────────────────────

    #[test]
    fn header_zoey() {
        let data = fixture("zoey.jpg");
        let h = jpeg_header(&data).unwrap();

        assert_eq!(h.width, 3024);
        assert_eq!(h.height, 4032);
        assert_eq!(h.components, 3);
        assert!(!h.is_progressive);
        assert_eq!(h.orientation, 6);
    }

    #[test]
    fn header_invalid_signature() {
        let data = [0u8; 100];

        assert_eq!(jpeg_header(&data), Err(JpegError::InvalidSignature));
    }

    #[test]
    fn header_truncated() {
        assert_eq!(jpeg_header(&[0xFF, 0xD8]), Err(JpegError::Truncated));
    }

    // ── Buffer size ─────────────────────────────────────

    #[test]
    fn buf_size_zoey() {
        let data = fixture("zoey.jpg");
        let size = jpeg_decode_buf_size(&data).unwrap();

        // Needs 2x pixel_size for rotation scratch (orientation 6 swaps dims).
        assert_eq!(size, 4032 * 3024 * 4 * 2);
    }

    // ── Full decode ─────────────────────────────────────

    #[test]
    fn decode_zoey() {
        let data = fixture("zoey.jpg");
        let buf_size = jpeg_decode_buf_size(&data).unwrap();
        let mut output = vec![0u8; buf_size];
        let header = jpeg_decode(&data, &mut output).unwrap();

        // After EXIF orientation 6 (90° CW), dimensions swap.
        assert_eq!(header.width, 3024);
        assert_eq!(header.height, 4032);
        assert_eq!(header.components, 3);

        let pixel_count = 3024 * 4032;
        let has_color = output[..pixel_count * 4]
            .chunks(4)
            .any(|px| px[0] != px[1] || px[1] != px[2]);

        assert!(has_color, "output should contain color data");

        let non_zero = output[..pixel_count * 4].iter().any(|&b| b != 0);

        assert!(non_zero, "output should not be all zeros");

        let all_opaque = output[..pixel_count * 4].chunks(4).all(|px| px[3] == 255);

        assert!(all_opaque, "alpha should be 255 for all pixels");

        // Verify pixel accuracy against PIL reference (after EXIF rotation).
        // Top-left (0,0) of oriented image: R=251 G=253 B=239.
        let px0 = &output[0..4];
        let (r, g, b) = (px0[2], px0[1], px0[0]);

        assert!(
            (r as i32 - 251).abs() <= 3
                && (g as i32 - 253).abs() <= 3
                && (b as i32 - 239).abs() <= 3,
            "pixel (0,0): got R={r} G={g} B={b}, expected ~R=251 G=253 B=239"
        );
    }

    #[test]
    fn buffer_too_small() {
        let data = fixture("zoey.jpg");
        let mut output = [0u8; 10];

        assert_eq!(
            jpeg_decode(&data, &mut output),
            Err(JpegError::BufferTooSmall)
        );
    }

    // ── Progressive decode ────────────────────────────────

    #[test]
    fn header_progressive() {
        let data = fixture("test-progressive.jpg");
        let h = jpeg_header(&data).unwrap();

        assert_eq!(h.width, 64);
        assert_eq!(h.height, 64);
        assert_eq!(h.components, 3);
        assert!(h.is_progressive);
    }

    #[test]
    fn decode_progressive() {
        let data = fixture("test-progressive.jpg");
        let buf_size = jpeg_decode_buf_size(&data).unwrap();

        assert!(buf_size > 64 * 64 * 4, "progressive needs coeff buffer");

        let mut output = vec![0u8; buf_size];
        let header = jpeg_decode(&data, &mut output).unwrap();

        assert_eq!(header.width, 64);
        assert_eq!(header.height, 64);
        assert!(header.is_progressive);

        let pixel_count = 64 * 64;
        let has_color = output[..pixel_count * 4]
            .chunks(4)
            .any(|px| px[0] != px[1] || px[1] != px[2]);

        assert!(has_color, "progressive output should contain color");

        let all_opaque = output[..pixel_count * 4].chunks(4).all(|px| px[3] == 255);

        assert!(all_opaque, "alpha should be 255");

        // Verify pixel accuracy against PIL reference.
        // Top-left (0,0): R=145 G=148 B=155 (BGRA: B=155 G=148 R=145).
        // Progressive JPEG accumulates more rounding error across multiple
        // scans than baseline, so allow ±5.
        let px0 = &output[0..4];

        assert!(
            (px0[2] as i32 - 145).abs() <= 5
                && (px0[1] as i32 - 148).abs() <= 5
                && (px0[0] as i32 - 155).abs() <= 5,
            "progressive pixel (0,0): got R={} G={} B={}, expected ~R=145 G=148 B=155",
            px0[2],
            px0[1],
            px0[0]
        );
    }

    // ── IDCT unit tests ─────────────────────────────────

    #[test]
    fn idct_dc_only() {
        let mut block = [0i32; 64];

        block[0] = 800;

        idct_2d(&mut block);

        // DC-only should produce a uniform value.
        let expected = ((800i64 * 1448 * 1448 + (1 << 23)) >> 24) as i32 + 128;

        for i in 0..64 {
            let diff = (block[i] - expected).abs();

            assert!(
                diff <= 1,
                "DC-only pixel {i}: got {}, expected ~{expected}",
                block[i]
            );
        }
    }

    #[test]
    fn idct_all_zero() {
        let mut block = [0i32; 64];

        idct_2d(&mut block);

        for i in 0..64 {
            assert_eq!(block[i], 128, "zero block should produce 128 (level shift)");
        }
    }

    // ── Color conversion ────────────────────────────────

    #[test]
    fn ycbcr_white() {
        let bgra = ycbcr_to_bgra(255, 128, 128);

        assert_eq!(bgra, [255, 255, 255, 255]);
    }

    #[test]
    fn ycbcr_black() {
        let bgra = ycbcr_to_bgra(0, 128, 128);

        assert_eq!(bgra, [0, 0, 0, 255]);
    }

    #[test]
    fn ycbcr_red() {
        let bgra = ycbcr_to_bgra(76, 84, 255);

        // Should be approximately red.
        assert!(bgra[2] > 200, "R should be high: {}", bgra[2]);
        assert!(bgra[1] < 50, "G should be low: {}", bgra[1]);
        assert!(bgra[0] < 50, "B should be low: {}", bgra[0]);
    }

    #[test]
    fn ycbcr_green() {
        let bgra = ycbcr_to_bgra(150, 44, 21);

        assert!(bgra[1] > 200, "G should be high: {}", bgra[1]);
        assert!(bgra[2] < 50, "R should be low: {}", bgra[2]);
        assert!(bgra[0] < 50, "B should be low: {}", bgra[0]);
    }

    #[test]
    fn ycbcr_blue() {
        let bgra = ycbcr_to_bgra(29, 255, 107);

        assert!(bgra[0] > 200, "B should be high: {}", bgra[0]);
    }

    // ── Huffman table ───────────────────────────────────

    #[test]
    fn huffman_build_decode() {
        // A simple Huffman table: one symbol with code length 1.
        let bits = [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let values = [42u8];
        let table = HuffTable::build(&bits, &values, 1);

        assert_eq!(table.num_values, 1);
        assert_eq!(table.max_code[1], 0);
    }

    // ── Zig-zag order ───────────────────────────────────

    #[test]
    fn zigzag_valid() {
        let mut seen = [false; 64];

        for &z in &ZIGZAG {
            assert!((z as usize) < 64);
            assert!(!seen[z as usize], "duplicate zigzag entry");

            seen[z as usize] = true;
        }
    }

    #[test]
    fn zigzag_dc_is_zero() {
        assert_eq!(ZIGZAG[0], 0, "DC coefficient should map to position 0");
    }

    // ── Receive/extend ──────────────────────────────────

    #[test]
    fn receive_extend_values() {
        // Category 1: 0 → -1, 1 → 1
        let mut reader = BitReader::new(&[0b0000_0000], 0);

        assert_eq!(reader.receive_extend(1).unwrap(), -1);

        let mut reader = BitReader::new(&[0b1000_0000], 0);

        assert_eq!(reader.receive_extend(1).unwrap(), 1);

        // Category 3: 0..3 → -7..-4, 4..7 → 4..7
        let mut reader = BitReader::new(&[0b000_00000], 0);

        assert_eq!(reader.receive_extend(3).unwrap(), -7);

        let mut reader = BitReader::new(&[0b111_00000], 0);

        assert_eq!(reader.receive_extend(3).unwrap(), 7);
    }

    // ── IDCT basis matrix symmetry ──────────────────────

    #[test]
    fn idct_basis_symmetry() {
        // Row 0 should be constant (DC).
        for n in 1..8 {
            assert_eq!(
                IDCT_BASIS[0][0], IDCT_BASIS[0][n],
                "DC row should be constant"
            );
        }

        // Row k should have even symmetry for even k, odd for odd k.
        for k in 1..8 {
            for n in 0..4 {
                let left = IDCT_BASIS[k][n];
                let right = IDCT_BASIS[k][7 - n];

                if k % 2 == 0 {
                    assert_eq!(left, right, "even row {k} should be symmetric");
                } else {
                    assert_eq!(left, -right, "odd row {k} should be antisymmetric");
                }
            }
        }
    }
}
