// TrueType font parser — zero-copy, no allocations.
//
// Parses a TTF file from a borrowed byte slice. Extracts glyph outlines,
// metrics, and codepoint mappings. All table access is bounds-checked.
//
// Only handles TrueType outlines (quadratic beziers in `glyf` table).
// Does NOT handle OpenType/CFF fonts.

// ---------------------------------------------------------------------------
// Big-endian readers
// ---------------------------------------------------------------------------

fn read_i16_be(data: &[u8], off: usize) -> Option<i16> {
    if off + 2 > data.len() {
        return None;
    }

    Some(i16::from_be_bytes([data[off], data[off + 1]]))
}
fn read_u8(data: &[u8], off: usize) -> Option<u8> {
    data.get(off).copied()
}
fn read_u16_be(data: &[u8], off: usize) -> Option<u16> {
    if off + 2 > data.len() {
        return None;
    }

    Some(u16::from_be_bytes([data[off], data[off + 1]]))
}
fn read_u32_be(data: &[u8], off: usize) -> Option<u32> {
    if off + 4 > data.len() {
        return None;
    }

    Some(u32::from_be_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
}

// ---------------------------------------------------------------------------
// Table locations
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct TableLoc {
    offset: u32,
    length: u32,
}

impl TableLoc {
    fn slice<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        let start = self.offset as usize;
        let end = start + self.length as usize;

        if end <= data.len() {
            Some(&data[start..end])
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Metrics for a single rasterized glyph.
#[derive(Clone, Copy, Debug)]
pub struct GlyphMetrics {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal offset from pen position to left edge of bitmap.
    pub bearing_x: i32,
    /// Vertical offset from baseline to top edge of bitmap (positive = up).
    pub bearing_y: i32,
    /// Horizontal advance to next glyph in pixels.
    pub advance: u32,
}
/// Caller-provided buffer for rasterization output (1 byte per pixel coverage).
pub struct RasterBuffer<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}
/// A parsed TrueType font. Borrows the raw font data — zero-copy.
pub struct TrueTypeFont<'a> {
    data: &'a [u8],
    cmap: TableLoc,
    glyf: TableLoc,
    hmtx: TableLoc,
    loca: TableLoc,
    gpos: TableLoc,
    units_per_em: u16,
    #[allow(dead_code)]
    num_glyphs: u16,
    loca_format: i16, // 0 = short (u16 * 2), 1 = long (u32)
    num_h_metrics: u16,
    /// hhea ascent (positive, in font units).
    hhea_ascent: i16,
    /// hhea descent (negative, in font units).
    hhea_descent: i16,
    /// hhea lineGap (in font units).
    hhea_line_gap: i16,
}

// ---------------------------------------------------------------------------
// Glyph outline (intermediate, used during rasterization)
// ---------------------------------------------------------------------------

/// Maximum points per glyph outline.
const MAX_GLYPH_POINTS: usize = 512;
/// Maximum contours per glyph.
const MAX_CONTOURS: usize = 64;

/// Decoded glyph outline — contours of on-curve and off-curve points.
pub struct GlyphOutline {
    pub points: [GlyphPoint; MAX_GLYPH_POINTS],
    pub num_points: u16,
    pub contour_ends: [u16; MAX_CONTOURS],
    pub num_contours: u16,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}
/// A point in a glyph outline, in font units.
#[derive(Clone, Copy, Default)]
pub struct GlyphPoint {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

impl GlyphOutline {
    pub const fn zeroed() -> Self {
        GlyphOutline {
            points: [GlyphPoint {
                x: 0,
                y: 0,
                on_curve: false,
            }; MAX_GLYPH_POINTS],
            num_points: 0,
            contour_ends: [0u16; MAX_CONTOURS],
            num_contours: 0,
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Font parsing
// ---------------------------------------------------------------------------

impl<'a> TrueTypeFont<'a> {
    /// Parse a TrueType font from raw data. Returns `None` if the data is
    /// not a valid TrueType font or required tables are missing.
    pub fn new(data: &'a [u8]) -> Option<Self> {
        // Minimum: 12-byte offset table header.
        if data.len() < 12 {
            return None;
        }

        // Check sfVersion: must be 0x00010000 (TrueType).
        let sf_version = read_u32_be(data, 0)?;

        if sf_version != 0x0001_0000 {
            return None;
        }

        let num_tables = read_u16_be(data, 4)? as usize;
        // Locate required tables.
        let mut head = TableLoc::default();
        let mut maxp = TableLoc::default();
        let mut cmap = TableLoc::default();
        let mut hhea = TableLoc::default();
        let mut hmtx = TableLoc::default();
        let mut loca = TableLoc::default();
        let mut glyf = TableLoc::default();
        let mut gpos = TableLoc::default();
        let mut found: u8 = 0;

        for i in 0..num_tables {
            let entry = 12 + i * 16;

            if entry + 16 > data.len() {
                return None;
            }

            let tag = read_u32_be(data, entry)?;
            let offset = read_u32_be(data, entry + 8)?;
            let length = read_u32_be(data, entry + 12)?;
            let loc = TableLoc { offset, length };

            // Validate table doesn't exceed data.
            if (offset as usize) + (length as usize) > data.len() {
                return None;
            }

            match &tag.to_be_bytes() {
                b"head" => {
                    head = loc;
                    found |= 1 << 0;
                }
                b"maxp" => {
                    maxp = loc;
                    found |= 1 << 1;
                }
                b"cmap" => {
                    cmap = loc;
                    found |= 1 << 2;
                }
                b"hhea" => {
                    hhea = loc;
                    found |= 1 << 3;
                }
                b"hmtx" => {
                    hmtx = loc;
                    found |= 1 << 4;
                }
                b"loca" => {
                    loca = loc;
                    found |= 1 << 5;
                }
                b"glyf" => {
                    glyf = loc;
                    found |= 1 << 6;
                }
                b"GPOS" => {
                    gpos = loc; // optional
                }
                _ => {}
            }
        }

        // All 7 required tables present?
        if found != 0x7F {
            return None;
        }

        // Parse scalar values from head, maxp, hhea.
        let head_data = head.slice(data)?;
        let maxp_data = maxp.slice(data)?;
        let hhea_data = hhea.slice(data)?;
        let units_per_em = read_u16_be(head_data, 18)?;
        let loca_format = read_i16_be(head_data, 50)?;
        let num_glyphs = read_u16_be(maxp_data, 4)?;
        let num_h_metrics = read_u16_be(hhea_data, 34)?;

        if units_per_em == 0 || num_glyphs == 0 || num_h_metrics == 0 {
            return None;
        }

        // Parse hhea ascent/descent/lineGap (offsets 4, 6, 8 in hhea table).
        let hhea_ascent = read_i16_be(hhea_data, 4)?;
        let hhea_descent = read_i16_be(hhea_data, 6)?;
        let hhea_line_gap = read_i16_be(hhea_data, 8)?;

        Some(TrueTypeFont {
            data,
            cmap,
            glyf,
            hmtx,
            loca,
            gpos,
            units_per_em,
            num_glyphs,
            loca_format,
            num_h_metrics,
            hhea_ascent,
            hhea_descent,
            hhea_line_gap,
        })
    }

    /// Format 4 cmap lookup (segmented mapping for BMP codepoints).
    fn cmap_format4(&self, sub: &[u8], cp: u32) -> Option<u16> {
        if cp > 0xFFFF {
            return None;
        }

        let cp = cp as u16;
        let seg_count = read_u16_be(sub, 6)? / 2;
        let seg_count_usize = seg_count as usize;
        // Array offsets within the subtable.
        let end_codes = 14;
        let start_codes = end_codes + seg_count_usize * 2 + 2; // +2 for reservedPad
        let id_deltas = start_codes + seg_count_usize * 2;
        let id_range_offsets = id_deltas + seg_count_usize * 2;
        // Binary search for the segment containing cp.
        let mut lo: usize = 0;
        let mut hi: usize = seg_count_usize;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let end_code = read_u16_be(sub, end_codes + mid * 2)?;

            if end_code < cp {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        if lo >= seg_count_usize {
            return None;
        }

        let end_code = read_u16_be(sub, end_codes + lo * 2)?;
        let start_code = read_u16_be(sub, start_codes + lo * 2)?;

        if cp < start_code || cp > end_code {
            return None;
        }

        let id_delta = read_i16_be(sub, id_deltas + lo * 2)?;
        let range_offset_pos = id_range_offsets + lo * 2;
        let id_range_offset = read_u16_be(sub, range_offset_pos)?;
        let glyph_index = if id_range_offset == 0 {
            // Simple delta mapping.
            (cp as i32 + id_delta as i32) as u16
        } else {
            // Offset into glyphIdArray. The offset is relative to its own
            // position in the idRangeOffset array.
            let glyph_offset =
                range_offset_pos + id_range_offset as usize + (cp - start_code) as usize * 2;
            let gi = read_u16_be(sub, glyph_offset)?;

            if gi == 0 {
                return None;
            }

            (gi as i32 + id_delta as i32) as u16
        };

        if glyph_index == 0 {
            None
        } else {
            Some(glyph_index)
        }
    }
    /// Look up a glyph index in a Coverage table.
    /// Returns the coverage index, or None if not found.
    fn coverage_index(&self, cov: &[u8], glyph: u16) -> Option<u16> {
        if cov.len() < 4 {
            return None;
        }

        let fmt = read_u16_be(cov, 0)?;

        match fmt {
            1 => {
                // Format 1: array of glyph IDs.
                let count = read_u16_be(cov, 2)? as usize;
                // Binary search.
                let mut lo = 0usize;
                let mut hi = count;

                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let g = read_u16_be(cov, 4 + mid * 2)?;

                    if g < glyph {
                        lo = mid + 1;
                    } else if g > glyph {
                        hi = mid;
                    } else {
                        return Some(mid as u16);
                    }
                }

                None
            }
            2 => {
                // Format 2: ranges.
                let range_count = read_u16_be(cov, 2)? as usize;

                for r in 0..range_count {
                    let roff = 4 + r * 6;
                    let start = read_u16_be(cov, roff)?;
                    let end = read_u16_be(cov, roff + 2)?;
                    let start_idx = read_u16_be(cov, roff + 4)?;

                    if glyph >= start && glyph <= end {
                        return Some(start_idx + (glyph - start));
                    }
                }

                None
            }
            _ => None,
        }
    }
    /// Locate a glyph's data in the glyf table. Returns the byte offset and
    /// length within the glyf table, or `None` for empty glyphs (e.g. space).
    fn glyph_location(&self, glyph_index: u16) -> Option<(usize, usize)> {
        let loca_data = self.loca.slice(self.data)?;
        let (off_this, off_next) = if self.loca_format == 0 {
            // Short format: offsets are u16, multiplied by 2.
            let a = read_u16_be(loca_data, glyph_index as usize * 2)? as usize * 2;
            let b = read_u16_be(loca_data, (glyph_index as usize + 1) * 2)? as usize * 2;

            (a, b)
        } else {
            // Long format: offsets are u32.
            let a = read_u32_be(loca_data, glyph_index as usize * 4)? as usize;
            let b = read_u32_be(loca_data, (glyph_index as usize + 1) * 4)? as usize;

            (a, b)
        };

        if off_next <= off_this {
            return None; // Empty glyph (space, .notdef with no outline).
        }

        Some((off_this, off_next - off_this))
    }
    /// Parse a compound glyph: read components and recursively extract
    /// simple outlines, applying x/y offsets to each component's points.
    fn glyph_outline_compound(&self, g: &[u8], out: &mut GlyphOutline) -> bool {
        const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
        const ARGS_ARE_XY_VALUES: u16 = 0x0002;
        const MORE_COMPONENTS: u16 = 0x0020;
        const WE_HAVE_A_SCALE: u16 = 0x0008;
        const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
        const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

        let mut cursor = 10; // skip header (numContours + bbox)
        let mut any = false;

        loop {
            if cursor + 4 > g.len() {
                break;
            }

            let flags = match read_u16_be(g, cursor) {
                Some(v) => v,
                None => break,
            };
            let component_idx = match read_u16_be(g, cursor + 2) {
                Some(v) => v,
                None => break,
            };

            cursor += 4;

            // Read offset arguments.
            let (dx, dy) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                let a = read_i16_be(g, cursor).unwrap_or(0) as i32;
                let b = read_i16_be(g, cursor + 2).unwrap_or(0) as i32;

                cursor += 4;
                (a, b)
            } else {
                if cursor + 2 > g.len() {
                    break;
                }

                let a = g[cursor] as i8 as i32;
                let b = g[cursor + 1] as i8 as i32;

                cursor += 2;
                (a, b)
            };

            // Skip optional scale/transform data (we don't apply transforms).
            if flags & WE_HAVE_A_SCALE != 0 {
                cursor += 2;
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                cursor += 4;
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                cursor += 8;
            }

            // Recursively extract the component's outline.
            let (comp_off, comp_len) = match self.glyph_location(component_idx) {
                Some(v) => v,
                None => {
                    if flags & MORE_COMPONENTS == 0 {
                        break;
                    }
                    continue;
                }
            };
            let glyf_data = match self.glyf.slice(self.data) {
                Some(d) => d,
                None => break,
            };

            if comp_off + comp_len > glyf_data.len() {
                if flags & MORE_COMPONENTS == 0 {
                    break;
                }
                continue;
            }

            let comp_g = &glyf_data[comp_off..comp_off + comp_len];
            let comp_nc = match read_i16_be(comp_g, 0) {
                Some(n) if n > 0 => n as usize,
                _ => {
                    if flags & MORE_COMPONENTS == 0 {
                        break;
                    }
                    continue;
                }
            };

            let pts_before = out.num_points as usize;

            if !Self::glyph_outline_simple(comp_g, comp_nc, out) {
                if flags & MORE_COMPONENTS == 0 {
                    break;
                }
                continue;
            }

            // Apply offset to the newly added points.
            if (flags & ARGS_ARE_XY_VALUES != 0) && (dx != 0 || dy != 0) {
                let pts_after = out.num_points as usize;

                for i in pts_before..pts_after {
                    out.points[i].x += dx;
                    out.points[i].y += dy;
                }
            }

            any = true;

            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }

        any
    }
    /// Parse a simple glyph's contours, points, and flags from raw glyf data.
    fn glyph_outline_simple(g: &[u8], nc: usize, out: &mut GlyphOutline) -> bool {
        if nc > MAX_CONTOURS {
            return false;
        }

        let base_contours = out.num_contours as usize;
        let base_points = out.num_points as usize;

        if base_contours + nc > MAX_CONTOURS {
            return false;
        }

        for i in 0..nc {
            out.contour_ends[base_contours + i] = match read_u16_be(g, 10 + i * 2) {
                Some(v) => v + base_points as u16,
                None => return false,
            };
        }

        let local_last = match read_u16_be(g, 10 + (nc - 1) * 2) {
            Some(v) => v as usize,
            None => return false,
        };
        let num_points = local_last + 1;

        if base_points + num_points > MAX_GLYPH_POINTS {
            return false;
        }

        // Skip instructions.
        let instr_off = 10 + nc * 2;
        let instr_len = match read_u16_be(g, instr_off) {
            Some(v) => v as usize,
            None => return false,
        };
        let mut flags = [0u8; MAX_GLYPH_POINTS];
        let mut cursor = instr_off + 2 + instr_len;
        let mut fi = 0;

        while fi < num_points {
            let flag = match read_u8(g, cursor) {
                Some(v) => v,
                None => return false,
            };

            cursor += 1;
            flags[fi] = flag;
            fi += 1;

            if flag & 0x08 != 0 && fi < num_points {
                let repeat = match read_u8(g, cursor) {
                    Some(v) => v as usize,
                    None => return false,
                };

                cursor += 1;

                let end = min_usize(fi + repeat, num_points);

                while fi < end {
                    flags[fi] = flag;
                    fi += 1;
                }
            }
        }

        let mut x: i32 = 0;

        for i in 0..num_points {
            let f = flags[i];

            if f & 0x02 != 0 {
                let dx = read_u8(g, cursor).unwrap_or(0) as i32;

                cursor += 1;
                x += if f & 0x10 != 0 { dx } else { -dx };
            } else if f & 0x10 == 0 {
                let dx = match read_i16_be(g, cursor) {
                    Some(v) => v as i32,
                    None => return false,
                };

                cursor += 2;
                x += dx;
            }

            out.points[base_points + i].x = x;
        }

        let mut y: i32 = 0;

        for i in 0..num_points {
            let f = flags[i];

            if f & 0x04 != 0 {
                let dy = read_u8(g, cursor).unwrap_or(0) as i32;

                cursor += 1;
                y += if f & 0x20 != 0 { dy } else { -dy };
            } else if f & 0x20 == 0 {
                let dy = match read_i16_be(g, cursor) {
                    Some(v) => v as i32,
                    None => return false,
                };

                cursor += 2;
                y += dy;
            }

            out.points[base_points + i].y = y;
        }

        for i in 0..num_points {
            out.points[base_points + i].on_curve = flags[i] & 0x01 != 0;
        }

        out.num_contours = (base_contours + nc) as u16;
        out.num_points = (base_points + num_points) as u16;

        true
    }
    /// PairPos format 1: individual glyph pairs with per-pair adjustments.
    fn gpos_pairpos_format1(&self, sub: &[u8], left: u16, right: u16) -> i16 {
        // Format: posFormat(2), coverageOffset(2), valueFormat1(2),
        //         valueFormat2(2), pairSetCount(2), pairSetOffsets[](2 each)
        if sub.len() < 10 {
            return 0;
        }

        let cov_off = match read_u16_be(sub, 2) {
            Some(v) => v as usize,
            None => return 0,
        };
        let val_format1 = match read_u16_be(sub, 4) {
            Some(v) => v,
            None => return 0,
        };
        let _val_format2 = match read_u16_be(sub, 6) {
            Some(v) => v,
            None => return 0,
        };
        let pair_set_count = match read_u16_be(sub, 8) {
            Some(v) => v as usize,
            None => return 0,
        };
        // Find the left glyph's index in the coverage table.
        let cov_idx = match self.coverage_index(&sub[cov_off..], left) {
            Some(idx) => idx as usize,
            None => return 0,
        };

        if cov_idx >= pair_set_count {
            return 0;
        }

        let ps_off = match read_u16_be(sub, 10 + cov_idx * 2) {
            Some(v) => v as usize,
            None => return 0,
        };

        if ps_off >= sub.len() {
            return 0;
        }

        let ps = &sub[ps_off..];
        let pv_count = match read_u16_be(ps, 0) {
            Some(v) => v as usize,
            None => return 0,
        };

        // Record size: secondGlyph(2) + valueFormat1 values + valueFormat2 values.
        let vf1_size = value_format_size(val_format1);
        let vf2_size = value_format_size(_val_format2);
        let record_size = 2 + vf1_size + vf2_size;

        // Binary search for right glyph in the sorted pair list.
        let mut lo = 0usize;
        let mut hi = pv_count;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec_off = 2 + mid * record_size;
            let second = match read_u16_be(ps, rec_off) {
                Some(v) => v,
                None => return 0,
            };

            if second < right {
                lo = mid + 1;
            } else if second > right {
                hi = mid;
            } else {
                // Found the pair — extract XAdvance from valueFormat1.
                // valueFormat1 bit 0x0004 = XAdvance is at bit position 2.
                // The offset depends on which lower bits are set.
                return extract_x_advance(ps, rec_off + 2, val_format1);
            }
        }

        0
    }
    /// PairPos format 2: class-based pair adjustments.
    fn gpos_pairpos_format2(&self, sub: &[u8], left: u16, right: u16) -> i16 {
        // Format: posFormat(2), coverageOffset(2), valueFormat1(2),
        //         valueFormat2(2), classDef1Offset(2), classDef2Offset(2),
        //         class1Count(2), class2Count(2), class1Records[]
        if sub.len() < 16 {
            return 0;
        }

        let cov_off = match read_u16_be(sub, 2) {
            Some(v) => v as usize,
            None => return 0,
        };

        // Check coverage for the left glyph first.
        if cov_off >= sub.len() {
            return 0;
        }
        if self.coverage_index(&sub[cov_off..], left).is_none() {
            return 0;
        }

        let val_format1 = match read_u16_be(sub, 4) {
            Some(v) => v,
            None => return 0,
        };
        let val_format2 = match read_u16_be(sub, 6) {
            Some(v) => v,
            None => return 0,
        };
        let cd1_off = match read_u16_be(sub, 8) {
            Some(v) => v as usize,
            None => return 0,
        };
        let cd2_off = match read_u16_be(sub, 10) {
            Some(v) => v as usize,
            None => return 0,
        };
        let class1_count = match read_u16_be(sub, 12) {
            Some(v) => v as usize,
            None => return 0,
        };
        let class2_count = match read_u16_be(sub, 14) {
            Some(v) => v as usize,
            None => return 0,
        };

        if cd1_off >= sub.len() || cd2_off >= sub.len() {
            return 0;
        }

        let c1 = classdef_lookup(&sub[cd1_off..], left) as usize;
        let c2 = classdef_lookup(&sub[cd2_off..], right) as usize;

        if c1 >= class1_count || c2 >= class2_count {
            return 0;
        }

        let vf1_size = value_format_size(val_format1);
        let vf2_size = value_format_size(val_format2);
        let record_size = vf1_size + vf2_size;
        // Class1Record array starts at offset 16. Each Class1Record contains
        // class2_count * (vf1_size + vf2_size) bytes.
        let class1_record_size = class2_count * record_size;
        let rec_off = 16 + c1 * class1_record_size + c2 * record_size;

        extract_x_advance(sub, rec_off, val_format1)
    }
    /// Look up kerning in a single GPOS PairPos subtable.
    fn gpos_pairpos_lookup(&self, sub: &[u8], left: u16, right: u16) -> i16 {
        if sub.len() < 2 {
            return 0;
        }

        let pos_format = match read_u16_be(sub, 0) {
            Some(v) => v,
            None => return 0,
        };

        match pos_format {
            1 => self.gpos_pairpos_format1(sub, left, right),
            2 => self.gpos_pairpos_format2(sub, left, right),
            _ => 0,
        }
    }

    /// Map a Unicode codepoint to a glyph index via the cmap table.
    /// Returns `None` if the codepoint has no mapping (falls to .notdef).
    pub fn glyph_index(&self, codepoint: char) -> Option<u16> {
        let cmap_data = self.cmap.slice(self.data)?;
        let cp = codepoint as u32;
        // Walk encoding records to find a format 4 subtable.
        let num_subtables = read_u16_be(cmap_data, 2)? as usize;

        for i in 0..num_subtables {
            let rec = 4 + i * 8;
            let platform = read_u16_be(cmap_data, rec)?;
            let encoding = read_u16_be(cmap_data, rec + 2)?;
            let sub_offset = read_u32_be(cmap_data, rec + 4)? as usize;
            // Accept Unicode BMP subtables.
            let is_unicode = platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10));

            if !is_unicode {
                continue;
            }

            if sub_offset >= cmap_data.len() {
                continue;
            }

            let sub = &cmap_data[sub_offset..];
            let format = read_u16_be(sub, 0)?;

            if format == 4 {
                return self.cmap_format4(sub, cp);
            }
        }

        None
    }
    /// Get horizontal metrics for a glyph: (advance_width, left_side_bearing).
    pub fn glyph_h_metrics(&self, glyph_index: u16) -> Option<(u16, i16)> {
        let hmtx_data = self.hmtx.slice(self.data)?;

        if glyph_index < self.num_h_metrics {
            let off = glyph_index as usize * 4;
            let advance = read_u16_be(hmtx_data, off)?;
            let lsb = read_i16_be(hmtx_data, off + 2)?;

            Some((advance, lsb))
        } else {
            // Glyphs beyond num_h_metrics share the last advance width.
            let last_adv_off = (self.num_h_metrics as usize - 1) * 4;
            let advance = read_u16_be(hmtx_data, last_adv_off)?;
            let lsb_off =
                self.num_h_metrics as usize * 4 + (glyph_index - self.num_h_metrics) as usize * 2;
            let lsb = read_i16_be(hmtx_data, lsb_off)?;

            Some((advance, lsb))
        }
    }
    /// Extract the outline for a glyph (simple or compound).
    pub fn glyph_outline(&self, glyph_index: u16, out: &mut GlyphOutline) -> bool {
        out.num_points = 0;
        out.num_contours = 0;

        let (glyf_off, glyf_len) = match self.glyph_location(glyph_index) {
            Some(v) => v,
            None => return false,
        };
        let glyf_data = match self.glyf.slice(self.data) {
            Some(d) => d,
            None => return false,
        };

        if glyf_off + glyf_len > glyf_data.len() {
            return false;
        }

        let g = &glyf_data[glyf_off..glyf_off + glyf_len];
        let num_contours = match read_i16_be(g, 0) {
            Some(n) => n,
            None => return false,
        };

        out.x_min = read_i16_be(g, 2).unwrap_or(0);
        out.y_min = read_i16_be(g, 4).unwrap_or(0);
        out.x_max = read_i16_be(g, 6).unwrap_or(0);
        out.y_max = read_i16_be(g, 8).unwrap_or(0);

        if num_contours < 0 {
            self.glyph_outline_compound(g, out)
        } else if num_contours > 0 {
            Self::glyph_outline_simple(g, num_contours as usize, out)
        } else {
            false
        }
    }
    /// Returns hhea ascent in font units (positive above baseline).
    pub fn hhea_ascent(&self) -> i16 {
        self.hhea_ascent
    }
    /// Returns hhea descent in font units (negative below baseline).
    pub fn hhea_descent(&self) -> i16 {
        self.hhea_descent
    }
    /// Returns hhea lineGap in font units.
    pub fn hhea_line_gap(&self) -> i16 {
        self.hhea_line_gap
    }
    /// Look up the GPOS kerning adjustment (x-advance delta) for a glyph pair.
    ///
    /// Searches the GPOS table for a PairPos lookup (lookup type 2). Supports
    /// format 1 (individual pairs) and format 2 (class-based). Returns the
    /// x-advance adjustment in font units, or 0 if no kerning is found.
    pub fn kern_advance(&self, left_glyph: u16, right_glyph: u16) -> i16 {
        if self.gpos.length == 0 {
            return 0;
        }

        let gpos_data = match self.gpos.slice(self.data) {
            Some(d) => d,
            None => return 0,
        };

        // GPOS header: version (4 bytes), scriptListOff (2), featureListOff (2),
        // lookupListOff (2).
        if gpos_data.len() < 10 {
            return 0;
        }

        let lookup_list_off = match read_u16_be(gpos_data, 8) {
            Some(v) => v as usize,
            None => return 0,
        };

        if lookup_list_off >= gpos_data.len() {
            return 0;
        }

        let ll = &gpos_data[lookup_list_off..];
        let lookup_count = match read_u16_be(ll, 0) {
            Some(v) => v as usize,
            None => return 0,
        };

        // Search all lookups for PairPos (type 2).
        for li in 0..lookup_count {
            let lookup_off = match read_u16_be(ll, 2 + li * 2) {
                Some(v) => v as usize,
                None => continue,
            };

            if lookup_off + 6 > ll.len() {
                continue;
            }

            let lookup = &ll[lookup_off..];
            let lookup_type = match read_u16_be(lookup, 0) {
                Some(v) => v,
                None => continue,
            };

            if lookup_type != 2 {
                continue; // Not PairPos.
            }

            let sub_count = match read_u16_be(lookup, 4) {
                Some(v) => v as usize,
                None => continue,
            };

            // Check each subtable.
            for si in 0..sub_count {
                let sub_off = match read_u16_be(lookup, 6 + si * 2) {
                    Some(v) => v as usize,
                    None => continue,
                };

                if lookup_off + sub_off >= ll.len() {
                    continue;
                }

                let sub = &ll[lookup_off + sub_off..];
                let result = self.gpos_pairpos_lookup(sub, left_glyph, right_glyph);

                if result != 0 {
                    return result;
                }
            }
        }

        0
    }
    /// Rasterize a glyph into the provided coverage buffer.
    ///
    /// `size_px` is the font size in pixels (height of the em square).
    /// The buffer receives a coverage map (0–255 per pixel). Returns metrics
    /// describing the bitmap dimensions and positioning, or `None` if the
    /// glyph cannot be rasterized (missing, empty, or exceeds buffer).
    pub fn rasterize(
        &self,
        codepoint: char,
        size_px: u32,
        buffer: &mut RasterBuffer,
        scratch: &mut RasterScratch,
    ) -> Option<GlyphMetrics> {
        let glyph_index = self.glyph_index(codepoint)?;
        let (advance_fu, lsb_fu) = self.glyph_h_metrics(glyph_index)?;

        if !self.glyph_outline(glyph_index, &mut scratch.outline) {
            // Empty glyph (e.g., space). Return metrics with zero bitmap.
            let advance = scale_fu(advance_fu as i32, size_px, self.units_per_em) as u32;

            return Some(GlyphMetrics {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                advance,
            });
        }

        // Read bounding box values before passing scratch mutably.
        let upem = self.units_per_em;
        let x_min_fu = scratch.outline.x_min;
        let y_min_fu = scratch.outline.y_min;
        let x_max_fu = scratch.outline.x_max;
        let y_max_fu = scratch.outline.y_max;
        // Scale bounding box to pixels.
        let x_min_px = scale_fu_floor(x_min_fu as i32, size_px, upem);
        let y_min_px = scale_fu_floor(y_min_fu as i32, size_px, upem);
        let x_max_px = scale_fu_ceil(x_max_fu as i32, size_px, upem);
        let y_max_px = scale_fu_ceil(y_max_fu as i32, size_px, upem);
        let _ = y_min_px; // used for height calc via y_max - y_min
        let bmp_w = (x_max_px - x_min_px) as u32;
        let bmp_h = (y_max_px - y_min_px) as u32;

        if bmp_w == 0 || bmp_h == 0 {
            let advance = scale_fu(advance_fu as i32, size_px, self.units_per_em) as u32;

            return Some(GlyphMetrics {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                advance,
            });
        }

        if bmp_w > buffer.width || bmp_h > buffer.height {
            return None; // Exceeds caller's buffer.
        }

        // Subpixel rendering: rasterize at OVERSAMPLE_X × width (6× = 3
        // sub-pixels × 2× oversampling each), then downsample into 3 channels
        // (R, G, B) for LCD subpixel coverage. Vertical oversampling
        // (OVERSAMPLE_Y) is already handled by sub-scanlines in
        // rasterize_segments.
        //
        // OVERSAMPLE_X must be 6 for subpixel rendering (3 sub-pixels × 2).
        // Each output pixel gets 3 bytes: R from columns [0..1], G from
        // columns [2..3], B from columns [4..5] of the oversampled row.
        let over_w = bmp_w * OVERSAMPLE_X as u32;
        let over_total = (over_w * bmp_h) as usize;
        // Output is 3 bytes per pixel (RGB coverage).
        let out_total = (bmp_w * bmp_h * 3) as usize;

        if over_total > buffer.data.len() {
            return None;
        }

        // Clear the oversampled coverage region.
        for b in buffer.data[..over_total].iter_mut() {
            *b = 0;
        }

        // Flatten outline into line segments (at 1× pixel coordinates).
        scratch.num_segments = 0;

        flatten_outline_from_scratch(scratch, size_px, upem, x_min_px, y_max_px);

        // Scale segment x-coordinates by OVERSAMPLE_X for wider rasterization.
        for i in 0..scratch.num_segments {
            scratch.segments[i].x0 *= OVERSAMPLE_X;
            scratch.segments[i].x1 *= OVERSAMPLE_X;
        }

        // Rasterize at oversampled width.
        rasterize_segments(scratch, &mut buffer.data[..over_total], over_w, bmp_h);

        // Downsample into 3-channel (RGB) subpixel coverage.
        //
        // For each output pixel, the 6 oversampled columns map to:
        //   R = average of columns [0, 1] (sub-pixel 0)
        //   G = average of columns [2, 3] (sub-pixel 1)
        //   B = average of columns [4, 5] (sub-pixel 2)
        //
        // We write the 3-channel output into the beginning of the same buffer.
        // Safe because out_total (W*H*3) <= over_total (W*H*6) always, and
        // we process left-to-right, top-to-bottom so dst never overtakes src.
        let samples_per_channel = (OVERSAMPLE_X / 3) as u32; // 2

        for row in 0..bmp_h {
            for col in 0..bmp_w {
                let src_base = (row * over_w + col * OVERSAMPLE_X as u32) as usize;
                let dst_base = (row * bmp_w * 3 + col * 3) as usize;
                // R channel: average of first 2 oversampled columns.
                let mut sum_r = 0u32;

                for s in 0..samples_per_channel {
                    sum_r += buffer.data[src_base + s as usize] as u32;
                }

                // G channel: average of middle 2 oversampled columns.
                let mut sum_g = 0u32;

                for s in 0..samples_per_channel {
                    sum_g +=
                        buffer.data[src_base + samples_per_channel as usize + s as usize] as u32;
                }

                // B channel: average of last 2 oversampled columns.
                let mut sum_b = 0u32;

                for s in 0..samples_per_channel {
                    sum_b += buffer.data[src_base + 2 * samples_per_channel as usize + s as usize]
                        as u32;
                }

                buffer.data[dst_base] = (sum_r / samples_per_channel) as u8;
                buffer.data[dst_base + 1] = (sum_g / samples_per_channel) as u8;
                buffer.data[dst_base + 2] = (sum_b / samples_per_channel) as u8;
            }
        }

        // Apply 3-tap low-pass FIR filter [1/4, 1/2, 1/4] to reduce color
        // fringing. The filter runs across the 3*W subpixel samples in each
        // row, treating all R, G, B values as a flat stream of sub-pixels.
        //
        // The filter smooths sharp transitions between channels, reducing the
        // visible colored edges at glyph boundaries while preserving overall
        // sharpness.
        //
        // We need a temporary row buffer for the filter (max 48*3 = 144 bytes).
        {
            let stride3 = (bmp_w * 3) as usize;
            // Temporary buffer for one row of subpixel data.
            let mut tmp = [0u8; GLYPH_MAX_W * 3];

            for row in 0..bmp_h {
                let row_start = (row * bmp_w * 3) as usize;

                // Copy current row to temporary buffer.
                for i in 0..stride3 {
                    tmp[i] = buffer.data[row_start + i];
                }

                // Apply FIR filter: out[i] = tmp[i-1]/4 + tmp[i]/2 + tmp[i+1]/4
                // Boundary: clamp to edge (repeat edge value).
                for i in 0..stride3 {
                    let prev = if i > 0 {
                        tmp[i - 1] as u32
                    } else {
                        tmp[0] as u32
                    };
                    let curr = tmp[i] as u32;
                    let next = if i + 1 < stride3 {
                        tmp[i + 1] as u32
                    } else {
                        tmp[stride3 - 1] as u32
                    };
                    // [1/4, 1/2, 1/4] = (prev + 2*curr + next) / 4
                    let filtered = (prev + 2 * curr + next + 2) / 4;

                    buffer.data[row_start + i] = if filtered > 255 { 255 } else { filtered as u8 };
                }
            }
        }

        // Apply stem darkening: boost coverage values using the pre-computed
        // lookup table. This makes thin strokes heavier and more legible.
        // Applied equally to all 3 subpixel channels (R, G, B).
        {
            for i in 0..out_total {
                buffer.data[i] = STEM_DARKENING_LUT[buffer.data[i] as usize];
            }
        }

        let advance = scale_fu(advance_fu as i32, size_px, self.units_per_em) as u32;
        let bearing_x = scale_fu(lsb_fu as i32, size_px, upem);
        let bearing_y = y_max_px;

        Some(GlyphMetrics {
            width: bmp_w,
            height: bmp_h,
            bearing_x,
            bearing_y,
            advance,
        })
    }
    /// Returns units-per-em for external use (e.g., computing line height).
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
}

// ---------------------------------------------------------------------------
// GPOS helpers (outside impl — pure functions)
// ---------------------------------------------------------------------------

/// Look up a glyph's class in a ClassDef table. Returns 0 (class 0) if
/// the glyph is not covered by any range.
fn classdef_lookup(data: &[u8], glyph: u16) -> u16 {
    if data.len() < 4 {
        return 0;
    }

    let fmt = match read_u16_be(data, 0) {
        Some(v) => v,
        None => return 0,
    };

    match fmt {
        1 => {
            // Format 1: array indexed by glyph ID.
            let start_glyph = match read_u16_be(data, 2) {
                Some(v) => v,
                None => return 0,
            };
            let count = match read_u16_be(data, 4) {
                Some(v) => v as usize,
                None => return 0,
            };

            if glyph < start_glyph {
                return 0;
            }

            let idx = (glyph - start_glyph) as usize;

            if idx >= count {
                return 0;
            }

            read_u16_be(data, 6 + idx * 2).unwrap_or(0)
        }
        2 => {
            // Format 2: ranges (startGlyphID, endGlyphID, class).
            let range_count = match read_u16_be(data, 2) {
                Some(v) => v as usize,
                None => return 0,
            };

            // Binary search for the range containing glyph.
            let mut lo = 0usize;
            let mut hi = range_count;

            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let roff = 4 + mid * 6;
                let start = match read_u16_be(data, roff) {
                    Some(v) => v,
                    None => return 0,
                };
                let end = match read_u16_be(data, roff + 2) {
                    Some(v) => v,
                    None => return 0,
                };

                if glyph > end {
                    lo = mid + 1;
                } else if glyph < start {
                    hi = mid;
                } else {
                    return read_u16_be(data, roff + 4).unwrap_or(0);
                }
            }

            0
        }
        _ => 0,
    }
}
/// Extract the XAdvance value from a ValueRecord at the given offset.
/// ValueFormat bit 0x0004 indicates XAdvance is present. The position of
/// XAdvance within the record depends on how many lower bits are set:
/// - bit 0 (XPlacement): if set, skip 2 bytes
/// - bit 1 (YPlacement): if set, skip 2 bytes
/// - bit 2 (XAdvance): the value we want
fn extract_x_advance(data: &[u8], offset: usize, val_format: u16) -> i16 {
    if val_format & 0x0004 == 0 {
        return 0; // No XAdvance in this format.
    }

    // Count how many values precede XAdvance (bits 0 and 1).
    let mut skip = 0usize;

    if val_format & 0x0001 != 0 {
        skip += 2; // XPlacement
    }
    if val_format & 0x0002 != 0 {
        skip += 2; // YPlacement
    }

    read_i16_be(data, offset + skip).unwrap_or(0)
}
/// Count the number of i16 values in a GPOS ValueRecord based on ValueFormat.
/// Each set bit in the format means one i16 (2 bytes) in the record.
fn value_format_size(fmt: u16) -> usize {
    let mut count = 0usize;
    let mut bits = fmt;

    while bits != 0 {
        count += (bits & 1) as usize;
        bits >>= 1;
    }

    count * 2 // each value is 2 bytes (i16)
}

// ---------------------------------------------------------------------------
// Coordinate scaling helpers (integer only)
// ---------------------------------------------------------------------------

fn min_usize(a: usize, b: usize) -> usize {
    if a < b {
        a
    } else {
        b
    }
}
/// Scale a value in font units to pixels: `val * size_px / units_per_em`.
fn scale_fu(val: i32, size_px: u32, upem: u16) -> i32 {
    ((val as i64 * size_px as i64) / upem as i64) as i32
}
/// Scale and round toward positive infinity (ceil).
fn scale_fu_ceil(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;

    if n > 0 {
        ((n + d - 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
/// Scale and round toward negative infinity (floor).
fn scale_fu_floor(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;

    if n < 0 {
        ((n - d + 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
